# Metadata Write Support — Implementation Plan

**Goal:** Add metadata write support (`metadata::modify()`) to ia-core and `ia metadata --modify` CLI, enabling the `ia ai` command prerequisite.

**Architecture:** Diff-based approach matching Python `internetarchive` library. Fetch current metadata, apply desired changes to a copy, compute RFC 6902 JSON Patch via `json-patch` crate, POST patch to IA metadata API with S3 auth in request body. All tests mocked with wiremock.

**Tech Stack:** `json-patch` (RFC 6902 diff), `calamine` (XLSX/ODS read), `csv` (CSV/TSV), existing `serde_json`, `reqwest`, `wiremock`

**Design Doc:** `docs/plans/2026-02-22-metadata-write-design.md`

---

## Task 1: Add Dependencies

**Files:**
- Modify: `ia-core/Cargo.toml`

**Step 1: Add new crate dependencies**

Add to `[dependencies]` in `ia-core/Cargo.toml`:

```toml
json-patch = "3"
calamine = "0.26"
csv = "1"
```

**Step 2: Verify build**

Run: `cargo check --workspace`
Expected: Compiles successfully with new deps

**Step 3: Commit**

```bash
git add ia-core/Cargo.toml Cargo.lock
git commit -m "chore: add json-patch, calamine, csv dependencies"
```

---

## Task 2: Add Error Variants

**Files:**
- Modify: `ia-core/src/error.rs`
- Test: `ia-core/src/error.rs` (inline tests)

**Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `ia-core/src/error.rs`:

```rust
#[test]
fn auth_error_displays_message() {
    let err = IaError::Auth("S3 credentials required".to_string());
    assert_eq!(err.to_string(), "authentication required: S3 credentials required");
}

#[test]
fn metadata_write_error_displays_details() {
    let err = IaError::MetadataWrite {
        identifier: "nasa".to_string(),
        message: "no changes to xml".to_string(),
    };
    assert!(err.to_string().contains("nasa"));
    assert!(err.to_string().contains("no changes to xml"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- error::tests::auth_error error::tests::metadata_write`
Expected: FAIL — variants don't exist yet

**Step 3: Add error variants**

Add to the `IaError` enum in `ia-core/src/error.rs`, after the `Config` variant:

```rust
#[error("authentication required: {0}")]
Auth(String),

#[error("metadata write failed for {identifier}: {message}")]
MetadataWrite { identifier: String, message: String },
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- error::tests`
Expected: All pass

**Step 5: Commit**

```bash
git add ia-core/src/error.rs
git commit -m "feat: add Auth and MetadataWrite error variants"
```

---

## Task 3: Add `require_auth()` to IaClient

**Files:**
- Modify: `ia-core/src/client.rs`
- Test: `ia-core/src/client.rs` (inline tests)

**Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `ia-core/src/client.rs`:

```rust
#[test]
fn require_auth_returns_credentials_when_present() {
    let mut config = IaConfig::default();
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    let client = IaClient::from_config(config).unwrap();
    let (access, secret) = client.require_auth().unwrap();
    assert_eq!(access, "test_access");
    assert_eq!(secret, "test_secret");
}

#[test]
fn require_auth_errors_when_no_credentials() {
    let client = IaClient::from_config(IaConfig::default()).unwrap();
    let result = client.require_auth();
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), crate::error::IaError::Auth(_)));
}

#[test]
fn require_auth_errors_when_partial_credentials() {
    let mut config = IaConfig::default();
    config.s3_access = Some("access_only".to_string());
    // s3_secret is None
    let client = IaClient::from_config(config).unwrap();
    assert!(client.require_auth().is_err());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- client::tests::require_auth`
Expected: FAIL — method doesn't exist

**Step 3: Implement require_auth()**

Add to the `impl IaClient` block in `ia-core/src/client.rs`:

```rust
/// Get S3 credentials, or error if not configured.
/// Called on first write attempt — read operations stay unauthenticated.
pub fn require_auth(&self) -> Result<(&str, &str)> {
    match (&self.config.s3_access, &self.config.s3_secret) {
        (Some(a), Some(s)) => Ok((a.as_str(), s.as_str())),
        _ => Err(crate::error::IaError::Auth(
            "S3 credentials required. Run `ia configure` or set \
             IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY environment variables."
                .into(),
        )),
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- client::tests::require_auth`
Expected: All 3 pass

**Step 5: Commit**

```bash
git add ia-core/src/client.rs
git commit -m "feat: add require_auth() to IaClient for write operations"
```

---

## Task 4: Add Core Write Types

**Files:**
- Create: `ia-core/src/metadata/mod.rs`
- Create: `ia-core/src/metadata/read.rs`
- Create: `ia-core/src/metadata/write.rs`
- Delete: `ia-core/src/metadata.rs` (replaced by module)
- Modify: `ia-core/src/lib.rs` (module path unchanged — `pub mod metadata` stays)

This task splits `metadata.rs` into a module directory and adds the new write types.

**Step 1: Move existing read code**

Move the contents of `ia-core/src/metadata.rs` into `ia-core/src/metadata/read.rs` (exact same code, no changes).

Create `ia-core/src/metadata/mod.rs`:

```rust
mod read;
pub mod write;

pub use read::{exists, get};
```

**Step 2: Create write types in `ia-core/src/metadata/write.rs`**

```rust
use serde::{Deserialize, Serialize};

/// How to apply metadata changes.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataOp {
    /// Set field to value (replace if exists, add if new).
    /// To delete a field, set its value to "REMOVE_TAG".
    Set,
    /// Append string to existing string field (space-separated).
    /// Errors if target field is a list.
    Append,
    /// Append value to a list field.
    /// Converts scalar to list if needed.
    AppendList,
    /// Insert value at a specific index in a list field.
    /// Deduplicates: removes existing occurrence before inserting.
    Insert(usize),
    /// Remove a specific value from a field.
    /// Deletes the field entirely if removing the last value.
    Remove,
}

/// Response from the IA metadata write API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModifyResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The REMOVE_TAG sentinel value. Setting a field to this string
/// signals that the field should be deleted entirely.
pub const REMOVE_TAG: &str = "REMOVE_TAG";

/// Fields that cannot be modified by users.
pub const IMMUTABLE_FIELDS: &[&str] = &[
    "identifier",
    "addeddate",
    "publicdate",
    "uploader",
];

/// Fields that only IA admins can modify (warn but don't block).
pub const ADMIN_ONLY_FIELDS: &[&str] = &["mediatype"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_op_set_is_default_like() {
        let op = MetadataOp::Set;
        assert_eq!(op, MetadataOp::Set);
    }

    #[test]
    fn metadata_op_insert_carries_index() {
        let op = MetadataOp::Insert(3);
        assert!(matches!(op, MetadataOp::Insert(3)));
    }

    #[test]
    fn modify_response_deserializes_success() {
        let json = r#"{"success":true,"task_id":12345,"log":"https://catalogd.archive.org/log/12345"}"#;
        let resp: ModifyResponse = serde_json::from_str(json).unwrap();
        assert!(resp.success);
        assert_eq!(resp.task_id, Some(12345));
        assert!(resp.log.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn modify_response_deserializes_error() {
        let json = r#"{"success":false,"error":"no changes to xml"}"#;
        let resp: ModifyResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("no changes to xml"));
        assert!(resp.task_id.is_none());
    }

    #[test]
    fn remove_tag_is_correct_sentinel() {
        assert_eq!(REMOVE_TAG, "REMOVE_TAG");
    }

    #[test]
    fn immutable_fields_includes_identifier() {
        assert!(IMMUTABLE_FIELDS.contains(&"identifier"));
        assert!(IMMUTABLE_FIELDS.contains(&"addeddate"));
    }

    #[test]
    fn admin_only_fields_includes_mediatype() {
        assert!(ADMIN_ONLY_FIELDS.contains(&"mediatype"));
    }
}
```

