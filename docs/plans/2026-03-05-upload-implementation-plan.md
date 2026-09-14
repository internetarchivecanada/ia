# Upload Implementation Plan

**Goal:** Implement `ia upload` — single item upload, batch import from spreadsheet, and template generation — with robust 503 handling, checksum verification, and 100-continue support.

**Architecture:** Module directory `ia-core/src/upload/` with separate files for types, headers, validation, rate limiting, single file upload, item upload, batch upload, and template generation. CLI command in `ia-cli/src/commands/upload.rs`. Function-based public API with option structs, consistent with existing download and metadata modules.

**Tech Stack:** reqwest 0.12 (HTTP), tokio 1 (async), serde 1 (serialization), wiremock 0.6 (testing), md-5 crate (MD5 hashing), indicatif 0.17 (progress bars), clap 4 (CLI). Possibly hyper direct for 100-continue if reqwest doesn't handle it.

**Design doc:** `docs/plans/2026-03-05-upload-design.md`
**Parent issue:** #201
**Phase 1 issues:** #202-#213

---

## Task 1: Upload Error Types and S3 XML Parsing (#211)

This must come first — all other upload modules depend on these error types.

**Files:**
- Modify: `ia-core/src/error.rs`
- Test: `ia-core/src/error.rs` (inline tests)

**Step 1: Write failing tests for new error variants**

Add to the `#[cfg(test)] mod tests` block in `ia-core/src/error.rs`:

```rust
// -- Upload error tests --

#[test]
fn upload_failed_displays_details() {
    let err = IaError::UploadFailed {
        identifier: "my-item".into(),
        key: "file.pdf".into(),
        message: "connection reset".into(),
    };
    assert!(err.to_string().contains("my-item"));
    assert!(err.to_string().contains("file.pdf"));
}

#[test]
fn spam_detected_is_not_retryable() {
    let err = IaError::SpamDetected {
        identifier: "spam-item".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn collection_not_found_is_not_retryable() {
    let err = IaError::CollectionNotFound {
        collection: "nonexistent".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn invalid_identifier_is_not_retryable() {
    let err = IaError::InvalidIdentifier {
        identifier: "!!!".into(),
        reason: "invalid characters".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn missing_required_metadata_is_not_retryable() {
    let err = IaError::MissingRequiredMetadata {
        field: "mediatype".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn check_limit_failed_is_retryable() {
    let err = IaError::CheckLimitFailed {
        identifier: "my-item".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn json_upload_failed() {
    let err = IaError::UploadFailed {
        identifier: "my-item".into(),
        key: "file.pdf".into(),
        message: "connection reset".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "upload_failed");
    assert_eq!(v["error"]["identifier"], "my-item");
    assert_eq!(v["error"]["key"], "file.pdf");
}

#[test]
fn json_spam_detected() {
    let err = IaError::SpamDetected {
        identifier: "spam-item".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "spam_detected");
}

#[test]
fn json_collection_not_found() {
    let err = IaError::CollectionNotFound {
        collection: "nonexistent".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "collection_not_found");
    assert_eq!(v["error"]["collection"], "nonexistent");
}

#[test]
fn json_invalid_identifier() {
    let err = IaError::InvalidIdentifier {
        identifier: "!!!".into(),
        reason: "invalid characters".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "invalid_identifier");
    assert_eq!(v["error"]["identifier"], "!!!");
}

#[test]
fn json_missing_required_metadata() {
    let err = IaError::MissingRequiredMetadata {
        field: "mediatype".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "missing_required_metadata");
    assert_eq!(v["error"]["field"], "mediatype");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- upload_failed spam_detected collection_not_found invalid_identifier missing_required check_limit_failed`
Expected: FAIL — variants don't exist yet.

**Step 3: Add error variants to `IaError` enum**

Add these variants to the `IaError` enum in `ia-core/src/error.rs` (after the `DownloadTooLarge` variant, before the `#[error(transparent)]` section):

```rust
#[error("upload failed for {identifier}/{key}: {message}")]
UploadFailed {
    identifier: String,
    key: String,
    message: String,
},

#[error("upload blocked: {identifier} appears to be spam")]
SpamDetected { identifier: String },

#[error("collection not found: {collection}")]
CollectionNotFound { collection: String },

#[error("invalid identifier '{identifier}': {reason}")]
InvalidIdentifier { identifier: String, reason: String },

#[error("missing required metadata field: {field}")]
MissingRequiredMetadata { field: String },

#[error("check_limit request failed for {identifier}")]
CheckLimitFailed { identifier: String },

#[error("file too large: {} ({size} bytes)", path.display())]
FileTooLarge { path: PathBuf, size: u64 },

#[error("no files to upload")]
EmptyUpload,

#[error("symlink skipped: {}", path.display())]
SymlinkSkipped { path: PathBuf },
```

**Step 4: Update `is_retryable()`**

Add arms to the `match self` block:

```rust
IaError::UploadFailed { .. } => true,   // transient network issues
IaError::SpamDetected { .. } => false,  // permanent
IaError::CollectionNotFound { .. } => false,
IaError::InvalidIdentifier { .. } => false,
IaError::MissingRequiredMetadata { .. } => false,
IaError::CheckLimitFailed { .. } => true,  // conservative: treat as overloaded
IaError::FileTooLarge { .. } => false,
IaError::EmptyUpload => false,
IaError::SymlinkSkipped { .. } => false,
```

