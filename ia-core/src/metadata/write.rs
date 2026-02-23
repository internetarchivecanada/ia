use crate::client::IaClient;
use crate::error::{IaError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How to apply metadata changes.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Apply metadata changes to a copy of the source metadata.
///
/// This is the core preparation step: take the current metadata state,
/// apply the desired changes according to the operation mode, and return
/// the modified copy. The caller then diffs source vs destination to
/// compute the JSON Patch.
pub fn prepare_metadata(
    source: &serde_json::Value,
    changes: &[(String, serde_json::Value)],
    op: &MetadataOp,
    identifier: &str,
) -> Result<serde_json::Value> {
    let mut dest = source.clone();
    let obj = dest
        .as_object_mut()
        .ok_or_else(|| IaError::Config("metadata must be a JSON object".into()))?;

    for (key, value) in changes {
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
                    Some(serde_json::Value::Array(_)) => {
                        return Err(IaError::Config(format!(
                            "cannot append to list field {key:?} — use --append-list instead"
                        )));
                    }
                    Some(serde_json::Value::String(existing)) => {
                        let new_val = format!(
                            "{} {}",
                            existing,
                            value.as_str().unwrap_or(&value.to_string())
                        );
                        obj.insert(key.clone(), serde_json::Value::String(new_val));
                    }
                    Some(_) | None => {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
            MetadataOp::AppendList => {
                let current = obj.get(key).cloned();
                match current {
                    Some(serde_json::Value::Array(mut arr)) => {
                        arr.push(value.clone());
                        obj.insert(key.clone(), serde_json::Value::Array(arr));
                    }
                    Some(serde_json::Value::String(s)) => {
                        obj.insert(
                            key.clone(),
                            serde_json::Value::Array(vec![
                                serde_json::Value::String(s),
                                value.clone(),
                            ]),
                        );
                    }
                    _ => {
                        obj.insert(
                            key.clone(),
                            serde_json::Value::Array(vec![value.clone()]),
                        );
                    }
                }
            }
            MetadataOp::Insert(index) => {
                let current = obj.get(key).cloned();
                let mut arr = match current {
                    Some(serde_json::Value::Array(a)) => a,
                    Some(serde_json::Value::String(s)) => {
                        vec![serde_json::Value::String(s)]
                    }
                    _ => vec![],
                };
                // Deduplicate: remove existing occurrence
                arr.retain(|v| v != value);
                // Insert at index (clamped to bounds)
                let idx = (*index).min(arr.len());
                arr.insert(idx, value.clone());
                obj.insert(key.clone(), serde_json::Value::Array(arr));
            }
            MetadataOp::Remove => {
                let current = obj.get(key).cloned();
                match current {
                    Some(serde_json::Value::Array(arr)) => {
                        let filtered: Vec<serde_json::Value> =
                            arr.into_iter().filter(|v| v != value).collect();
                        if filtered.is_empty() {
                            if key == "collection" {
                                return Err(IaError::MetadataWrite {
                                    identifier: identifier.to_string(),
                                    message: "cannot remove last collection".into(),
                                });
                            }
                            obj.remove(key);
                        } else {
                            obj.insert(key.clone(), serde_json::Value::Array(filtered));
                        }
                    }
                    Some(serde_json::Value::String(s)) => {
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
                                    serde_json::Value::String(parts.join(";")),
                                );
                            }
                        } else if s == value_str {
                            if key == "collection" {
                                return Err(IaError::MetadataWrite {
                                    identifier: identifier.to_string(),
                                    message: "cannot remove last collection".into(),
                                });
                            }
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

/// Compute a JSON Patch (RFC 6902) from desired metadata changes.
///
/// 1. Applies changes to a copy of source via `prepare_metadata()`
/// 2. Diffs source vs destination using `json_patch::diff()`
/// 3. Prepends `test` operations from `expect` for optimistic concurrency
///
/// Returns the patch as a Vec of serde_json::Value operations.
pub fn compute_patch(
    source: &serde_json::Value,
    changes: &[(String, serde_json::Value)],
    op: &MetadataOp,
    expect: Option<&HashMap<String, serde_json::Value>>,
    identifier: &str,
) -> Result<Vec<serde_json::Value>> {
    let destination = prepare_metadata(source, changes, op, identifier)?;
    let patch = json_patch::diff(source, &destination);

    // Convert patch to Vec<Value> for serialization
    let patch_value = serde_json::to_value(&patch)
        .map_err(|e| IaError::Config(format!("failed to serialize patch: {e}")))?;
    let mut ops: Vec<serde_json::Value> = match patch_value {
        serde_json::Value::Array(arr) => arr,
        _ => vec![],
    };

    // Prepend test operations from --expect
    if let Some(expect_map) = expect {
        let mut test_ops = Vec::new();
        for (key, value) in expect_map {
            if let Some((field, idx)) = parse_indexed_key(key) {
                test_ops.push(serde_json::json!({
                    "op": "test",
                    "path": format!("/{field}/{idx}"),
                    "value": value,
                }));
            } else {
                test_ops.push(serde_json::json!({
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

/// Request parameters for metadata modification.
#[derive(Debug, Clone)]
pub struct ModifyRequest {
    /// Item identifier on archive.org
    pub identifier: String,
    /// List of (field_name, value) pairs to apply
    pub changes: Vec<(String, serde_json::Value)>,
    /// How to apply the changes (Set, Append, AppendList, Insert, Remove)
    pub op: MetadataOp,
    /// Target: "metadata" (default) or "files/filename"
    pub target: String,
    /// Optimistic concurrency checks: field -> expected value
    pub expect: Option<HashMap<String, serde_json::Value>>,
    /// Task priority (default 0 for single, -5 for batch)
    pub priority: Option<i32>,
    /// Whether to send X-Accept-Reduced-Priority header
    pub reduced_priority: bool,
}

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
    req: &ModifyRequest,
) -> Result<ModifyResponse> {
    let identifier = &req.identifier;

    // 1. Validate auth
    let (access, secret) = client.require_auth()?;
    let access = access.to_string();
    let secret = secret.to_string();

    // 2. Fetch current metadata
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
    let item: serde_json::Value = response
        .json()
        .await
        .map_err(reqwest_middleware::Error::from)?;

    // 3. Extract source metadata based on target
    let source = extract_target_metadata(&item, &req.target, identifier)?;

    // 4. Compute patch
    let patch_ops = compute_patch(&source, &req.changes, &req.op, req.expect.as_ref(), identifier)?;
    if patch_ops.is_empty() {
        return Err(IaError::MetadataWrite {
            identifier: identifier.to_string(),
            message: "no changes computed (values already match current metadata)".into(),
        });
    }

    // 5. POST the patch
    let patch_json = serde_json::to_string(&patch_ops)
        .map_err(|e| IaError::Config(format!("failed to serialize patch: {e}")))?;

    let priority_val = req.priority.unwrap_or(0);
    let body = format!(
        "-target={}&-patch={}&priority={}&access={}&secret={}",
        urlencoding::encode(&req.target),
        urlencoding::encode(&patch_json),
        priority_val,
        urlencoding::encode(&access),
        urlencoding::encode(&secret),
    );

    let post_url = client.url(&format!("/metadata/{identifier}"));
    let mut request = client
        .http()
        .post(&post_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body);

    if req.reduced_priority {
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
pub fn extract_target_metadata(
    item: &serde_json::Value,
    target: &str,
    identifier: &str,
) -> Result<serde_json::Value> {
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
            identifier: identifier.to_string(),
            message: format!("file not found in item: {filename}"),
        });
    }

    // Other targets
    item.get(target)
        .cloned()
        .ok_or_else(|| IaError::Config(format!("unknown target: {target}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_key_value tests ---

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

    // --- parse_indexed_key tests ---

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

    // --- prepare_metadata: Set operation ---

    #[test]
    fn prepare_set_replaces_existing_field() {
        let source = serde_json::json!({"title": "Old Title", "date": "2020"});
        let changes = vec![("title".to_string(), serde_json::json!("New Title"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
        assert_eq!(dest["title"], serde_json::json!("New Title"));
        assert_eq!(dest["date"], serde_json::json!("2020")); // unchanged
    }

    #[test]
    fn prepare_set_adds_new_field() {
        let source = serde_json::json!({"title": "Existing"});
        let changes = vec![("date".to_string(), serde_json::json!("2024-01-01"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
        assert_eq!(dest["date"], serde_json::json!("2024-01-01"));
        assert_eq!(dest["title"], serde_json::json!("Existing"));
    }

    #[test]
    fn prepare_set_remove_tag_deletes_field() {
        let source = serde_json::json!({"title": "Keep", "bad_field": "remove me"});
        let changes = vec![("bad_field".to_string(), serde_json::json!("REMOVE_TAG"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
        assert!(dest.get("bad_field").is_none());
        assert_eq!(dest["title"], serde_json::json!("Keep"));
    }

    #[test]
    fn prepare_set_multiple_fields() {
        let source = serde_json::json!({"title": "Old", "date": "2020"});
        let changes = vec![
            ("title".to_string(), serde_json::json!("New")),
            ("date".to_string(), serde_json::json!("2024")),
        ];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
        assert_eq!(dest["title"], serde_json::json!("New"));
        assert_eq!(dest["date"], serde_json::json!("2024"));
    }

    // --- prepare_metadata: Append operation ---

    #[test]
    fn prepare_append_to_existing_string() {
        let source = serde_json::json!({"description": "Original text"});
        let changes = vec![("description".to_string(), serde_json::json!("and more"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
        assert_eq!(dest["description"], serde_json::json!("Original text and more"));
    }

    #[test]
    fn prepare_append_to_missing_field_sets_it() {
        let source = serde_json::json!({"title": "Test"});
        let changes = vec![("description".to_string(), serde_json::json!("New desc"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
        assert_eq!(dest["description"], serde_json::json!("New desc"));
    }

    #[test]
    fn prepare_append_to_number_replaces_it() {
        let source = serde_json::json!({"ppi": 300});
        let changes = vec![("ppi".to_string(), serde_json::json!("600"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
        // Non-string, non-array values are replaced (not concatenated)
        assert_eq!(dest["ppi"], serde_json::json!("600"));
    }

    #[test]
    fn prepare_append_to_array_field_errors() {
        let source = serde_json::json!({"subject": ["math", "science"]});
        let changes = vec![("subject".to_string(), serde_json::json!("physics"))];
        let result = prepare_metadata(&source, &changes, &MetadataOp::Append, "test");
        assert!(result.is_err());
    }

    // --- prepare_metadata: AppendList operation ---

    #[test]
    fn prepare_append_list_to_existing_array() {
        let source = serde_json::json!({"subject": ["math", "science"]});
        let changes = vec![("subject".to_string(), serde_json::json!("physics"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!(["math", "science", "physics"]));
    }

    #[test]
    fn prepare_append_list_to_string_converts() {
        let source = serde_json::json!({"subject": "math"});
        let changes = vec![("subject".to_string(), serde_json::json!("physics"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!(["math", "physics"]));
    }

    #[test]
    fn prepare_append_list_to_missing_creates_list() {
        let source = serde_json::json!({"title": "Test"});
        let changes = vec![("subject".to_string(), serde_json::json!("physics"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!(["physics"]));
    }

    #[test]
    fn prepare_append_list_allows_duplicates() {
        let source = serde_json::json!({"subject": ["math"]});
        let changes = vec![("subject".to_string(), serde_json::json!("math"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!(["math", "math"]));
    }

    // --- prepare_metadata: Insert operation ---

    #[test]
    fn prepare_insert_at_beginning() {
        let source = serde_json::json!({"collection": ["existing"]});
        let changes = vec![("collection".to_string(), serde_json::json!("featured"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
        assert_eq!(dest["collection"], serde_json::json!(["featured", "existing"]));
    }

    #[test]
    fn prepare_insert_deduplicates() {
        let source = serde_json::json!({"collection": ["a", "featured", "b"]});
        let changes = vec![("collection".to_string(), serde_json::json!("featured"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
        assert_eq!(dest["collection"], serde_json::json!(["featured", "a", "b"]));
    }

    #[test]
    fn prepare_insert_into_string_converts() {
        let source = serde_json::json!({"collection": "existing"});
        let changes = vec![("collection".to_string(), serde_json::json!("new"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
        assert_eq!(dest["collection"], serde_json::json!(["new", "existing"]));
    }

    // --- prepare_metadata: Remove operation ---

    #[test]
    fn prepare_remove_from_array() {
        let source = serde_json::json!({"subject": ["math", "science", "physics"]});
        let changes = vec![("subject".to_string(), serde_json::json!("science"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!(["math", "physics"]));
    }

    #[test]
    fn prepare_remove_last_from_array_deletes_field() {
        let source = serde_json::json!({"subject": ["only_one"]});
        let changes = vec![("subject".to_string(), serde_json::json!("only_one"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
        assert!(dest.get("subject").is_none());
    }

    #[test]
    fn prepare_remove_scalar_match_deletes_field() {
        let source = serde_json::json!({"notes": "remove me"});
        let changes = vec![("notes".to_string(), serde_json::json!("remove me"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
        assert!(dest.get("notes").is_none());
    }

    #[test]
    fn prepare_remove_scalar_no_match_is_noop() {
        let source = serde_json::json!({"notes": "keep me"});
        let changes = vec![("notes".to_string(), serde_json::json!("something else"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
        assert_eq!(dest["notes"], serde_json::json!("keep me"));
    }

    #[test]
    fn prepare_remove_from_semicolon_subject() {
        let source = serde_json::json!({"subject": "math;science;physics"});
        let changes = vec![("subject".to_string(), serde_json::json!("science"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
        assert_eq!(dest["subject"], serde_json::json!("math;physics"));
    }

    // --- prepare_metadata: collection last-removal enforcement ---

    #[test]
    fn prepare_remove_last_collection_string_errors() {
        let source = serde_json::json!({"collection": "only-collection", "title": "Test"});
        let changes = vec![("collection".to_string(), serde_json::json!("only-collection"))];
        let result = prepare_metadata(&source, &changes, &MetadataOp::Remove, "my-item");
        match result.unwrap_err() {
            IaError::MetadataWrite { identifier, message } => {
                assert_eq!(identifier, "my-item");
                assert!(message.contains("cannot remove last collection"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn prepare_remove_last_collection_array_errors() {
        let source = serde_json::json!({"collection": ["only-collection"], "title": "Test"});
        let changes = vec![("collection".to_string(), serde_json::json!("only-collection"))];
        let result = prepare_metadata(&source, &changes, &MetadataOp::Remove, "my-item");
        match result.unwrap_err() {
            IaError::MetadataWrite { identifier, message } => {
                assert_eq!(identifier, "my-item");
                assert!(message.contains("cannot remove last collection"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn prepare_remove_non_last_collection_succeeds() {
        let source = serde_json::json!({"collection": ["keep", "remove-me"], "title": "Test"});
        let changes = vec![("collection".to_string(), serde_json::json!("remove-me"))];
        let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "my-item").unwrap();
        assert_eq!(dest["collection"], serde_json::json!(["keep"]));
    }

    // --- compute_patch tests ---

    #[test]
    fn compute_patch_for_set_field() {
        let source = serde_json::json!({"title": "Old"});
        let changes = vec![("title".to_string(), serde_json::json!("New"))];
        let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
        assert_eq!(patch.len(), 1);
        let op = &patch[0];
        assert_eq!(op["op"], "replace");
        assert_eq!(op["path"], "/title");
        assert_eq!(op["value"], "New");
    }

    #[test]
    fn compute_patch_for_add_field() {
        let source = serde_json::json!({"title": "Existing"});
        let changes = vec![("date".to_string(), serde_json::json!("2024"))];
        let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0]["op"], "add");
        assert_eq!(patch[0]["path"], "/date");
    }

    #[test]
    fn compute_patch_for_remove_tag() {
        let source = serde_json::json!({"title": "Keep", "bad": "remove"});
        let changes = vec![("bad".to_string(), serde_json::json!("REMOVE_TAG"))];
        let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0]["op"], "remove");
        assert_eq!(patch[0]["path"], "/bad");
    }

    #[test]
    fn compute_patch_no_changes_returns_empty() {
        let source = serde_json::json!({"title": "Same"});
        let changes = vec![("title".to_string(), serde_json::json!("Same"))];
        let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
        assert!(patch.is_empty());
    }

    #[test]
    fn compute_patch_with_expect_prepends_test_ops() {
        use std::collections::HashMap;
        let source = serde_json::json!({"title": "Old"});
        let changes = vec![("title".to_string(), serde_json::json!("New"))];
        let expect = HashMap::from([("title".to_string(), serde_json::json!("Old"))]);
        let patch = compute_patch(&source, &changes, &MetadataOp::Set, Some(&expect), "test").unwrap();
        assert!(patch.len() >= 2);
        assert_eq!(patch[0]["op"], "test");
        assert_eq!(patch[0]["path"], "/title");
        assert_eq!(patch[0]["value"], "Old");
    }

    #[test]
    fn compute_patch_with_indexed_expect_key() {
        use std::collections::HashMap;
        let source = serde_json::json!({"collection": ["opensource", "community"]});
        let changes = vec![("collection".to_string(), serde_json::json!("featured"))];
        let expect = HashMap::from([("collection[0]".to_string(), serde_json::json!("opensource"))]);
        let patch = compute_patch(&source, &changes, &MetadataOp::AppendList, Some(&expect), "test").unwrap();
        // First op should be the test with indexed path
        assert_eq!(patch[0]["op"], "test");
        assert_eq!(patch[0]["path"], "/collection/0");
        assert_eq!(patch[0]["value"], "opensource");
    }

    // --- extract_target_metadata tests ---

    #[test]
    fn extract_target_item_metadata() {
        let item = serde_json::json!({
            "metadata": {"identifier": "test", "title": "Test Item"},
            "files": [{"name": "foo.pdf", "size": "100"}]
        });
        let source = extract_target_metadata(&item, "metadata", "test").unwrap();
        assert_eq!(source["identifier"], "test");
        assert_eq!(source["title"], "Test Item");
    }

    #[test]
    fn extract_target_file_metadata() {
        let item = serde_json::json!({
            "metadata": {"identifier": "test"},
            "files": [
                {"name": "foo.pdf", "size": "100"},
                {"name": "bar.txt", "size": "200", "custom": "hello"}
            ]
        });
        let source = extract_target_metadata(&item, "files/bar.txt", "test").unwrap();
        assert_eq!(source["name"], "bar.txt");
        assert_eq!(source["custom"], "hello");
    }

    #[test]
    fn extract_target_file_not_found() {
        let item = serde_json::json!({
            "metadata": {"identifier": "test"},
            "files": [{"name": "foo.pdf"}]
        });
        let result = extract_target_metadata(&item, "files/missing.txt", "my-item-id");
        assert!(result.is_err());
    }

    #[test]
    fn extract_target_file_not_found_includes_identifier() {
        let item = serde_json::json!({
            "metadata": {"identifier": "test"},
            "files": [{"name": "foo.pdf"}]
        });
        let result = extract_target_metadata(&item, "files/missing.txt", "my-item-id");
        match result.unwrap_err() {
            IaError::MetadataWrite { identifier, message } => {
                assert_eq!(identifier, "my-item-id");
                assert!(message.contains("file not found"));
                assert!(message.contains("missing.txt"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    // --- modify() async tests ---

    use wiremock::matchers::{body_string_contains, header, method, path};
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

    fn mock_item_metadata() -> serde_json::Value {
        serde_json::json!({
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
    async fn modify_request_struct_works() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/metadata/test-item"))
            .and(body_string_contains("-target=metadata"))
            .and(body_string_contains("access=test_access"))
            .and(body_string_contains("secret=test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "task_id": 12345,
                "log": "https://catalogd.archive.org/log/12345"
            })))
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("New Title"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let resp = modify(&client, &req).await.unwrap();

        assert!(resp.success);
        assert_eq!(resp.task_id, Some(12345));
    }

    #[tokio::test]
    async fn modify_errors_without_auth() {
        let config = crate::config::IaConfig::default(); // no credentials
        let client = crate::client::IaClient::from_config(config).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("New"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let result = modify(&client, &req).await;

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
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("Old Title"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let result = modify(&client, &req).await;

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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "task_id": 99999
            })))
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("custom_tag".to_string(), serde_json::json!("hello"))],
            op: MetadataOp::Set,
            target: "files/test.pdf".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let resp = modify(&client, &req).await.unwrap();

        assert!(resp.success);
    }

    #[tokio::test]
    async fn modify_file_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("tag".to_string(), serde_json::json!("val"))],
            op: MetadataOp::Set,
            target: "files/missing.pdf".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let result = modify(&client, &req).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn modify_429_returns_rate_limited() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/metadata/test-item"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "60")
            )
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("New"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let result = modify(&client, &req).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::RateLimited { retry_after } => {
                assert_eq!(retry_after, 60);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn modify_reduced_priority_sends_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/metadata/test-item"))
            .and(header("X-Accept-Reduced-Priority", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "task_id": 55555
            })))
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("New"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: Some(0),
            reduced_priority: true,
        };
        let resp = modify(&client, &req).await.unwrap();

        assert!(resp.success);
    }

    #[tokio::test]
    async fn modify_server_error_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_item_metadata()))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "error": "no changes to xml"
            })))
            .mount(&mock_server)
            .await;

        let client = crate::client::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let req = ModifyRequest {
            identifier: "test-item".to_string(),
            changes: vec![("title".to_string(), serde_json::json!("New"))],
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: None,
            priority: None,
            reduced_priority: false,
        };
        let result = modify(&client, &req).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::MetadataWrite { message, .. } => {
                assert!(message.contains("no changes to xml"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