**Step 3: Verify existing tests still pass**

Run: `cargo test -p ia-core -- metadata`
Expected: All existing metadata read tests pass, plus new write type tests

**Step 4: Verify workspace builds**

Run: `cargo check --workspace`
Expected: Compiles — module re-exports maintain backward compat

**Step 5: Commit**

```bash
git add ia-core/src/metadata/ ia-core/src/lib.rs
git rm ia-core/src/metadata.rs 2>/dev/null || true
git commit -m "refactor: split metadata into module, add write types"
```

---

## Task 5: Key:Value Parsing Utility

**Files:**
- Modify: `ia-core/src/metadata/write.rs`

Parses CLI-style `"field:value"` and `"field[index]:value"` strings into structured data.

**Step 1: Write failing tests**

Add to `ia-core/src/metadata/write.rs` tests module:

```rust
#[test]
fn parse_key_value_simple() {
    let (key, value) = parse_key_value("title:New Title").unwrap();
    assert_eq!(key, "title");
    assert_eq!(value, "New Title");
}

#[test]
fn parse_key_value_with_colons_in_value() {
    let (key, value) = parse_key_value("description:Time: 10:30 AM").unwrap();
    assert_eq!(key, "description");
    assert_eq!(value, "Time: 10:30 AM");
}

#[test]
fn parse_key_value_empty_value() {
    let (key, value) = parse_key_value("title:").unwrap();
    assert_eq!(key, "title");
    assert_eq!(value, "");
}

#[test]
fn parse_key_value_no_colon_errors() {
    assert!(parse_key_value("no_colon_here").is_err());
}

#[test]
fn parse_key_value_indexed() {
    let (key, value) = parse_key_value("collection[0]:featured").unwrap();
    assert_eq!(key, "collection[0]");
    assert_eq!(value, "featured");
}

#[test]
fn parse_indexed_key_extracts_field_and_index() {
    let (field, index) = parse_indexed_key("collection[0]").unwrap();
    assert_eq!(field, "collection");
    assert_eq!(index, 0);
}

#[test]
fn parse_indexed_key_larger_index() {
    let (field, index) = parse_indexed_key("subject[42]").unwrap();
    assert_eq!(field, "subject");
    assert_eq!(index, 42);
}

#[test]
fn parse_indexed_key_no_brackets_returns_none() {
    assert!(parse_indexed_key("title").is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- metadata::write::tests::parse`
Expected: FAIL — functions don't exist

**Step 3: Implement parsing functions**

Add to `ia-core/src/metadata/write.rs` (above the tests module):

```rust
use crate::error::{IaError, Result};

/// Parse a "key:value" string into (key, value).
/// The first colon is the separator — value may contain additional colons.
pub fn parse_key_value(s: &str) -> Result<(String, String)> {
    let Some(pos) = s.find(':') else {
        return Err(IaError::Config(format!(
            "invalid key:value format (missing colon): {s:?}"
        )));
    };
    let key = s[..pos].to_string();
    let value = s[pos + 1..].to_string();
    Ok((key, value))
}

/// Parse an indexed key like "collection[0]" into ("collection", 0).
/// Returns None if the key doesn't contain brackets.
pub fn parse_indexed_key(key: &str) -> Option<(String, usize)> {
    let bracket_start = key.find('[')?;
    let bracket_end = key.find(']')?;
    if bracket_end <= bracket_start + 1 {
        return None;
    }
    let field = key[..bracket_start].to_string();
    let index: usize = key[bracket_start + 1..bracket_end].parse().ok()?;
    Some((field, index))
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- metadata::write::tests::parse`
Expected: All pass

**Step 5: Commit**

```bash
git add ia-core/src/metadata/write.rs
git commit -m "feat: add key:value and indexed key parsing for metadata write"
```

---

## Task 6: Metadata Preparation — Apply Changes to Copy

**Files:**
- Modify: `ia-core/src/metadata/write.rs`

This is the core logic: take current metadata as `serde_json::Value`, apply desired changes based on `MetadataOp`, return the modified copy. The caller then diffs source vs destination.

**Step 1: Write failing tests**

Add to the tests module in `ia-core/src/metadata/write.rs`:

```rust
use serde_json::json;

// --- Set operation ---

#[test]
fn prepare_set_replaces_existing_field() {
    let source = json!({"title": "Old Title", "date": "2020"});
    let changes = vec![("title".to_string(), json!("New Title"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set).unwrap();
    assert_eq!(dest["title"], json!("New Title"));
    assert_eq!(dest["date"], json!("2020")); // unchanged
}

#[test]
fn prepare_set_adds_new_field() {
    let source = json!({"title": "Existing"});
    let changes = vec![("date".to_string(), json!("2024-01-01"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set).unwrap();
    assert_eq!(dest["date"], json!("2024-01-01"));
    assert_eq!(dest["title"], json!("Existing"));
}

#[test]
fn prepare_set_remove_tag_deletes_field() {
    let source = json!({"title": "Keep", "bad_field": "remove me"});
    let changes = vec![("bad_field".to_string(), json!("REMOVE_TAG"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set).unwrap();
    assert!(dest.get("bad_field").is_none());
    assert_eq!(dest["title"], json!("Keep"));
}

#[test]
fn prepare_set_multiple_fields() {
    let source = json!({"title": "Old", "date": "2020"});
    let changes = vec![
        ("title".to_string(), json!("New")),
        ("date".to_string(), json!("2024")),
    ];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set).unwrap();
    assert_eq!(dest["title"], json!("New"));
    assert_eq!(dest["date"], json!("2024"));
}

// --- Append operation ---

#[test]
fn prepare_append_to_existing_string() {
    let source = json!({"description": "Original text"});
    let changes = vec![("description".to_string(), json!("and more"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append).unwrap();
    assert_eq!(dest["description"], json!("Original text and more"));
}

#[test]
fn prepare_append_to_missing_field_sets_it() {
    let source = json!({"title": "Test"});
    let changes = vec![("description".to_string(), json!("New desc"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append).unwrap();
    assert_eq!(dest["description"], json!("New desc"));
}

#[test]
fn prepare_append_to_array_field_errors() {
    let source = json!({"subject": ["math", "science"]});
    let changes = vec![("subject".to_string(), json!("physics"))];
    let result = prepare_metadata(&source, &changes, &MetadataOp::Append);
    assert!(result.is_err());
}

// --- AppendList operation ---

#[test]
fn prepare_append_list_to_existing_array() {
    let source = json!({"subject": ["math", "science"]});
    let changes = vec![("subject".to_string(), json!("physics"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList).unwrap();
    assert_eq!(dest["subject"], json!(["math", "science", "physics"]));
}

#[test]
fn prepare_append_list_to_string_converts() {
    let source = json!({"subject": "math"});
    let changes = vec![("subject".to_string(), json!("physics"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList).unwrap();
    assert_eq!(dest["subject"], json!(["math", "physics"]));
}

#[test]
fn prepare_append_list_to_missing_creates_list() {
    let source = json!({"title": "Test"});
    let changes = vec![("subject".to_string(), json!("physics"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList).unwrap();
    assert_eq!(dest["subject"], json!(["physics"]));
}

#[test]
fn prepare_append_list_allows_duplicates() {
    let source = json!({"subject": ["math"]});
    let changes = vec![("subject".to_string(), json!("math"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList).unwrap();
    assert_eq!(dest["subject"], json!(["math", "math"]));
}

// --- Insert operation ---

#[test]
fn prepare_insert_at_beginning() {
    let source = json!({"collection": ["existing"]});
    let changes = vec![("collection".to_string(), json!("featured"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0)).unwrap();
    assert_eq!(dest["collection"], json!(["featured", "existing"]));
}

#[test]
fn prepare_insert_deduplicates() {
    let source = json!({"collection": ["a", "featured", "b"]});
    let changes = vec![("collection".to_string(), json!("featured"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0)).unwrap();
    assert_eq!(dest["collection"], json!(["featured", "a", "b"]));
}

#[test]
fn prepare_insert_into_string_converts() {
    let source = json!({"collection": "existing"});
    let changes = vec![("collection".to_string(), json!("new"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0)).unwrap();
    assert_eq!(dest["collection"], json!(["new", "existing"]));
}

// --- Remove operation ---

#[test]
fn prepare_remove_from_array() {
    let source = json!({"subject": ["math", "science", "physics"]});
    let changes = vec![("subject".to_string(), json!("science"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove).unwrap();
    assert_eq!(dest["subject"], json!(["math", "physics"]));
}

#[test]
fn prepare_remove_last_from_array_deletes_field() {
    let source = json!({"subject": ["only_one"]});
    let changes = vec![("subject".to_string(), json!("only_one"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove).unwrap();
    assert!(dest.get("subject").is_none());
}

#[test]
fn prepare_remove_scalar_match_deletes_field() {
    let source = json!({"notes": "remove me"});
    let changes = vec![("notes".to_string(), json!("remove me"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove).unwrap();
    assert!(dest.get("notes").is_none());
}

#[test]
fn prepare_remove_scalar_no_match_is_noop() {
    let source = json!({"notes": "keep me"});
    let changes = vec![("notes".to_string(), json!("something else"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove).unwrap();
    assert_eq!(dest["notes"], json!("keep me"));
}

#[test]
fn prepare_remove_from_semicolon_subject() {
    let source = json!({"subject": "math;science;physics"});
    let changes = vec![("subject".to_string(), json!("science"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove).unwrap();
    assert_eq!(dest["subject"], json!("math;physics"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- metadata::write::tests::prepare`