**Step 5: Update `to_json_error()`**

Add arms to the `match self` block:

```rust
IaError::UploadFailed { identifier, key, .. } => {
    extra.insert("identifier".into(), identifier.clone().into());
    extra.insert("key".into(), key.clone().into());
    "upload_failed"
}
IaError::SpamDetected { identifier } => {
    extra.insert("identifier".into(), identifier.clone().into());
    "spam_detected"
}
IaError::CollectionNotFound { collection } => {
    extra.insert("collection".into(), collection.clone().into());
    "collection_not_found"
}
IaError::InvalidIdentifier { identifier, reason } => {
    extra.insert("identifier".into(), identifier.clone().into());
    extra.insert("reason".into(), reason.clone().into());
    "invalid_identifier"
}
IaError::MissingRequiredMetadata { field } => {
    extra.insert("field".into(), field.clone().into());
    "missing_required_metadata"
}
IaError::CheckLimitFailed { identifier } => {
    extra.insert("identifier".into(), identifier.clone().into());
    "check_limit_failed"
}
IaError::FileTooLarge { path, size } => {
    extra.insert("path".into(), path.display().to_string().into());
    extra.insert("size".into(), (*size).into());
    "file_too_large"
}
IaError::EmptyUpload => "empty_upload",
IaError::SymlinkSkipped { path } => {
    extra.insert("path".into(), path.display().to_string().into());
    "symlink_skipped"
}
```

**Step 6: Run tests to verify they pass**

Run: `cargo test -p ia-core`
Expected: ALL pass (both new and existing tests).

**Step 7: Commit**

```
git add ia-core/src/error.rs
git commit -m "feat(ia-core): add upload error variants and S3 XML parsing

Add IaError variants for upload operations:
- UploadFailed, SpamDetected, CollectionNotFound
- InvalidIdentifier, MissingRequiredMetadata
- CheckLimitFailed, FileTooLarge, EmptyUpload, SymlinkSkipped

Each variant has is_retryable() classification and
to_json_error() serialization.

Closes #211"
```

---

## Task 2: Upload Types and Module Scaffold (#202)

**Files:**
- Create: `ia-core/src/upload/mod.rs`
- Create: `ia-core/src/upload/types.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Create the upload module directory and mod.rs**

Create `ia-core/src/upload/mod.rs`:

```rust
mod types;

pub use types::{UploadOpts, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus};
```

**Step 2: Create types.rs with all core types**

Create `ia-core/src/upload/types.rs` with:

```rust
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

/// Options for upload operations.
#[derive(Debug, Clone)]
pub struct UploadOpts {
    /// Metadata key-value pairs to set on the item.
    pub metadata: Vec<(String, String)>,
    /// Explicit remote filename (required for stdin uploads).
    pub remote_name: Option<String>,
    /// Prepend this path prefix to all remote filenames.
    pub remote_dir: Option<String>,
    /// Preserve relative directory structure in remote filenames.
    pub keep_directories: bool,
    /// Send Content-MD5 header for server-side verification.
    pub verify: bool,
    /// Skip files whose MD5 matches the remote copy.
    pub checksum: bool,
    /// Pre-computed MD5 checksums keyed by filename.
    pub checksums: Option<HashMap<String, String>>,
    /// Delete local file after verified upload.
    pub delete_after_upload: bool,
    /// Skip derivative generation (x-archive-queue-derive: 0 on all files).
    pub no_derive: bool,
    /// Don't keep old file versions (omit x-archive-keep-old-version).
    pub no_backup: bool,
    /// Error if item doesn't already exist.
    pub no_auto_make_bucket: bool,
    /// Don't send x-archive-size-hint header.
    pub no_size_hint: bool,
    /// Skip collection existence check.
    pub no_collection_check: bool,
    /// Upload to test_collection (items auto-removed after 30 days).
    pub test_item: bool,
    /// Use multipart upload (Phase 2).
    pub multipart: bool,
    /// Maximum retry attempts on transient failure.
    pub retries: u32,
    /// Sleep duration between retries.
    pub retry_sleep: Duration,
    /// Additional HTTP headers to include.
    pub headers: Vec<(String, String)>,
    /// Validate everything but don't actually upload.
    pub dry_run: bool,
}

impl Default for UploadOpts {
    fn default() -> Self {
        Self {
            metadata: Vec::new(),
            remote_name: None,
            remote_dir: None,
            keep_directories: false,
            verify: true,
            checksum: false,
            checksums: None,
            delete_after_upload: false,
            no_derive: false,
            no_backup: false,
            no_auto_make_bucket: false,
            no_size_hint: false,
            no_collection_check: false,
            test_item: false,
            multipart: false,
            retries: 10,
            retry_sleep: Duration::from_secs(30),
            headers: Vec::new(),
            dry_run: false,
        }
    }
}

/// Result of a single file upload.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub identifier: String,
    pub key: String,
    pub status: UploadStatus,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    pub elapsed_ms: u64,
    pub retries: u32,
}

/// Upload outcome for a single file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Uploaded,
    Skipped,
    Failed(String),
    DryRun,
}

/// Progress update during an upload.
#[derive(Debug, Clone)]
pub struct UploadProgress {
    pub identifier: String,
    pub key: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub status: UploadProgressStatus,
}

/// Current phase of an individual file upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadProgressStatus {
    Verifying,
    Uploading,
    WaitingRateLimit,
    Complete,
    Skipped,
    Failed,
}
```

**Step 3: Register upload module in lib.rs**

Add `pub mod upload;` to `ia-core/src/lib.rs` (after `pub mod update;`).

**Step 4: Verify it compiles**

Run: `cargo check -p ia-core`
Expected: OK — no errors.

**Step 5: Write a quick unit test for defaults**

Add to `ia-core/src/upload/types.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_opts_defaults() {
        let opts = UploadOpts::default();
        assert!(opts.verify);
        assert!(!opts.checksum);
        assert!(!opts.no_derive);
        assert!(!opts.no_backup);
        assert_eq!(opts.retries, 10);
        assert_eq!(opts.retry_sleep, Duration::from_secs(30));
        assert!(opts.metadata.is_empty());
    }

    #[test]
    fn upload_result_serializes_to_json() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: Some("abc123".into()),
            elapsed_ms: 500,
            retries: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("test-item"));
        assert!(json.contains("uploaded"));
    }

    #[test]
    fn upload_result_skips_none_md5() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: None,
            elapsed_ms: 500,
            retries: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("md5"));
    }
}
```

**Step 6: Run tests**

Run: `cargo test -p ia-core -- upload`
Expected: ALL pass.

**Step 7: Commit**

```
git add ia-core/src/upload/ ia-core/src/lib.rs
git commit -m "feat(ia-core): add upload module scaffold with types

Create upload/ module directory with types.rs containing:
- UploadOpts with sensible defaults (verify=true, retries=10)
- UploadResult with JSON serialization
- UploadStatus enum (Uploaded/Skipped/Failed/DryRun)
- UploadProgress and UploadProgressStatus for callbacks

Ref #202"
```

---

## Task 3: S3 Header Construction (#203)

The most bug-prone part. Extensive tests are critical.

**Files:**
- Create: `ia-core/src/upload/headers.rs`
- Modify: `ia-core/src/upload/mod.rs`

**Step 1: Write comprehensive tests first**

Create `ia-core/src/upload/headers.rs` starting with tests:

```rust
// Implementation will go above tests

#[cfg(test)]
mod tests {
    use super::*;

    // -- needs_quote tests --

    #[test]
    fn needs_quote_ascii_no_spaces() {
        assert!(!needs_quote("hello"));
        assert!(!needs_quote("foo-bar_baz.123"));
    }

    #[test]
    fn needs_quote_with_spaces() {
        assert!(needs_quote("hello world"));
        assert!(needs_quote("foo\tbar"));
        assert!(needs_quote("line\nbreak"));
    }

    #[test]
    fn needs_quote_non_ascii() {
        assert!(needs_quote("snowman ☃"));
        assert!(needs_quote("日本語"));
        assert!(needs_quote("café"));
    }

    #[test]
    fn needs_quote_empty() {
        assert!(!needs_quote(""));
    }

    // -- encode_metadata_headers tests --

    #[test]
    fn simple_single_value() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "My Item".into()),
        ]);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-archive-meta00-title");
        // "My Item" has a space, so needs uri() encoding
        assert_eq!(headers[0].1, "uri(My%20Item)");
    }

    #[test]
    fn no_space_no_encoding() {
        let headers = encode_metadata_headers(&[
            ("mediatype".into(), "texts".into()),
        ]);
        assert_eq!(headers[0].1, "texts");
    }

    #[test]
    fn underscore_in_key_becomes_double_dash() {
        let headers = encode_metadata_headers(&[
            ("my_field".into(), "value".into()),
        ]);
        assert_eq!(headers[0].0, "x-archive-meta00-my--field");
    }

    #[test]
    fn multivalue_incrementing_index() {
        let headers = encode_metadata_headers(&[
            ("subject".into(), "rust".into()),
            ("subject".into(), "archive".into()),
            ("subject".into(), "cli".into()),
        ]);
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].0, "x-archive-meta00-subject");
        assert_eq!(headers[0].1, "rust");
        assert_eq!(headers[1].0, "x-archive-meta01-subject");
        assert_eq!(headers[1].1, "archive");
        assert_eq!(headers[2].0, "x-archive-meta02-subject");
        assert_eq!(headers[2].1, "cli");
    }

    #[test]
    fn mixed_fields_each_start_at_zero() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "Test".into()),
            ("subject".into(), "a".into()),
            ("subject".into(), "b".into()),
        ]);
        // title gets index 00, subject[0] gets 00, subject[1] gets 01
        let title_h: Vec<_> = headers.iter().filter(|h| h.0.contains("title")).collect();
        let subj_h: Vec<_> = headers.iter().filter(|h| h.0.contains("subject")).collect();
        assert_eq!(title_h.len(), 1);
        assert_eq!(title_h[0].0, "x-archive-meta00-title");
        assert_eq!(subj_h.len(), 2);
        assert_eq!(subj_h[0].0, "x-archive-meta00-subject");
        assert_eq!(subj_h[1].0, "x-archive-meta01-subject");
    }

    #[test]
    fn empty_value_skipped() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "".into()),
            ("mediatype".into(), "texts".into()),
        ]);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-archive-meta00-mediatype");
    }

    #[test]
    fn non_ascii_uri_encoded() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "snowman ☃".into()),
        ]);
        assert_eq!(headers[0].1, "uri(snowman%20%E2%98%83)");
    }

    #[test]
    fn cjk_uri_encoded() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "日本語".into()),
        ]);
        assert!(headers[0].1.starts_with("uri("));
    }

    #[test]
    fn emoji_uri_encoded() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "🚀".into()),
        ]);
        assert!(headers[0].1.starts_with("uri("));
    }

    // -- encode_file_metadata_headers tests --

    #[test]
    fn file_metadata_uses_filemeta_prefix() {
        let headers = encode_file_metadata_headers(&[
            ("title".into(), "MyFile".into()),
        ]);
        assert_eq!(headers[0].0, "x-archive-filemeta00-title");
    }

    // -- S3 XML error parsing tests --

    #[test]
    fn parse_s3_error_valid_xml() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8'?>