Expected: FAIL — `prepare_metadata` doesn't exist

**Step 3: Implement `prepare_metadata()`**

Add to `ia-core/src/metadata/write.rs`:

```rust
use serde_json::Value;
use std::collections::HashMap;

/// Apply metadata changes to a copy of the source metadata.
///
/// This is the core preparation step: take the current metadata state,
/// apply the desired changes according to the operation mode, and return
/// the modified copy. The caller then diffs source vs destination to
/// compute the JSON Patch.
pub fn prepare_metadata(
    source: &Value,
    changes: &[(String, Value)],
    op: &MetadataOp,
) -> Result<Value> {
    let mut dest = source.clone();
    let obj = dest
        .as_object_mut()
        .ok_or_else(|| IaError::Config("metadata must be a JSON object".into()))?;

    for (key, value) in changes {
        // Check for indexed key (e.g., "subject[0]")
        if let Some((field, _idx)) = parse_indexed_key(key) {
            // For REMOVE_TAG on indexed keys, remove the element at that index
            if op == &MetadataOp::Set && value.as_str() == Some(REMOVE_TAG) {
                if let Some(arr) = obj.get_mut(&field).and_then(|v| v.as_array_mut()) {
                    if _idx < arr.len() {
                        arr.remove(_idx);
                        if arr.is_empty() {
                            obj.remove(&field);
                        }
                    }
                }
                continue;
            }
        }

        match op {
            MetadataOp::Set => {
                if value.as_str() == Some(REMOVE_TAG) {
                    obj.remove(key);
                } else {
                    obj.insert(key.clone(), value.clone());
                }
            }
            MetadataOp::Append => {
                let current = obj.get(key);
                match current {
                    Some(Value::Array(_)) => {
                        return Err(IaError::MetadataWrite {
                            identifier: String::new(),
                            message: format!(
                                "cannot append to list field {key:?} — use --append-list instead"
                            ),
                        });
                    }
                    Some(Value::String(existing)) => {
                        let new_val = format!(
                            "{} {}",
                            existing,
                            value.as_str().unwrap_or(&value.to_string())
                        );
                        obj.insert(key.clone(), Value::String(new_val));
                    }
                    Some(_) | None => {
                        // Missing or non-string: just set the value
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
            MetadataOp::AppendList => {
                let current = obj.get(key).cloned();
                match current {
                    Some(Value::Array(mut arr)) => {
                        arr.push(value.clone());
                        obj.insert(key.clone(), Value::Array(arr));
                    }
                    Some(Value::String(s)) => {
                        // Convert scalar to list, then append
                        obj.insert(
                            key.clone(),
                            Value::Array(vec![Value::String(s), value.clone()]),
                        );
                    }
                    _ => {
                        // Missing: create single-element list
                        obj.insert(key.clone(), Value::Array(vec![value.clone()]));
                    }
                }
            }
            MetadataOp::Insert(index) => {
                let current = obj.get(key).cloned();
                let mut arr = match current {
                    Some(Value::Array(a)) => a,
                    Some(Value::String(s)) => vec![Value::String(s)],
                    _ => vec![],
                };
                // Deduplicate: remove existing occurrence
                arr.retain(|v| v != value);
                // Insert at index (clamped to bounds)
                let idx = (*index).min(arr.len());
                arr.insert(idx, value.clone());
                obj.insert(key.clone(), Value::Array(arr));
            }
            MetadataOp::Remove => {
                let current = obj.get(key).cloned();
                match current {
                    Some(Value::Array(arr)) => {
                        let filtered: Vec<Value> =
                            arr.into_iter().filter(|v| v != value).collect();
                        if filtered.is_empty() {
                            obj.remove(key);
                        } else {
                            obj.insert(key.clone(), Value::Array(filtered));
                        }
                    }
                    Some(Value::String(s)) => {
                        let value_str = value.as_str().unwrap_or("");
                        // Handle semicolon-delimited subjects
                        if key == "subject" && s.contains(';') {
                            let parts: Vec<&str> = s
                                .split(';')
                                .filter(|p| p.trim() != value_str)
                                .collect();
                            if parts.is_empty() {
                                obj.remove(key);
                            } else {
                                obj.insert(
                                    key.clone(),
                                    Value::String(parts.join(";")),
                                );
                            }
                        } else if s == value_str {
                            obj.remove(key);
                        }
                        // If no match, leave unchanged (no-op)
                    }
                    _ => {
                        // Field doesn't exist — nothing to remove
                    }
                }
            }
        }
    }

    Ok(dest)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- metadata::write::tests::prepare`
Expected: All pass

**Step 5: Commit**

```bash
git add ia-core/src/metadata/write.rs
git commit -m "feat: implement prepare_metadata for all MetadataOp modes"
```

---

## Task 7: Patch Computation and Expect Test Ops

**Files:**
- Modify: `ia-core/src/metadata/write.rs`

Compute RFC 6902 JSON Patch from source vs destination, and prepend `test` operations from `--expect`.

**Step 1: Write failing tests**

Add to the tests module:

```rust
#[test]
fn compute_patch_for_set_field() {
    let source = json!({"title": "Old"});
    let changes = vec![("title".to_string(), json!("New"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None).unwrap();
    assert_eq!(patch.len(), 1);
    let op = &patch[0];
    assert_eq!(op["op"], "replace");
    assert_eq!(op["path"], "/title");
    assert_eq!(op["value"], "New");
}

#[test]
fn compute_patch_for_add_field() {
    let source = json!({"title": "Existing"});
    let changes = vec![("date".to_string(), json!("2024"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None).unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0]["op"], "add");
    assert_eq!(patch[0]["path"], "/date");
}

#[test]
fn compute_patch_for_remove_tag() {
    let source = json!({"title": "Keep", "bad": "remove"});
    let changes = vec![("bad".to_string(), json!("REMOVE_TAG"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None).unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0]["op"], "remove");
    assert_eq!(patch[0]["path"], "/bad");
}

#[test]
fn compute_patch_no_changes_returns_empty() {
    let source = json!({"title": "Same"});
    let changes = vec![("title".to_string(), json!("Same"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None).unwrap();
    assert!(patch.is_empty());
}

#[test]
fn compute_patch_with_expect_prepends_test_ops() {
    let source = json!({"title": "Old"});
    let changes = vec![("title".to_string(), json!("New"))];
    let expect = HashMap::from([("title".to_string(), json!("Old"))]);
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, Some(&expect)).unwrap();
    assert!(patch.len() >= 2);
    assert_eq!(patch[0]["op"], "test");
    assert_eq!(patch[0]["path"], "/title");
    assert_eq!(patch[0]["value"], "Old");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- metadata::write::tests::compute_patch`