<Error>
  <Code>SlowDown</Code>
  <Message>Please reduce your request rate.</Message>
  <Resource>/my-item/file.pdf</Resource>
  <RequestId>db1b9e2b-1234</RequestId>
</Error>"#;
        let (code, message) = parse_s3_error_xml(xml);
        assert_eq!(code.as_deref(), Some("SlowDown"));
        assert_eq!(message.as_deref(), Some("Please reduce your request rate."));
    }

    #[test]
    fn parse_s3_error_missing_message() {
        let xml = "<Error><Code>AccessDenied</Code></Error>";
        let (code, message) = parse_s3_error_xml(xml);
        assert_eq!(code.as_deref(), Some("AccessDenied"));
        assert!(message.is_none());
    }

    #[test]
    fn parse_s3_error_malformed() {
        let (code, message) = parse_s3_error_xml("not xml at all");
        assert!(code.is_none());
        assert!(message.is_none());
    }

    #[test]
    fn parse_s3_error_html_error_page() {
        let html = "<html><body>500 Internal Server Error</body></html>";
        let (code, message) = parse_s3_error_xml(html);
        assert!(code.is_none());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- upload::headers`
Expected: FAIL — functions don't exist.

**Step 3: Implement the header encoding functions**

Add implementation above the tests in `ia-core/src/upload/headers.rs`:

```rust
use std::collections::HashMap;
use urlencoding::encode as url_encode;

/// Check if a string value needs uri() encoding.
///
/// Returns true if the string contains non-ASCII characters or any whitespace.
pub(crate) fn needs_quote(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Non-ASCII check
    if !s.is_ascii() {
        return true;
    }
    // Whitespace check
    s.chars().any(|c| c.is_whitespace())
}

/// Encode a value for an IA S3 metadata header.
///
/// If the value contains non-ASCII or whitespace, wraps it as `uri({percent_encoded})`.
fn encode_value(value: &str) -> String {
    if needs_quote(value) {
        format!("uri({})", url_encode(value))
    } else {
        value.to_string()
    }
}

/// Encode a metadata key for IA S3 headers.
///
/// Replaces underscores with double-dashes per IA convention.
fn encode_key(key: &str) -> String {
    key.replace('_', "--")
}

/// Encode metadata key-value pairs into x-archive-meta headers.
///
/// Handles multivalue fields (incrementing index per field name),
/// underscore-to-double-dash key encoding, and uri() value encoding.
/// Skips empty values.
pub fn encode_metadata_headers(metadata: &[(String, String)]) -> Vec<(String, String)> {
    encode_headers_with_prefix(metadata, "meta")
}

/// Encode file-level metadata into x-archive-filemeta headers.
pub fn encode_file_metadata_headers(metadata: &[(String, String)]) -> Vec<(String, String)> {
    encode_headers_with_prefix(metadata, "filemeta")
}

fn encode_headers_with_prefix(
    metadata: &[(String, String)],
    prefix: &str,
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut index_counters: HashMap<String, usize> = HashMap::new();

    for (key, value) in metadata {
        if value.is_empty() {
            continue;
        }

        let idx = index_counters.entry(key.clone()).or_insert(0);
        let header_key = format!("x-archive-{}{:02}-{}", prefix, idx, encode_key(key));
        let header_value = encode_value(value);

        result.push((header_key, header_value));
        *index_counters.get_mut(key).unwrap() += 1;
    }

    result
}

/// Parse an S3 XML error response, extracting Code and Message.
///
/// Uses basic string matching — no XML crate needed. Returns (Option<Code>, Option<Message>).
/// Gracefully returns (None, None) on malformed input.
pub fn parse_s3_error_xml(body: &str) -> (Option<String>, Option<String>) {
    let code = extract_xml_tag(body, "Code");
    let message = extract_xml_tag(body, "Message");
    (code, message)
}

/// Extract text content from a simple XML tag like `<Tag>content</Tag>`.
fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)?;
    let content_start = start + open.len();
    let end = xml[content_start..].find(&close)?;
    let content = &xml[content_start..content_start + end];
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}
```

**Step 4: Add `urlencoding` to Cargo.toml if not already present**

Check `ia-core/Cargo.toml` — `urlencoding` 2 is already listed in the crate stack (used for metadata write). If not in Cargo.toml, add `urlencoding = "2"` to `[dependencies]`.

**Step 5: Export headers module**

In `ia-core/src/upload/mod.rs`, add:

```rust
pub mod headers;
```

**Step 6: Run tests**

Run: `cargo test -p ia-core -- upload::headers`
Expected: ALL pass.

**Step 7: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: ALL pass.

**Step 8: Commit**

```
git add ia-core/src/upload/headers.rs ia-core/src/upload/mod.rs
git commit -m "feat(ia-core): S3 header construction and metadata encoding

Implement IA S3 header encoding rules:
- x-archive-meta{NN:02d}-{key} format
- Underscore → double-dash in key names
- uri() wrapping for non-ASCII and whitespace values
- Multivalue: incrementing index per field
- Empty values skipped
- File-level metadata via filemeta prefix
- S3 XML error parsing (Code/Message extraction)

Comprehensive tests for encoding edge cases including
emoji, CJK, mixed content, multivalue, and malformed XML.

Ref #203"
```

---

## Task 4: Upload Validation (#204)

**Files:**
- Create: `ia-core/src/upload/validate.rs`
- Modify: `ia-core/src/upload/mod.rs`

**Step 1: Write tests for identifier validation**

Create `ia-core/src/upload/validate.rs` with tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_identifier tests --

    #[test]
    fn valid_identifiers() {
        assert!(validate_identifier("nasa").is_ok());
        assert!(validate_identifier("my-item-123").is_ok());
        assert!(validate_identifier("test.item").is_ok());
        assert!(validate_identifier("a_b_c").is_ok());
        assert!(validate_identifier("abc").is_ok()); // minimum length
        assert!(validate_identifier("@username").is_ok()); // user items
    }

    #[test]
    fn invalid_identifier_too_short() {
        assert!(validate_identifier("ab").is_err());
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn invalid_identifier_too_long() {
        let long = "a".repeat(101);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn invalid_identifier_bad_chars() {
        assert!(validate_identifier("has space").is_err());
        assert!(validate_identifier("has!bang").is_err());
        assert!(validate_identifier("has#hash").is_err());
    }

    #[test]
    fn invalid_identifier_bad_start() {
        assert!(validate_identifier(".dotstart").is_err());
        assert!(validate_identifier("_understart").is_err());
        assert!(validate_identifier("-dashstart").is_err());
    }

    #[test]
    fn valid_identifier_at_max_length() {
        let exactly_100 = "a".repeat(100);
        assert!(validate_identifier(&exactly_100).is_ok());
    }

    // -- validate_required_metadata tests --

    #[test]
    fn valid_metadata_has_required_fields() {
        let meta = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        assert!(validate_required_metadata(&meta).is_ok());
    }

    #[test]
    fn missing_mediatype() {
        let meta = vec![("collection".into(), "test_collection".into())];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "mediatype"));
    }

    #[test]
    fn missing_collection() {
        let meta = vec![("mediatype".into(), "texts".into())];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "collection"));
    }
}
```

**Step 2: Implement validation functions**

Add above the tests:

```rust
use crate::error::IaError;
use std::path::Path;

/// Validate an IA identifier.
///
/// Rules: 3-100 chars, `[a-zA-Z0-9._-]`, must start with alphanumeric or `@`.
pub fn validate_identifier(id: &str) -> Result<(), IaError> {
    if id.is_empty() || id.len() < 3 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at least 3 characters".into(),
        });
    }
    if id.len() > 100 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at most 100 characters".into(),
        });
    }

    let first = id.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() && first != '@' {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("must start with alphanumeric or '@', got '{}'", first),
        });
    }

    if let Some(bad) = id.chars().find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '@')) {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("contains invalid character '{}'", bad),
        });
    }

    Ok(())
}

/// Validate that required metadata fields are present.
///
/// Required fields: `mediatype`, `collection`.
pub fn validate_required_metadata(metadata: &[(String, String)]) -> Result<(), IaError> {
    let has = |field: &str| metadata.iter().any(|(k, v)| k == field && !v.is_empty());

    if !has("mediatype") {
        return Err(IaError::MissingRequiredMetadata {
            field: "mediatype".into(),
        });
    }
    if !has("collection") {
        return Err(IaError::MissingRequiredMetadata {
            field: "collection".into(),
        });
    }

    Ok(())
}

/// Check that a file exists and is not a symlink.
///
/// Returns `Err(SymlinkSkipped)` for symlinks, `Err(Io)` if file doesn't exist.
pub fn validate_file(path: &Path) -> Result<(), IaError> {
    let symlink_meta = std::fs::symlink_metadata(path)?;
    if symlink_meta.file_type().is_symlink() {
        return Err(IaError::SymlinkSkipped {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}
```

**Step 3: Export in mod.rs**

Add `pub mod validate;` to `ia-core/src/upload/mod.rs`.

**Step 4: Run tests**

Run: `cargo test -p ia-core -- upload::validate`
Expected: ALL pass.

**Step 5: Commit**

```
git add ia-core/src/upload/validate.rs ia-core/src/upload/mod.rs
git commit -m "feat(ia-core): upload validation (identifier, metadata, files)

Implement pre-flight validation:
- validate_identifier: 3-100 chars, [a-zA-Z0-9._-@], start with alnum/@
- validate_required_metadata: mediatype and collection required
- validate_file: file exists and is not a symlink

Ref #204"
```

---

## Task 5: Per-Item Rate Limiter and check_limit (#205)

**Files:**
- Create: `ia-core/src/upload/check_limit.rs`
- Modify: `ia-core/src/upload/mod.rs`

**Step 1: Write tests**