Expected: FAIL — function doesn't exist

**Step 3: Implement `compute_patch()`**

Add to `ia-core/src/metadata/write.rs`:

```rust
/// Compute a JSON Patch (RFC 6902) from desired metadata changes.
///
/// 1. Applies changes to a copy of source via `prepare_metadata()`
/// 2. Diffs source vs destination using `json_patch::diff()`
/// 3. Prepends `test` operations from `expect` for optimistic concurrency
///
/// Returns the patch as a Vec of serde_json::Value operations.
pub fn compute_patch(
    source: &Value,
    changes: &[(String, Value)],
    op: &MetadataOp,
    expect: Option<&HashMap<String, Value>>,
) -> Result<Vec<Value>> {
    let destination = prepare_metadata(source, changes, op)?;
    let patch = json_patch::diff(source, &destination);

    // Convert patch to Vec<Value> for serialization
    let patch_value = serde_json::to_value(&patch)
        .map_err(|e| IaError::Config(format!("failed to serialize patch: {e}")))?;
    let mut ops: Vec<Value> = match patch_value {
        Value::Array(arr) => arr,
        _ => vec![],
    };

    // Prepend test operations from --expect
    if let Some(expect_map) = expect {
        let mut test_ops = Vec::new();
        for (key, value) in expect_map {
            if let Some((field, idx)) = parse_indexed_key(key) {
                test_ops.push(json!({
                    "op": "test",
                    "path": format!("/{field}/{idx}"),
                    "value": value,
                }));
            } else {
                test_ops.push(json!({
                    "op": "test",
                    "path": format!("/{key}"),
                    "value": value,
                }));
            }
        }
        test_ops.append(&mut ops);
        ops = test_ops;
    }

    Ok(ops)
}
```

Add the import at the top of `write.rs`:

```rust
use json_patch;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- metadata::write::tests::compute_patch`
Expected: All pass

**Step 5: Commit**

```bash
git add ia-core/src/metadata/write.rs
git commit -m "feat: implement JSON Patch computation with expect test ops"
```

---

## Task 8: The `modify()` Function — HTTP POST

**Files:**
- Modify: `ia-core/src/metadata/write.rs`
- Modify: `ia-core/src/metadata/mod.rs` (re-export)

This is the main public function that ties everything together: fetch current metadata, compute patch, POST to IA API.

**Step 1: Write failing tests**

Add to the tests module:

```rust
use wiremock::matchers::{method, path, body_string_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_config(server_uri: &str) -> crate::config::IaConfig {
    let mut config = crate::config::IaConfig::default();
    let host = server_uri
        .strip_prefix("http://")
        .or_else(|| server_uri.strip_prefix("https://"))
        .unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.secure = false;
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    config
}

fn mock_item_metadata() -> Value {
    json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Old Title",
            "mediatype": "texts",
            "subject": ["math", "science"],
            "collection": ["opensource"],
            "description": "A test item"
        },
        "files": [
            {"name": "test.pdf", "size": "1000", "source": "original", "md5": "abc123"}
        ],
        "server": "ia000000.us.archive.org"
    })
}

#[tokio::test]
async fn modify_set_sends_correct_patch() {
    let mock_server = MockServer::start().await;

    // Mock GET metadata
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
        .mount(&mock_server)
        .await;

    // Mock POST metadata - verify it receives the patch
    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .and(body_string_contains("-target=metadata"))
        .and(body_string_contains("access=test_access"))
        .and(body_string_contains("secret=test_secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "task_id": 12345,
            "log": "https://catalogd.archive.org/log/12345"
        })))
        .mount(&mock_server)
        .await;

    let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New Title"))];
    let resp = modify(
        &client,
        "test-item",
        &changes,
        &MetadataOp::Set,
        "metadata",
        None,
        None,
        false,
    )
    .await
    .unwrap();

    assert!(resp.success);
    assert_eq!(resp.task_id, Some(12345));
}

#[tokio::test]
async fn modify_errors_without_auth() {
    let config = crate::config::IaConfig::default(); // no credentials
    let client = crate::client::IaClient::from_config(config).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let result = modify(
        &client,
        "test-item",
        &changes,
        &MetadataOp::Set,
        "metadata",
        None,
        None,
        false,
    )
    .await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
}

#[tokio::test]
async fn modify_no_changes_returns_error() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
        .mount(&mock_server)
        .await;

    let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    // Set title to its current value — no changes
    let changes = vec![("title".to_string(), json!("Old Title"))];
    let result = modify(
        &client,
        "test-item",
        &changes,
        &MetadataOp::Set,
        "metadata",
        None,
        None,
        false,
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        IaError::MetadataWrite { identifier, message } => {
            assert_eq!(identifier, "test-item");
            assert!(message.contains("no changes"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn modify_file_target() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .and(body_string_contains("-target=files%2Ftest.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true, "task_id": 99999
        })))
        .mount(&mock_server)
        .await;

    let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let changes = vec![("custom_tag".to_string(), json!("hello"))];
    let resp = modify(
        &client,
        "test-item",
        &changes,
        &MetadataOp::Set,
        "files/test.pdf",
        None,
        None,
        false,
    )
    .await
    .unwrap();

    assert!(resp.success);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- metadata::write::tests::modify`
Expected: FAIL — `modify` function doesn't exist

**Step 3: Implement `modify()`**

Add to `ia-core/src/metadata/write.rs`:

```rust
use crate::client::IaClient;

/// Modify metadata on an Internet Archive item.
///
/// 1. Validates auth credentials
/// 2. Fetches current metadata via GET /metadata/{identifier}
/// 3. Extracts the target metadata (item-level or file-level)
/// 4. Applies changes and computes RFC 6902 JSON Patch
/// 5. POSTs the patch to /metadata/{identifier}
///
/// Returns `ModifyResponse` with task_id on success.
pub async fn modify(
    client: &IaClient,
    identifier: &str,
    changes: &[(String, Value)],
    op: &MetadataOp,
    target: &str,
    expect: Option<&HashMap<String, Value>>,
    priority: Option<i32>,
    reduced_priority: bool,
) -> Result<ModifyResponse> {
    // 1. Validate auth
    let (access, secret) = client.require_auth()?;

    // 2. Fetch current metadata
    let item: Value = {
        let url = client.url(&format!("/metadata/{identifier}"));
        let response = client.http().get(&url).send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(IaError::NotFound(identifier.to_string()));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(IaError::Http {
                status: status.as_u16(),
                message: body,
            });
        }
        response
            .json()
            .await
            .map_err(reqwest_middleware::Error::from)?
    };

    // 3. Extract source metadata based on target
    let source = extract_target_metadata(&item, target)?;

    // 4. Compute patch
    let patch_ops = compute_patch(&source, changes, op, expect)?;
    if patch_ops.is_empty() {
        return Err(IaError::MetadataWrite {
            identifier: identifier.to_string(),
            message: "no changes computed (values already match current metadata)".into(),
        });
    }

    // 5. POST the patch
    let patch_json = serde_json::to_string(&patch_ops)
        .map_err(|e| IaError::Config(format!("failed to serialize patch: {e}")))?;

    let priority_val = priority.unwrap_or(0);
    let body = format!(
        "-target={}&-patch={}&priority={}&access={}&secret={}",
        urlencoding::encode(target),
        urlencoding::encode(&patch_json),
        priority_val,
        urlencoding::encode(access),
        urlencoding::encode(secret),
    );

    let url = client.url(&format!("/metadata/{identifier}"));
    let mut request = client
        .http()
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body);

    if reduced_priority {
        request = request.header("X-Accept-Reduced-Priority", "1");
    }

    let response = request.send().await?;
    let status = response.status();

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Err(IaError::RateLimited {
            retry_after: retry_after.unwrap_or(30),
        });
    }

    let resp: ModifyResponse = response
        .json()
        .await
        .map_err(reqwest_middleware::Error::from)?;

    if !resp.success {
        return Err(IaError::MetadataWrite {
            identifier: identifier.to_string(),
            message: resp.error.clone().unwrap_or_else(|| "unknown error".into()),
        });
    }

    Ok(resp)
}

/// Extract the metadata for the specified target from the full item metadata.
///
/// - "metadata" → item["metadata"] as object
/// - "files/foo.txt" → the file entry matching "foo.txt"
fn extract_target_metadata(item: &Value, target: &str) -> Result<Value> {
    if target == "metadata" {
        return item
            .get("metadata")
            .cloned()
            .ok_or_else(|| IaError::Config("item has no metadata field".into()));
    }

    if let Some(filename) = target.strip_prefix("files/") {
        let files = item
            .get("files")
            .and_then(|f| f.as_array())
            .ok_or_else(|| IaError::Config("item has no files array".into()))?;

        for file in files {
            if file.get("name").and_then(|n| n.as_str()) == Some(filename) {
                return Ok(file.clone());
            }
        }

        return Err(IaError::MetadataWrite {
            identifier: String::new(),
            message: format!("file not found in item: {filename}"),
        });
    }

    // Other targets (e.g., user JSON files)
    item.get(target)
        .cloned()
        .ok_or_else(|| IaError::Config(format!("unknown target: {target}")))
}
```