Create `ia-core/src/upload/check_limit.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_check_limit_clear() {
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":0,"detail":"ok"}"#;
        assert!(!parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_over() {
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":1,"detail":"slow"}"#;
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_malformed_json() {
        // Conservative: treat as overloaded
        assert!(parse_check_limit_response("not json"));
    }

    #[test]
    fn parse_check_limit_missing_field() {
        let json = r#"{"bucket":"test"}"#;
        // Missing over_limit → treat as overloaded
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn is_spam_response_detects_spam() {
        assert!(is_spam_response("Your upload appears to be spam."));
        assert!(is_spam_response("blah blah appears to be spam blah"));
    }

    #[test]
    fn is_spam_response_normal_503() {
        assert!(!is_spam_response("Please reduce your request rate."));
        assert!(!is_spam_response(""));
    }
}
```

**Step 2: Implement check_limit logic**

Add above tests:

```rust
use serde::Deserialize;

/// Parsed check_limit API response.
#[derive(Debug, Deserialize)]
struct CheckLimitResponse {
    over_limit: Option<i64>,
}

/// Parse a check_limit JSON response. Returns `true` if over limit.
///
/// Conservative: returns `true` (overloaded) on any parse error or missing field.
pub(crate) fn parse_check_limit_response(body: &str) -> bool {
    match serde_json::from_str::<CheckLimitResponse>(body) {
        Ok(resp) => resp.over_limit.unwrap_or(1) != 0,
        Err(_) => true, // conservative: treat as overloaded
    }
}

/// Check if a 503 response body indicates spam detection.
pub(crate) fn is_spam_response(body: &str) -> bool {
    body.contains("appears to be spam")
}

/// Status reported during rate limit polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitStatus {
    /// Sending check_limit request.
    Polling,
    /// Waiting before next poll.
    Waiting { seconds: u64 },
    /// Rate limit cleared, resuming uploads.
    Cleared,
    /// Retries exhausted.
    Exhausted,
}
```

**Step 3: Export in mod.rs**

Add `pub mod check_limit;` to `ia-core/src/upload/mod.rs`. Add `pub use check_limit::RateLimitStatus;` to the public re-exports.

**Step 4: Run tests**

Run: `cargo test -p ia-core -- upload::check_limit`
Expected: ALL pass.

**Step 5: Commit**

```
git add ia-core/src/upload/check_limit.rs ia-core/src/upload/mod.rs
git commit -m "feat(ia-core): per-item rate limiter and check_limit parsing

Implement check_limit response parsing:
- parse_check_limit_response: JSON parsing, conservative on error
- is_spam_response: detects permanent spam block
- RateLimitStatus enum for progress reporting

Ref #205"
```

---

## Task 6: 100-Continue Verification (#206)

**Files:**
- Create: `ia-core/tests/upload_100_continue.rs`

**Step 1: Write a test to verify reqwest/hyper 100-continue behavior**

```rust
//! Test whether reqwest respects Expect: 100-continue.
//!
//! If the server rejects before sending 100 Continue,
//! does reqwest avoid sending the body?

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn expect_100_continue_server_rejects_before_body() {
    let server = MockServer::start().await;

    // Mock that returns 403 immediately (no 100 Continue)
    Mock::given(method("PUT"))
        .and(path("/test-item/file.bin"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Access Denied"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let body = vec![0u8; 1_000_000]; // 1 MB body

    let response = client
        .put(format!("{}/test-item/file.bin", server.uri()))
        .header("Expect", "100-continue")
        .header("Content-Length", body.len().to_string())
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 403);

    // Note: We cannot easily verify that the body was NOT sent with wiremock.
    // This test verifies the basic flow works. True 100-continue verification
    // requires monitoring bytes on the wire, which we'll do manually with curl.
    //
    // The real verification: run this against IA with x-archive-simulate-error:AccessDenied
    // and observe that curl with -v shows body is not sent after 403.
}
```

**Step 2: Run the test**

Run: `cargo test -p ia-core --test upload_100_continue`
Expected: PASS (but doesn't fully prove 100-continue — see note in test).

**Step 3: Document findings**

The test framework (wiremock) cannot definitively prove 100-continue behavior because it doesn't expose byte-level protocol details. The real verification must happen manually:

```bash
# Manual test against IA S3 with error simulation:
curl -v -X PUT 'https://s3.us.archive.org/test-item/test-file' \
  -H 'Authorization: LOW access:secret' \
  -H 'Expect: 100-continue' \
  -H 'x-archive-simulate-error:AccessDenied' \
  --data-binary @/dev/zero -o /dev/null 2>&1 | head -50
# Look for: "< HTTP/1.1 403" BEFORE "=> Send data"
```

**Step 4: Commit**

```
git add ia-core/tests/upload_100_continue.rs
git commit -m "test(ia-core): 100-continue verification test scaffold

Add integration test for Expect: 100-continue behavior.
Note: full wire-level verification requires manual testing
with curl against live IA S3 (simulate-error).

Ref #206"
```

---

## Task 7: Checksum and Verification (#213)

**Files:**
- Create: `ia-core/src/upload/checksum.rs`
- Modify: `ia-core/src/upload/mod.rs`
- Modify: `ia-core/Cargo.toml` (add md-5 crate if needed)

**Step 1: Check if md-5 or similar is available**

Look at existing `ia-core/Cargo.toml` for MD5 dependencies. The download module has checksum support — check what it uses.

Run: `grep -r "md5\|Md5\|MD5" ia-core/src/`

If no MD5 crate exists, add `md-5 = "0.10"` to `ia-core/Cargo.toml` (`md-5` is the RustCrypto MD5 implementation). Ask the user before adding.

**Step 2: Write tests for checksums file parsing and MD5**

Create `ia-core/src/upload/checksum.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parse_gnu_md5sum_format() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\
                      abc123def456abc123def456abc123de  other.pdf\n";
        let map = parse_checksums(input).unwrap();
        assert_eq!(map.get("file.txt").unwrap(), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(map.get("other.pdf").unwrap(), "abc123def456abc123def456abc123de");
    }

    #[test]
    fn parse_bsd_md5_format() {
        let input = "MD5 (file.txt) = d41d8cd98f00b204e9800998ecf8427e\n\
                      MD5 (other.pdf) = abc123def456abc123def456abc123de\n";
        let map = parse_checksums(input).unwrap();
        assert_eq!(map.get("file.txt").unwrap(), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn parse_mixed_formats() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\
                      MD5 (other.pdf) = abc123def456abc123def456abc123de\n";
        let map = parse_checksums(input).unwrap();
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_skips_blank_lines() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\n\n";
        let map = parse_checksums(input).unwrap();
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn compute_md5_of_known_content() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let md5 = compute_file_md5(f.path()).unwrap();
        // MD5 of empty string
        assert_eq!(md5, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn compute_md5_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let md5 = compute_file_md5(f.path()).unwrap();
        assert_eq!(md5, "5d41402abc4b2a76b9719d911017c592");
    }
}
```

**Step 3: Implement**

Add above tests:

```rust
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use md5::{Md5, Digest};

/// Compute the MD5 hex digest of a file.
pub fn compute_file_md5(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 1024 * 1024]; // 1 MiB chunks
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Parse a checksums file (GNU md5sum or BSD md5 format).
///
/// Returns a map of filename → hex MD5 digest.
/// Skips blank lines and unrecognized formats with a warning.
pub fn parse_checksums(content: &str) -> Result<HashMap<String, String>, std::io::Error> {
    let mut map = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try GNU md5sum format: "hash  filename" or "hash filename"
        if let Some((hash, filename)) = try_parse_gnu(line) {
            map.insert(filename, hash);
            continue;
        }

        // Try BSD format: "MD5 (filename) = hash"
        if let Some((hash, filename)) = try_parse_bsd(line) {
            map.insert(filename, hash);
            continue;
        }

        // Unrecognized line — skip with tracing warning
        tracing::warn!("unrecognized checksums line: {}", line);
    }

    Ok(map)
}

fn try_parse_gnu(line: &str) -> Option<(String, String)> {
    // "hash  filename" — two spaces, or "hash filename" — one space
    let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 {
        return None;
    }
    let hash = parts[0].trim();
    let filename = parts[1].trim();
    // MD5 is 32 hex chars
    if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !filename.is_empty() {
        Some((hash.to_string(), filename.to_string()))
    } else {
        None
    }
}

fn try_parse_bsd(line: &str) -> Option<(String, String)> {
    // "MD5 (filename) = hash"
    let line = line.strip_prefix("MD5 (")?;
    let (filename, rest) = line.split_once(") = ")?;
    let hash = rest.trim();
    if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some((hash.to_string(), filename.to_string()))
    } else {
        None
    }
}
```

**Step 4: Add md-5 dependency**

Add to `ia-core/Cargo.toml` under `[dependencies]`:
```toml
md-5 = "0.10"
```

Update the import: `use md5::{Md5, Digest};` — the `md-5` crate re-exports as `md5`.

**Step 5: Export in mod.rs**

Add `pub mod checksum;` to `ia-core/src/upload/mod.rs`.

**Step 6: Run tests**

Run: `cargo test -p ia-core -- upload::checksum`
Expected: ALL pass.

**Step 7: Commit**

```
git add ia-core/src/upload/checksum.rs ia-core/src/upload/mod.rs ia-core/Cargo.toml
git commit -m "feat(ia-core): checksum verification and checksums file parsing

Implement MD5 file hashing and checksums file parsing:
- compute_file_md5: streaming MD5 with 1 MiB chunks
- parse_checksums: GNU md5sum and BSD md5 formats
- Auto-detect format per line, skip unrecognized with warning

New dependency: md-5 0.10 (RustCrypto MD5)

Ref #213"
```

---

## Task 8: Single File Upload (#207)

The core upload function. Depends on Tasks 1-7.

**Files:**
- Create: `ia-core/src/upload/single.rs`
- Modify: `ia-core/src/upload/mod.rs`
- Create: `ia-core/tests/upload_single.rs` (integration tests)

**Step 1: Write integration tests with wiremock**

Create `ia-core/tests/upload_single.rs`:

```rust
use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::IaClient;
use std::io::Write;
use tempfile::NamedTempFile;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Create a test client pointing at a mock server.
fn test_client(base_url: &str) -> IaClient {
    // Create a client configured to use the mock server
    let mut config = ia_core::IaConfig::default();
    config.s3.access = Some("test-access".into());
    config.s3.secret = Some("test-secret".into());
    // Override host to point at mock server
    config.general.host = base_url
        .strip_prefix("http://")
        .unwrap_or(base_url)
        .to_string();
    config.general.secure = false;
    IaClient::from_config(config).unwrap()
}

#[tokio::test]
async fn upload_single_file_success() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/hello.txt"))
        .and(header("authorization", "LOW test-access:test-secret"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"hello world").unwrap();
    f.flush().unwrap();

    let client = test_client(&server.uri());
    let opts = UploadOpts {
        no_verify: true, // skip MD5 for simplicity
        ..Default::default()
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "hello.txt",
        &opts,
        true, // is_last_file
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 11);
}

// ... additional integration tests for 503 retry, checksum skip, etc.
// These will be fleshed out during implementation.
```

**Step 2: Implement upload_file in single.rs**

This is the largest single implementation. Create `ia-core/src/upload/single.rs` with the full retry loop, header construction, streaming body, and progress callbacks. Reference the design doc section "Upload Flow — Single File" for the exact sequence.

Key implementation details:
- Build URL: `{protocol}://s3.us.archive.org/{identifier}/{url_encoded_key}`
- Use `reqwest::Body::wrap_stream()` or `reqwest::Body::from(bytes)` for the body
- Set `Content-Length` explicitly (never chunked)
- Retry loop with check_limit polling on 503
- Spam detection in 503 response body
- Seek to start on retry (if using a file, re-open or use `tokio::fs::File`)

**Step 3: Export in mod.rs**

Add `mod single;` and update the public re-exports to include `upload_file`.

**Step 4: Run integration tests**

Run: `cargo test -p ia-core --test upload_single`

**Step 5: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`

**Step 6: Commit**

```
git commit -m "feat(ia-core): single file upload with retry loop

Implement upload_file():
- PUT to s3.us.archive.org with streaming body
- Content-Length always set, never chunked
- Expect: 100-continue header
- Content-MD5 when verify=true
- Retry loop with check_limit polling on 503
- Spam detection for permanent 503s
- Checksum skip when --checksum enabled
- Dry run support
- Progress callbacks

Ref #207"
```

---

## Task 9: Multi-File Item Upload (#208)

**Files:**
- Create: `ia-core/src/upload/item.rs`
- Modify: `ia-core/src/upload/mod.rs`

Implement `upload_item()`:
- Validate identifier and collections
- Expand directories (walkdir or std::fs recursion)
- Compute total size for size_hint
- Map files to remote keys
- Upload with semaphore (2-3 concurrent)
- First file gets metadata headers + auto-make-bucket
- Last file gets queue-derive
- `--test-item` injects `collection:test_collection`

Integration test: multi-file upload against wiremock, verify derive header on last file only.

Commit message ref: `Ref #208`

---

## Task 10: Batch Upload from Spreadsheet (#209)

**Files:**
- Create: `ia-core/src/upload/batch.rs`
- Modify: `ia-core/src/upload/mod.rs`

Implement `upload_batch()`:
- Read spreadsheet records (reuse `spreadsheet.rs`)
- Pre-scan: group by identifier, validate required metadata
- Check collections upfront
- Upload items concurrently via `-j/--jobs`
- Joblog entry per file

This depends on Task 9. Integration tests with wiremock for multi-item concurrent uploads.

Commit message ref: `Ref #209`

---

## Task 11: Template Spreadsheet Generation (#210)

**Files:**
- Create: `ia-core/src/upload/template.rs`
- Modify: `ia-core/src/upload/mod.rs`

Implement template generation:
- Walk directory recursively, skip symlinks/dotfiles
- Generate spreadsheet with required + recommended columns
- `--identifier-prefix` and `--identifier-from-*` support
- Identifier sanitization
- Output via `spreadsheet.rs` writer

Commit message ref: `Ref #210`

---

## Task 12: CLI Upload Command (#212)

**Files:**
- Create: `ia-cli/src/commands/upload.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

Implement the CLI:
- `UploadArgs` with clap derive
- Sub-subcommands: bare (default), `import`, `template`, `cleanup` (hidden)
- All flags from design doc
- Default progress output with indicatif
- `--json` JSONL output
- `--quiet` suppresses progress
- Wire up to `main.rs` command routing
- Help text with examples
- Stdin support (detect `-` as file arg)
- `--open-after-upload` opens browser
- `--test-item` convenience flag

Commit message ref: `Ref #212`

---

## Task 13: CLI Tests (#212 continued)

**Files:**
- Create: `ia-cli/tests/upload.rs`

CLI-level tests via `assert_cmd`:
- `ia upload --dry-run` validates without uploading
- `ia upload --test-item` injects test_collection
- `ia upload --json` produces JSONL
- `ia upload import` reads spreadsheet
- `ia upload template` generates CSV
- Error cases: missing file, bad identifier, missing metadata

Commit message ref: `Ref #212`

---

## Task 14: Final Integration and Documentation

**Files:**
- Modify: `CLAUDE.md` (add upload to crate stack if new deps)
- Modify: `ia-cli/src/main.rs` (update long_about examples)

Steps:
1. Run `cargo test -p ia-core -p ia-cli` — all pass
2. Run `cargo clippy -p ia-core -p ia-cli -- -D warnings` — zero warnings
3. Update MEMORY.md with upload module status
4. Verify all help text is accurate
5. Final commit

---

## Phase 2: Multipart Upload (Future)

Issues #214-#216. Additive — new module + flag, no changes to Phase 1.

**Task P2-1:** Implement `upload/multipart.rs` — initiate, upload parts, complete (#214)
**Task P2-2:** Implement automatic resume — discover in-progress uploads, skip completed parts (#215)
**Task P2-3:** Implement hidden `cleanup` subcommand (#216)

---

## Phase 3: Dashboard (Future)

Issues #217-#218. Can be developed in parallel once progress callback API is stable.

**Task P3-1:** Factor shared TUI infrastructure from download dashboard (#217)
**Task P3-2:** Upload dashboard panels — S3 tasks, rate limit, file progress (#218)