Update `ia-core/src/metadata/mod.rs` to re-export:

```rust
mod read;
pub mod write;

pub use read::{exists, get};
pub use write::{modify, compute_patch, parse_key_value, parse_indexed_key, prepare_metadata};
pub use write::{MetadataOp, ModifyResponse, REMOVE_TAG, IMMUTABLE_FIELDS, ADMIN_ONLY_FIELDS};
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- metadata::write::tests::modify`
Expected: All pass

**Step 5: Run all tests**

Run: `cargo test -p ia-core`
Expected: All existing + new tests pass

**Step 6: Commit**

```bash
git add ia-core/src/metadata/
git commit -m "feat: implement metadata::modify() with auth, patch computation, and HTTP POST"
```

---

## Task 9: Comprehensive Edge Case Tests

**Files:**
- Create: `ia-core/tests/metadata_write.rs` (integration test with full mock fixtures)

This task builds the extensive test suite with all the fake metadata fixtures from the design doc.

**Step 1: Create the test file with all fixtures and edge case tests**

Create `ia-core/tests/metadata_write.rs` with comprehensive tests covering:

1. **All 16+ mock fixtures** (clean_item, minimal_item, string_arrays_item, mega_collections_item, unicode_item, html_description_item, date_chaos_item, duplicate_values_item, numeric_strings_item, empty_strings_item, null_fields_item, extra_fields_item, file_level_meta_item, dark_item, semicolon_subjects_item, description_array_item, scan_metadata_item)

2. **Set tests:** Replace existing, add new, REMOVE_TAG delete, multiple fields, overwrite list with scalar, overwrite scalar with list, unicode values, empty string values

3. **Append tests:** To string, to missing field, to array (error), to empty string

4. **AppendList tests:** To array, to string (convert), to missing (create), duplicates allowed

5. **Insert tests:** At 0 (prepend), at end, deduplication, into string, out-of-bounds index

6. **Remove tests:** From array, last from array (delete), scalar match (delete), no match (noop), semicolon subjects, from collection (can't remove last)

7. **Expect tests:** Match (succeed), no match (mock 400), missing field

8. **Target tests:** Item metadata, file metadata, file not found, multi-target

9. **Edge cases:** Zero-change patch, 429 response, 401/403, 500, huge metadata (100+ fields), indexed REMOVE_TAG

This file will be large (500+ lines). The implementer should use the fixtures and test cases listed in the design doc section "Testing Strategy" as the reference, and create thorough wiremock-based integration tests.

**Key pattern for wiremock integration tests:**

```rust
use ia_core::{IaClient, IaConfig};
use ia_core::metadata::write::*;
use serde_json::json;
use wiremock::matchers::{method, path, body_string_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_config_with_auth(server_uri: &str) -> IaConfig {
    let mut config = IaConfig::default();
    let host = server_uri.strip_prefix("http://").unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.secure = false;
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    config
}

// Each fixture is a function returning serde_json::Value
fn clean_item() -> serde_json::Value {
    json!({
        "metadata": {
            "identifier": "clean-item",
            "title": "A Well-Formed Item",
            "description": "This item has all fields populated correctly.",
            "mediatype": "texts",
            "collection": ["opensource", "community"],
            "creator": "Test Author",
            "date": "2024-01-15",
            "subject": ["testing", "metadata", "quality"],
            "language": "eng",
            "publicdate": "2024-01-15 10:30:00",
            "addeddate": "2024-01-15 10:30:00",
            "uploader": "test@example.com"
        },
        "files": [
            {"name": "document.pdf", "size": "5000000", "source": "original", "md5": "abc123", "format": "Text PDF"},
            {"name": "document_djvu.txt", "size": "120000", "source": "derivative", "original": "document.pdf", "format": "DjVuTXT"}
        ],
        "server": "ia802304.us.archive.org"
    })
}

// ... other fixtures following the same pattern
```

**Step 2: Run all tests**

Run: `cargo test -p ia-core -- metadata_write`
Expected: All tests pass

**Step 3: Commit**

```bash
git add ia-core/tests/metadata_write.rs
git commit -m "test: add comprehensive metadata write edge case tests with mock fixtures"
```

---

## Task 10: Extend CLI MetadataArgs with Write Flags

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Write failing CLI integration test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_modify_flag_in_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--modify"))
        .stdout(predicate::str::contains("--append"))
        .stdout(predicate::str::contains("--append-list"))
        .stdout(predicate::str::contains("--insert"))
        .stdout(predicate::str::contains("--remove"))
        .stdout(predicate::str::contains("--target"))
        .stdout(predicate::str::contains("--expect"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--spreadsheet"))
        .stdout(predicate::str::contains("--priority"))
        .stdout(predicate::str::contains("--reduced-priority"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli -- metadata_modify_flag`
Expected: FAIL — flags don't exist

**Step 3: Extend MetadataArgs**

Replace the `MetadataArgs` struct in `ia-cli/src/commands/metadata.rs`:

```rust
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct MetadataArgs {
    /// Item identifier(s)
    #[arg()]
    pub identifiers: Vec<String>,

    /// Check if item exists (exit code 0=yes, 1=no)
    #[arg(long)]
    pub exists: bool,

    /// List available file formats
    #[arg(long)]
    pub formats: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,

    // --- Write flags (mutually exclusive) ---

    /// Set field to value: --modify="field:value" (repeatable)
    #[arg(short = 'm', long = "modify", value_name = "K:V")]
    pub modify: Vec<String>,

    /// Append to string field: --append="field:value" (repeatable)
    #[arg(short = 'a', long = "append", value_name = "K:V",
          conflicts_with_all = ["modify", "append_list", "insert", "remove"])]
    pub append: Vec<String>,

    /// Append to list field: --append-list="field:value" (repeatable)
    #[arg(short = 'A', long = "append-list", value_name = "K:V",
          conflicts_with_all = ["modify", "append", "insert", "remove"])]
    pub append_list: Vec<String>,

    /// Insert at index: --insert="field[N]:value" (repeatable)
    #[arg(short = 'I', long = "insert", value_name = "K[N]:V",
          conflicts_with_all = ["modify", "append", "append_list", "remove"])]
    pub insert: Vec<String>,

    /// Remove value: --remove="field:value" (repeatable)
    #[arg(short = 'r', long = "remove", value_name = "K:V",
          conflicts_with_all = ["modify", "append", "append_list", "insert"])]
    pub remove: Vec<String>,

    // --- Write options ---

    /// Target: "metadata" (default) or "files/filename"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Server-side concurrency check: --expect="field:expected_value" (repeatable)
    #[arg(long, value_name = "K:V")]
    pub expect: Vec<String>,

    /// Task priority (default: 0 single, -5 batch)
    #[arg(long)]
    pub priority: Option<i32>,

    /// Accept reduced priority to reduce rate limiting
    #[arg(long)]
    pub reduced_priority: bool,

    /// Show changes without writing
    #[arg(long)]
    pub dry_run: bool,

    // --- Bulk input ---

    /// Read identifiers from file (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long)]
    pub search: Option<String>,

    /// Bulk from file (CSV, TSV, XLSX, ODS, JSONL)
    #[arg(short = 's', long)]
    pub spreadsheet: Option<PathBuf>,
}
```

**Note:** The `identifier` field changes from a single `String` to `Vec<String>` named `identifiers`. The existing `run()` function will need updating in Task 11.

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-cli -- metadata_modify_flag`
Expected: PASS

**Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: add metadata write CLI flags (modify/append/append-list/insert/remove)"
```

---

## Task 11: CLI Write Flow — Single Item

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

Implement the `run()` function for write operations (single item first). Read mode stays the same, write mode calls `ia_core::metadata::modify()`.

**Step 1: Implement the updated run() function**

Rewrite the `run()` function in `ia-cli/src/commands/metadata.rs` to handle both read and write modes:

```rust
use anyhow::{Context, Result};
use ia_core::IaClient;
use ia_core::metadata::write::{
    MetadataOp, ModifyResponse, IMMUTABLE_FIELDS, ADMIN_ONLY_FIELDS,
    parse_key_value, parse_indexed_key,
};
use serde_json::json;
use std::collections::HashMap;

pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<std::path::PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    // Determine if this is a write operation
    let is_write = !args.modify.is_empty()
        || !args.append.is_empty()
        || !args.append_list.is_empty()
        || !args.insert.is_empty()
        || !args.remove.is_empty()
        || args.spreadsheet.is_some();

    if !is_write {
        // --- Read mode (existing behavior) ---
        let identifier = args.identifiers.first()
            .context("identifier required")?;

        if args.exists {
            let exists = client.item_exists(identifier).await?;
            if !exists {
                std::process::exit(1);
            }
            return Ok(());
        }

        let item = client.get_item(identifier).await?;

        if args.formats {
            let mut formats: Vec<&str> = item
                .files
                .iter()
                .filter_map(|f| f.format.as_deref())
                .collect();
            formats.sort();
            formats.dedup();
            for fmt in formats {
                println!("{fmt}");
            }
            return Ok(());
        }

        if args.pretty {
            println!("{}", serde_json::to_string_pretty(&item)?);
        } else {
            println!("{}", serde_json::to_string(&item)?);
        }
        return Ok(());
    }

    // --- Write mode ---

    // Parse the changes and determine op
    let (changes_raw, op) = if !args.modify.is_empty() {
        (args.modify.clone(), MetadataOp::Set)
    } else if !args.append.is_empty() {
        (args.append.clone(), MetadataOp::Append)
    } else if !args.append_list.is_empty() {
        (args.append_list.clone(), MetadataOp::AppendList)
    } else if !args.insert.is_empty() {
        // Parse insert index from the first key
        let first = &args.insert[0];
        let (key, _) = parse_key_value(first).context("invalid key:value format")?;
        let index = parse_indexed_key(&key)
            .map(|(_, idx)| idx)
            .unwrap_or(0);
        (args.insert.clone(), MetadataOp::Insert(index))
    } else if !args.remove.is_empty() {
        (args.remove.clone(), MetadataOp::Remove)
    } else {
        // spreadsheet-only — will be handled in Task 13
        anyhow::bail!("spreadsheet mode not yet implemented");
    };

    // Parse key:value pairs
    let changes: Vec<(String, serde_json::Value)> = changes_raw
        .iter()
        .map(|s| {
            let (key, value) = parse_key_value(s)
                .context(format!("invalid key:value format: {s:?}"))?;
            Ok((key, json!(value)))
        })
        .collect::<Result<Vec<_>>>()?;

    // Warn about immutable/admin-only fields
    for (key, _) in &changes {
        let field = parse_indexed_key(key)
            .map(|(f, _)| f)
            .unwrap_or_else(|| key.clone());
        if IMMUTABLE_FIELDS.contains(&field.as_str()) {
            eprintln!("warning: field {field:?} cannot be modified (immutable)");
        }
        if ADMIN_ONLY_FIELDS.contains(&field.as_str()) {
            eprintln!("warning: field {field:?} typically requires IA admin access");
        }
    }

    // Parse expect
    let expect: Option<HashMap<String, serde_json::Value>> = if !args.expect.is_empty() {
        let mut map = HashMap::new();
        for s in &args.expect {
            let (key, value) = parse_key_value(s)
                .context(format!("invalid expect key:value: {s:?}"))?;
            map.insert(key, json!(value));
        }
        Some(map)
    } else {
        None
    };

    // Collect identifiers
    let identifiers = collect_identifiers(&args, client).await?;
    if identifiers.is_empty() {
        anyhow::bail!("no identifiers provided");
    }

    // Joblog
    let joblog = joblog_path
        .as_ref()
        .map(|p| ia_core::joblog::JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let priority = args.priority.unwrap_or(if identifiers.len() > 1 { -5 } else { 0 });

    // Process each identifier
    for identifier in &identifiers {
        if args.dry_run {
            // Dry-run: fetch metadata, compute patch, display
            let item: serde_json::Value = {
                let url = client.url(&format!("/metadata/{identifier}"));
                let resp = client.http().get(&url).send().await?;
                resp.json().await.map_err(|e| anyhow::anyhow!("{e}"))?
            };

            let source = if args.target == "metadata" {
                item.get("metadata").cloned().unwrap_or(json!({}))
            } else {
                json!({})
            };

            let patch = ia_core::metadata::compute_patch(
                &source,
                &changes,
                &op,
                expect.as_ref(),
            )?;

            if quiet == 0 {
                if patch.is_empty() {
                    println!("  {identifier}: no changes");
                } else {
                    println!("  {identifier}:");
                    for p in &patch {
                        let op_type = p["op"].as_str().unwrap_or("?");
                        let path = p["path"].as_str().unwrap_or("?");
                        if op_type == "test" {
                            continue; // skip expect test ops in display
                        }
                        if let Some(value) = p.get("value") {
                            println!("    {path}: {op_type} -> {value}");
                        } else {
                            println!("    {path}: {op_type}");
                        }
                    }
                }
            }
            continue;
        }

        // Actual write
        let start = std::time::Instant::now();
        let result = ia_core::metadata::modify(
            client,
            identifier,
            &changes,
            &op,
            &args.target,
            expect.as_ref(),
            Some(priority),
            args.reduced_priority,
        )
        .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        match &result {
            Ok(resp) => {
                if quiet == 0 {
                    println!("{identifier}: success (task_id: {})", resp.task_id.unwrap_or(0));
                }
                if let Some(ref jl) = joblog {
                    let mut entry = ia_core::joblog::JoblogEntry::new(
                        "modify", identifier,
                        if args.target == "metadata" { "" } else { &args.target },
                    );
                    entry.status = "ok".to_string();
                    entry.elapsed_ms = Some(elapsed_ms);
                    jl.write(&entry);
                }
            }
            Err(e) => {
                if quiet < 2 {
                    eprintln!("error: {identifier}: {e}");
                }
                if let Some(ref jl) = joblog {
                    let entry = ia_core::joblog::JoblogEntry::new(
                        "modify", identifier,
                        if args.target == "metadata" { "" } else { &args.target },
                    )
                    .error(&e.to_string(), 0);
                    jl.write(&entry);
                }
            }
        }
    }

    Ok(())
}

/// Collect identifiers from all sources (positional, --itemlist, --search, stdin).
async fn collect_identifiers(args: &MetadataArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = args.identifiers.clone();

    if let Some(ref path) = args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    if let Some(ref query) = args.search {
        use futures::StreamExt;
        let opts = ia_core::search::SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    if ids.is_empty() && args.itemlist.is_none() && args.search.is_none() {
        if atty::isnt(atty::Stream::Stdin) {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let line = line.context("failed to read from stdin")?;
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    ids.push(trimmed);
                }
            }
        }
    }

    Ok(ids)
}
```

**Step 2: Update main.rs to pass new parameters to metadata run()**

The `Commands::Metadata` arm in `main.rs` needs to pass the additional parameters (quiet, jobs, joblog_path, retry_failed) that metadata now needs. Check the current dispatch and update it.

**Step 3: Verify build**

Run: `cargo check --workspace`
Expected: Compiles

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass (existing + new)

**Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/src/main.rs
git commit -m "feat: implement metadata write CLI flow with single-item and batch support"
```

---

## Task 12: Spreadsheet Reader

**Files:**
- Create: `ia-core/src/spreadsheet.rs`
- Modify: `ia-core/src/lib.rs` (add module)

A reader that parses CSV, TSV, XLSX, ODS, and JSONL files into a uniform stream of `(identifier, HashMap<String, String>)` records.

**Step 1: Write failing tests**

Create tests in `ia-core/src/spreadsheet.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_csv() {
        let csv = "identifier,title,date\nnasa,NASA Images,2024-01-01\nmars,Mars Rover,2024-06-01\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "nasa");
        assert_eq!(records[0].1.get("title").unwrap(), "NASA Images");
        assert_eq!(records[1].0, "mars");
    }

    #[test]
    fn read_csv_with_bom() {
        let csv = "\u{FEFF}identifier,title\nnasa,NASA\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "nasa");
    }

    #[test]
    fn read_csv_skips_empty_identifier() {
        let csv = "identifier,title\nnasa,NASA\n,Empty\n\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn read_csv_skips_empty_values() {
        let csv = "identifier,title,date\nnasa,NASA,\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert!(records[0].1.get("date").is_none()); // empty → skipped
    }

    #[test]
    fn read_tsv() {
        let tsv = "identifier\ttitle\nnasa\tNASA Images\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tsv");
        std::fs::write(&path, tsv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.get("title").unwrap(), "NASA Images");
    }

    #[test]
    fn read_jsonl() {
        let jsonl = r#"{"identifier":"nasa","title":"NASA","date":"2024"}
{"identifier":"mars","title":"Mars"}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, jsonl).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "nasa");
        assert_eq!(records[0].1.get("title").unwrap(), "NASA");
    }

    #[test]
    fn read_jsonl_skips_empty_lines() {
        let jsonl = "{\"identifier\":\"nasa\",\"title\":\"NASA\"}\n\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, jsonl).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn unknown_extension_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xyz");
        std::fs::write(&path, "data").unwrap();
        assert!(read_spreadsheet(&path).is_err());
    }
}
```

**Step 2: Implement the spreadsheet reader**

```rust
use crate::error::{IaError, Result};
use std::collections::HashMap;
use std::path::Path;

/// A single record from a spreadsheet: (identifier, field_name → value).
pub type SpreadsheetRecord = (String, HashMap<String, String>);

/// Read a spreadsheet file and return records.
/// Format auto-detected by file extension: .csv, .tsv, .xlsx, .ods, .jsonl
pub fn read_spreadsheet(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => read_csv(path, b','),
        "tsv" => read_csv(path, b'\t'),
        "xlsx" | "ods" | "xls" => read_calamine(path),
        "jsonl" | "ndjson" => read_jsonl(path),
        other => Err(IaError::Config(format!(
            "unsupported spreadsheet format: .{other} (supported: .csv, .tsv, .xlsx, .ods, .jsonl)"
        ))),
    }
}

fn read_csv(path: &Path, delimiter: u8) -> Result<Vec<SpreadsheetRecord>> {
    let data = std::fs::read(path)?;
    // Strip BOM if present
    let data = if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &data[3..]
    } else {
        &data
    };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(data);

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| IaError::Config(format!("failed to read CSV headers: {e}")))?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| IaError::Config("CSV must have an 'identifier' column".into()))?;

    let mut records = Vec::new();
    for result in reader.records() {
        let row = result.map_err(|e| IaError::Config(format!("CSV parse error: {e}")))?;
        let identifier = row.get(id_col).unwrap_or("").trim().to_string();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (i, value) in row.iter().enumerate() {
            if i == id_col || i >= headers.len() {
                continue;
            }
            let value = value.trim();
            if !value.is_empty() {
                fields.insert(headers[i].clone(), value.to_string());
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}

fn read_calamine(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    use calamine::{open_workbook_auto, DataType, Reader};

    let mut workbook = open_workbook_auto(path)
        .map_err(|e| IaError::Config(format!("failed to open spreadsheet: {e}")))?;

    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| IaError::Config("spreadsheet has no sheets".into()))?;

    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| IaError::Config(format!("failed to read sheet: {e}")))?;

    let mut rows = range.rows();

    // First row = headers
    let header_row = rows
        .next()
        .ok_or_else(|| IaError::Config("spreadsheet is empty".into()))?;
    let headers: Vec<String> = header_row
        .iter()
        .map(|cell| cell.to_string().trim().to_lowercase())
        .collect();

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| IaError::Config("spreadsheet must have an 'identifier' column".into()))?;

    let mut records = Vec::new();
    for row in rows {
        let identifier = row
            .get(id_col)
            .map(|c| c.to_string().trim().to_string())
            .unwrap_or_default();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (i, cell) in row.iter().enumerate() {
            if i == id_col || i >= headers.len() {
                continue;
            }
            let value = cell.to_string().trim().to_string();
            if !value.is_empty() {
                fields.insert(headers[i].clone(), value);
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}

fn read_jsonl(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    let content = std::fs::read_to_string(path)?;
    let mut records = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let obj: HashMap<String, serde_json::Value> = serde_json::from_str(line)
            .map_err(|e| IaError::Config(format!("JSONL parse error: {e}")))?;

        let identifier = obj
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (key, value) in &obj {
            if key == "identifier" {
                continue;
            }
            let s = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !s.is_empty() {
                fields.insert(key.to_lowercase(), s);
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}
```

Add `pub mod spreadsheet;` to `ia-core/src/lib.rs`.

**Step 3: Run tests**

Run: `cargo test -p ia-core -- spreadsheet::tests`
Expected: All pass

**Step 4: Commit**

```bash
git add ia-core/src/spreadsheet.rs ia-core/src/lib.rs
git commit -m "feat: add spreadsheet reader (CSV, TSV, XLSX, ODS, JSONL)"
```

---

## Task 13: Wire Spreadsheet to CLI

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

Add spreadsheet mode to the CLI write flow: read the spreadsheet, iterate over records, call `modify()` for each.

**Step 1: Add spreadsheet handling to the run() function**

In the write mode section of `ia-cli/src/commands/metadata.rs`, before the `changes_raw`/`op` parsing, add:

```rust
if let Some(ref spreadsheet_path) = args.spreadsheet {
    let records = ia_core::spreadsheet::read_spreadsheet(spreadsheet_path)
        .context(format!("failed to read spreadsheet: {}", spreadsheet_path.display()))?;

    let op = if !args.append.is_empty() {
        MetadataOp::Append
    } else if !args.append_list.is_empty() {
        MetadataOp::AppendList
    } else {
        MetadataOp::Set // default for spreadsheet
    };

    let priority = args.priority.unwrap_or(-5); // batch default

    if args.dry_run && quiet == 0 {
        println!("Dry run -- no changes will be applied\n");
    }

    for (identifier, fields) in &records {
        let changes: Vec<(String, serde_json::Value)> = fields
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();

        if changes.is_empty() {
            continue;
        }

        if args.dry_run {
            if quiet == 0 {
                println!("  {identifier}:");
                for (k, v) in &changes {
                    println!("    {k}: {v}");
                }
            }
            continue;
        }

        let start = std::time::Instant::now();
        let result = ia_core::metadata::modify(
            client,
            identifier,
            &changes,
            &op,
            &args.target,
            None, // no expect for spreadsheet
            Some(priority),
            args.reduced_priority,
        )
        .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        match &result {
            Ok(resp) => {
                if quiet == 0 {
                    println!("{identifier}: success (task_id: {})", resp.task_id.unwrap_or(0));
                }
                if let Some(ref jl) = joblog {
                    let mut entry = ia_core::joblog::JoblogEntry::new("modify", identifier, "");
                    entry.status = "ok".to_string();
                    entry.elapsed_ms = Some(elapsed_ms);
                    jl.write(&entry);
                }
            }
            Err(e) => {
                if quiet < 2 {
                    eprintln!("error: {identifier}: {e}");
                }
                if let Some(ref jl) = joblog {
                    jl.write(&ia_core::joblog::JoblogEntry::new("modify", identifier, "")
                        .error(&e.to_string(), 0));
                }
            }
        }
    }

    return Ok(());
}
```

**Step 2: Verify build**

Run: `cargo check --workspace`

**Step 3: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 4: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "feat: wire spreadsheet reader to metadata write CLI"
```

---

## Task 14: Rate Limiting with Global Pause

**Files:**
- Create: `ia-core/src/rate_limit.rs`
- Modify: `ia-core/src/lib.rs`

Implements a shared rate limiter that pauses all concurrent tasks when a 429 is received.

**Step 1: Write the rate limiter**

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Shared rate limiter for metadata write operations.
///
/// When a 429 is received, all concurrent tasks pause until
/// the wait period expires. Uses AtomicBool for pause state
/// and Notify for wake-up.
#[derive(Clone)]
pub struct RateLimiter {
    paused: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Check if we're rate-limited. If so, wait until resumed.
    pub async fn wait_if_paused(&self) {
        while self.paused.load(Ordering::Relaxed) {
            self.notify.notified().await;
        }
    }

    /// Signal that a 429 was received. Pauses all tasks,
    /// waits for the specified duration, then resumes.
    pub async fn pause_for(&self, seconds: u64, on_pause: impl FnOnce(u64)) {
        // Only one task should trigger the pause
        if self
            .paused
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            on_pause(seconds);
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            self.paused.store(false, Ordering::SeqCst);
            self.notify.notify_waiters();
        } else {
            // Another task already paused — just wait
            self.wait_if_paused().await;
        }
    }

    /// Whether we're currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_paused_by_default() {
        let rl = RateLimiter::new();
        assert!(!rl.is_paused());
        // wait_if_paused should return immediately
        rl.wait_if_paused().await;
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let rl = RateLimiter::new();
        let rl2 = rl.clone();

        let handle = tokio::spawn(async move {
            rl2.pause_for(1, |_| {}).await;
        });

        // Give it a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(rl.is_paused());

        handle.await.unwrap();
        assert!(!rl.is_paused());
    }
}
```

Add `pub mod rate_limit;` to `ia-core/src/lib.rs`.

**Step 2: Run tests**

Run: `cargo test -p ia-core -- rate_limit::tests`
Expected: All pass

**Step 3: Commit**

```bash
git add ia-core/src/rate_limit.rs ia-core/src/lib.rs
git commit -m "feat: add RateLimiter for global 429 pause across concurrent tasks"
```

---

## Task 15: CLI Integration Tests for Metadata Write

**Files:**
- Modify: `ia-cli/tests/cli.rs`

Add integration tests for the new CLI flags and help text.

**Step 1: Add tests**

```rust
#[test]
fn metadata_write_flags_exist() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--modify"))
        .stdout(predicate::str::contains("--append"))
        .stdout(predicate::str::contains("--append-list"))
        .stdout(predicate::str::contains("--insert"))
        .stdout(predicate::str::contains("--remove"))
        .stdout(predicate::str::contains("--target"))
        .stdout(predicate::str::contains("--expect"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--spreadsheet"))
        .stdout(predicate::str::contains("--priority"))
        .stdout(predicate::str::contains("--reduced-priority"));
}

#[test]
fn metadata_short_flags() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-m"))
        .stdout(predicate::str::contains("-a"))
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-I"))
        .stdout(predicate::str::contains("-r"));
}

#[test]
fn metadata_write_flags_conflict() {
    // --modify and --append should conflict
    ia().args(["metadata", "test", "--modify=title:X", "--append=title:Y"])
        .assert()
        .failure();
}

#[test]
fn metadata_no_identifier_errors() {
    ia().args(["metadata"])
        .assert()
        .failure();
}
```

**Step 2: Run tests**

Run: `cargo test -p ia-cli -- metadata`
Expected: All pass

**Step 3: Commit**

```bash
git add ia-cli/tests/cli.rs
git commit -m "test: add CLI integration tests for metadata write flags"
```

---

## Task 16: Update Documentation

**Files:**
- Modify: `CLAUDE.md` (note metadata write capability)
- Modify: `MEMORY.md` (update implementation status)

**Step 1: Update MEMORY.md**

Add to the Implementation Status section:

```markdown
- **Milestone 5 (Metadata Write): IN PROGRESS** — Issue #85
  - Core `metadata::modify()` with diff-based patch computation
  - CLI `--modify/--append/--append-list/--insert/--remove` flags
  - Spreadsheet reader (CSV, TSV, XLSX, ODS, JSONL)
  - Rate limiter for 429 global pause
  - All tests mocked with wiremock
```

Add to Key Modules section:

```markdown
- `metadata/write.rs` — metadata::modify() with MetadataOp enum, patch computation, HTTP POST
- `spreadsheet.rs` — Multi-format reader (CSV/TSV/XLSX/ODS/JSONL) for bulk metadata updates
- `rate_limit.rs` — Shared rate limiter for 429 global pause
```

**Step 2: Commit**

```bash
git add CLAUDE.md MEMORY.md
git commit -m "docs: update CLAUDE.md and MEMORY.md for metadata write feature"
```

---

## Task 17: Final Verification

**Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass (existing + new)

**Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

**Step 3: Run check**

Run: `cargo check --workspace`
Expected: Clean build

**Step 4: Review changes**

Run: `git log --oneline` to verify all commits are clean.

---

## Summary of Files Created/Modified

### New files:
- `ia-core/src/metadata/mod.rs` — Module root (re-exports read + write)
- `ia-core/src/metadata/read.rs` — Existing metadata read (moved from metadata.rs)
- `ia-core/src/metadata/write.rs` — MetadataOp, modify(), patch computation, key:value parsing
- `ia-core/src/spreadsheet.rs` — Multi-format spreadsheet reader
- `ia-core/src/rate_limit.rs` — Shared rate limiter
- `ia-core/tests/metadata_write.rs` — Comprehensive integration tests

### Modified files:
- `ia-core/Cargo.toml` — New deps: json-patch, calamine, csv
- `ia-core/src/lib.rs` — New modules: spreadsheet, rate_limit
- `ia-core/src/error.rs` — New variants: Auth, MetadataWrite
- `ia-core/src/client.rs` — require_auth()
- `ia-cli/src/commands/metadata.rs` — Extended MetadataArgs + write flow
- `ia-cli/src/main.rs` — Updated dispatch for metadata
- `ia-cli/tests/cli.rs` — New CLI integration tests

### Deleted files:
- `ia-core/src/metadata.rs` — Replaced by metadata/ module directory
