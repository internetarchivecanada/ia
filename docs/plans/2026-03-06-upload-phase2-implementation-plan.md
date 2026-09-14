# Upload Phase 2 (Multipart + Resume) Implementation Plan

**Goal:** Add multipart upload with automatic resume and a cleanup subcommand to `ia upload`, enabling reliable uploads of large files (>5 GB) over unreliable connections.

**Architecture:** New `upload/multipart.rs` module implements the S3 multipart protocol (initiate, upload parts, complete, abort, resume). The existing `upload_file()` in `single.rs` dispatches to multipart when `opts.multipart` is true. The CLI gains a `--multipart` flag and an unhidden `cleanup` subcommand. All state for resume lives server-side (no local state files).

**Tech Stack:** reqwest (HTTP), wiremock (testing), serde (JSON for types), string-matching XML parsing (consistent with existing `s3_error.rs`), tokio (async file I/O)

**Design doc:** `docs/plans/2026-03-05-upload-design.md` (Multipart Upload section)
**Issues:** #214 (core multipart), #215 (resume), #216 (cleanup subcommand)

---

## Task 1: Multipart Types and Error Variants

Add the foundation types that all subsequent tasks depend on.

**Files:**
- Modify: `ia-core/src/upload/types.rs`
- Modify: `ia-core/src/error.rs`

### Step 1: Write tests for new types

Add to end of `ia-core/src/upload/types.rs` tests module:

```rust
#[test]
fn multipart_upload_info_debug() {
    let info = MultipartUploadInfo {
        key: "file.zip".into(),
        upload_id: "abc123".into(),
        initiated: "2026-03-06T12:00:00Z".into(),
    };
    assert_eq!(info.key, "file.zip");
    assert_eq!(info.upload_id, "abc123");
    let dbg = format!("{info:?}");
    assert!(dbg.contains("file.zip"));
}

#[test]
fn part_info_debug() {
    let part = PartInfo {
        part_number: 1,
        etag: "\"abc123\"".into(),
        size: 104857600,
    };
    assert_eq!(part.part_number, 1);
    assert_eq!(part.size, 104857600);
}

#[test]
fn multipart_upload_info_serializes() {
    let info = MultipartUploadInfo {
        key: "file.zip".into(),
        upload_id: "abc123".into(),
        initiated: "2026-03-06T12:00:00Z".into(),
    };
    let val: serde_json::Value = serde_json::to_value(&info).unwrap();
    assert_eq!(val["key"], "file.zip");
    assert_eq!(val["upload_id"], "abc123");
    assert_eq!(val["initiated"], "2026-03-06T12:00:00Z");
}
```

### Step 2: Run tests to verify they fail

Run: `cargo test -p ia-core -- multipart_upload_info_debug part_info_debug multipart_upload_info_serializes`
Expected: compile error — `MultipartUploadInfo` and `PartInfo` not defined.

### Step 3: Add types to `types.rs`

Add after the `UploadProgressStatus` enum (before `#[cfg(test)]`):

```rust
/// Information about an in-progress multipart upload (from S3 list-uploads).
#[derive(Debug, Clone, Serialize)]
pub struct MultipartUploadInfo {
    /// Remote filename (S3 key).
    pub key: String,
    /// Server-assigned upload ID.
    pub upload_id: String,
    /// ISO 8601 timestamp when the upload was initiated.
    pub initiated: String,
}

/// Information about a completed part (from S3 list-parts).
#[derive(Debug, Clone)]
pub struct PartInfo {
    /// 1-based part number.
    pub part_number: u32,
    /// ETag returned by S3 for this part (includes quotes).
    pub etag: String,
    /// Size in bytes.
    pub size: u64,
}
```

### Step 4: Run tests to verify they pass

Run: `cargo test -p ia-core -- multipart_upload_info_debug part_info_debug multipart_upload_info_serializes`
Expected: 3 PASS

### Step 5: Write tests for new error variants

Add to end of `ia-core/src/error.rs` tests module:

```rust
#[test]
fn multipart_aborted_displays_details() {
    let err = IaError::MultipartAborted {
        identifier: "my-item".into(),
        key: "file.zip".into(),
    };
    assert!(err.to_string().contains("my-item"));
    assert!(err.to_string().contains("file.zip"));
    assert!(!err.is_retryable());
}

#[test]
fn multipart_incomplete_displays_details() {
    let err = IaError::MultipartIncomplete {
        identifier: "my-item".into(),
        key: "file.zip".into(),
        upload_id: "abc123".into(),
    };
    assert!(err.to_string().contains("my-item"));
    assert!(err.to_string().contains("file.zip"));
    assert!(!err.is_retryable());
}

#[test]
fn json_multipart_aborted() {
    let err = IaError::MultipartAborted {
        identifier: "my-item".into(),
        key: "file.zip".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "multipart_aborted");
    assert_eq!(v["error"]["identifier"], "my-item");
    assert_eq!(v["error"]["key"], "file.zip");
}

#[test]
fn json_multipart_incomplete() {
    let err = IaError::MultipartIncomplete {
        identifier: "my-item".into(),
        key: "file.zip".into(),
        upload_id: "abc123".into(),
    };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "multipart_incomplete");
    assert_eq!(v["error"]["identifier"], "my-item");
    assert_eq!(v["error"]["upload_id"], "abc123");
}
```

### Step 6: Run tests to verify they fail

Run: `cargo test -p ia-core -- multipart_aborted multipart_incomplete`
Expected: compile error — variants not defined.

### Step 7: Add error variants to `error.rs`

Add two variants to the `IaError` enum (after the `SymlinkSkipped` variant, before `Network`):

```rust
#[error("multipart upload aborted for {identifier}/{key}")]
MultipartAborted { identifier: String, key: String },

#[error("multipart upload incomplete for {identifier}/{key} (upload_id: {upload_id})")]
MultipartIncomplete { identifier: String, key: String, upload_id: String },
```

Add arms to `is_retryable()` (in the upload errors section):

```rust
IaError::MultipartAborted { .. } => false,
IaError::MultipartIncomplete { .. } => false,
```

Add arms to `to_json_error()` (in the upload errors section):

```rust
IaError::MultipartAborted { identifier, key } => {
    extra.insert("identifier".into(), identifier.clone().into());
    extra.insert("key".into(), key.clone().into());
    "multipart_aborted"
}
IaError::MultipartIncomplete { identifier, key, upload_id } => {
    extra.insert("identifier".into(), identifier.clone().into());
    extra.insert("key".into(), key.clone().into());
    extra.insert("upload_id".into(), upload_id.clone().into());
    "multipart_incomplete"
}
```

### Step 8: Run all tests

Run: `cargo test -p ia-core`
Expected: ALL PASS (including new tests)

### Step 9: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 10: Commit

```
feat(upload): add multipart types and error variants

Add MultipartUploadInfo and PartInfo types for S3 multipart protocol.
Add MultipartAborted and MultipartIncomplete error variants with
JSON serialization support.

Part of #214, #215.
```

---

## Task 2: XML Parsing Helpers for Multipart S3 Responses

The S3 multipart API returns XML for initiate, list-uploads, and list-parts.
Extend the existing string-matching approach from `s3_error.rs`.

**Files:**
- Create: `ia-core/src/upload/multipart.rs`
- Modify: `ia-core/src/upload/mod.rs`

### Step 1: Create `multipart.rs` with parsing functions and tests

Create `ia-core/src/upload/multipart.rs`:

```rust
//! Multipart upload support for IA S3.
//!
//! Implements the S3 multipart upload protocol:
//! - Initiate: POST /{id}/{key}?uploads → UploadId
//! - Upload part: PUT /{id}/{key}?partNumber={N}&uploadId={ID} → ETag
//! - Complete: POST /{id}/{key}?uploadId={ID} with XML manifest
//! - Resume: GET /{id}?uploads → list, GET /{id}/{key}?uploadId={ID} → parts
//! - Abort: DELETE /{id}/{key}?uploadId={ID}
//! - Cleanup: GET /{id}?uploads (list all), then abort

use crate::upload::types::{MultipartUploadInfo, PartInfo};

/// Default part size: 100 MiB.
pub const DEFAULT_PART_SIZE: u64 = 100 * 1024 * 1024;

/// Minimum part size per S3 spec: 5 MiB (except last part).
pub const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

// ── XML parsing helpers ─────────────────────────────────────────────────────
//
// S3 returns XML for multipart operations. We use simple string matching
// (consistent with s3_error.rs) since the XML shapes are well-defined.

/// Extract text between `<Tag>` and `</Tag>`.
fn extract_xml_field(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

/// Extract all occurrences of `<Tag>...</Tag>` blocks.
fn extract_xml_blocks<'a>(body: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut search_from = 0;
    while let Some(start) = body[search_from..].find(&open) {
        let abs_start = search_from + start;
        let content_start = abs_start + open.len();
        if let Some(end) = body[content_start..].find(&close) {
            let abs_end = content_start + end + close.len();
            blocks.push(&body[abs_start..abs_end]);
            search_from = abs_end;
        } else {
            break;
        }
    }
    blocks
}

/// Parse the UploadId from an InitiateMultipartUpload response.
///
/// Example XML:
/// ```xml
/// <InitiateMultipartUploadResult>
///   <Bucket>my-item</Bucket>
///   <Key>file.zip</Key>
///   <UploadId>abc123</UploadId>
/// </InitiateMultipartUploadResult>
/// ```
pub fn parse_initiate_response(body: &str) -> Option<String> {
    extract_xml_field(body, "UploadId")
}

/// Parse the list of in-progress multipart uploads for an item.
///
/// Example XML:
/// ```xml
/// <ListMultipartUploadsResult>
///   <Upload>
///     <Key>file.zip</Key>
///     <UploadId>abc123</UploadId>
///     <Initiated>2026-03-06T12:00:00.000Z</Initiated>
///   </Upload>
/// </ListMultipartUploadsResult>
/// ```
pub fn parse_list_uploads_response(body: &str) -> Vec<MultipartUploadInfo> {
    extract_xml_blocks(body, "Upload")
        .into_iter()
        .filter_map(|block| {
            let key = extract_xml_field(block, "Key")?;
            let upload_id = extract_xml_field(block, "UploadId")?;
            let initiated = extract_xml_field(block, "Initiated").unwrap_or_default();
            Some(MultipartUploadInfo {
                key,
                upload_id,
                initiated,
            })
        })
        .collect()
}

/// Parse the list of completed parts for a multipart upload.
///
/// Example XML:
/// ```xml
/// <ListPartsResult>
///   <Part>
///     <PartNumber>1</PartNumber>
///     <ETag>"abc123"</ETag>
///     <Size>104857600</Size>
///   </Part>
/// </ListPartsResult>
/// ```
pub fn parse_list_parts_response(body: &str) -> Vec<PartInfo> {
    extract_xml_blocks(body, "Part")
        .into_iter()
        .filter_map(|block| {
            let part_number: u32 = extract_xml_field(block, "PartNumber")?.parse().ok()?;
            let etag = extract_xml_field(block, "ETag")?;
            let size: u64 = extract_xml_field(block, "Size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            Some(PartInfo {
                part_number,
                etag,
                size,
            })
        })
        .collect()
}

/// Build the XML manifest for CompleteMultipartUpload.
///
/// Output:
/// ```xml
/// <CompleteMultipartUpload>
///   <Part><PartNumber>1</PartNumber><ETag>"abc"</ETag></Part>
///   <Part><PartNumber>2</PartNumber><ETag>"def"</ETag></Part>
/// </CompleteMultipartUpload>
/// ```
pub fn build_complete_manifest(parts: &[(u32, String)]) -> String {
    let mut xml = String::from("<CompleteMultipartUpload>");
    for (num, etag) in parts {
        xml.push_str(&format!(
            "<Part><PartNumber>{num}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    xml.push_str("</CompleteMultipartUpload>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_xml_field --

    #[test]
    fn extract_field_basic() {
        let xml = "<Root><UploadId>abc123</UploadId></Root>";
        assert_eq!(extract_xml_field(xml, "UploadId").unwrap(), "abc123");
    }

    #[test]
    fn extract_field_with_whitespace() {
        let xml = "<Root>\n  <UploadId> abc123 </UploadId>\n</Root>";
        assert_eq!(extract_xml_field(xml, "UploadId").unwrap(), "abc123");
    }

    #[test]
    fn extract_field_missing() {
        let xml = "<Root><Other>value</Other></Root>";
        assert!(extract_xml_field(xml, "UploadId").is_none());
    }

    // -- extract_xml_blocks --

    #[test]
    fn extract_blocks_multiple() {
        let xml = "<Root><Item>a</Item><Item>b</Item><Item>c</Item></Root>";
        let blocks = extract_xml_blocks(xml, "Item");
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].contains("a"));
        assert!(blocks[2].contains("c"));
    }

    #[test]
    fn extract_blocks_none() {
        let xml = "<Root><Other>a</Other></Root>";
        let blocks = extract_xml_blocks(xml, "Item");
        assert!(blocks.is_empty());
    }

    // -- parse_initiate_response --

    #[test]
    fn parse_initiate_success() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult>
  <Bucket>my-item</Bucket>
  <Key>file.zip</Key>
  <UploadId>VXBsb2FkIElEIGZvciBlbG</UploadId>
</InitiateMultipartUploadResult>"#;
        assert_eq!(
            parse_initiate_response(xml).unwrap(),
            "VXBsb2FkIElEIGZvciBlbG"
        );
    }

    #[test]
    fn parse_initiate_not_xml() {
        assert!(parse_initiate_response("not xml").is_none());
    }

    // -- parse_list_uploads_response --

    #[test]
    fn parse_list_uploads_multiple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListMultipartUploadsResult>
  <Bucket>my-item</Bucket>
  <Upload>
    <Key>file1.zip</Key>
    <UploadId>upload-1</UploadId>
    <Initiated>2026-03-06T12:00:00.000Z</Initiated>
  </Upload>
  <Upload>
    <Key>file2.zip</Key>
    <UploadId>upload-2</UploadId>
    <Initiated>2026-03-06T13:00:00.000Z</Initiated>
  </Upload>
</ListMultipartUploadsResult>"#;
        let uploads = parse_list_uploads_response(xml);
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[0].key, "file1.zip");
        assert_eq!(uploads[0].upload_id, "upload-1");
        assert_eq!(uploads[1].key, "file2.zip");
    }

    #[test]
    fn parse_list_uploads_empty() {
        let xml = r#"<ListMultipartUploadsResult>
  <Bucket>my-item</Bucket>
</ListMultipartUploadsResult>"#;
        let uploads = parse_list_uploads_response(xml);
        assert!(uploads.is_empty());
    }

    // -- parse_list_parts_response --

    #[test]
    fn parse_list_parts_multiple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListPartsResult>
  <Bucket>my-item</Bucket>
  <Key>file.zip</Key>
  <UploadId>abc123</UploadId>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>"etag1"</ETag>
    <Size>104857600</Size>
  </Part>
  <Part>
    <PartNumber>2</PartNumber>
    <ETag>"etag2"</ETag>
    <Size>52428800</Size>
  </Part>
</ListPartsResult>"#;
        let parts = parse_list_parts_response(xml);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part_number, 1);
        assert_eq!(parts[0].etag, "\"etag1\"");
        assert_eq!(parts[0].size, 104857600);
        assert_eq!(parts[1].part_number, 2);
        assert_eq!(parts[1].size, 52428800);
    }

    #[test]
    fn parse_list_parts_empty() {
        let xml = "<ListPartsResult></ListPartsResult>";
        assert!(parse_list_parts_response(xml).is_empty());
    }

    // -- build_complete_manifest --

    #[test]
    fn build_manifest_single_part() {
        let parts = vec![(1, "\"etag1\"".to_string())];
        let xml = build_complete_manifest(&parts);
        assert_eq!(
            xml,
            "<CompleteMultipartUpload>\
             <Part><PartNumber>1</PartNumber><ETag>\"etag1\"</ETag></Part>\
             </CompleteMultipartUpload>"
        );
    }

    #[test]
    fn build_manifest_multiple_parts() {
        let parts = vec![
            (1, "\"etag1\"".to_string()),
            (2, "\"etag2\"".to_string()),
            (3, "\"etag3\"".to_string()),
        ];
        let xml = build_complete_manifest(&parts);
        assert!(xml.starts_with("<CompleteMultipartUpload>"));
        assert!(xml.ends_with("</CompleteMultipartUpload>"));
        assert!(xml.contains("<PartNumber>2</PartNumber>"));
        assert_eq!(xml.matches("<Part>").count(), 3);
    }

    // -- constants --

    #[test]
    fn default_part_size_is_100mib() {
        assert_eq!(DEFAULT_PART_SIZE, 100 * 1024 * 1024);
    }

    #[test]
    fn min_part_size_is_5mib() {
        assert_eq!(MIN_PART_SIZE, 5 * 1024 * 1024);
    }
}
```

### Step 2: Register module in `mod.rs`

Add `pub mod multipart;` to `ia-core/src/upload/mod.rs` (after the `mod single;` line).

Also add to the re-exports:
```rust
pub use types::{
    MultipartUploadInfo, PartInfo,
    UploadOpts, UploadOptsBuilder, UploadProgress, UploadProgressStatus, UploadResult,
    UploadStatus,
};
```

### Step 3: Run tests

Run: `cargo test -p ia-core -- multipart`
Expected: ALL PASS (12+ new tests)

### Step 4: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 5: Commit

```
feat(upload): add multipart XML parsing and manifest builder

Add upload/multipart.rs with S3 XML response parsers for
initiate, list-uploads, list-parts, and a manifest builder for
CompleteMultipartUpload. Uses string-matching (consistent with
s3_error.rs).

Part of #214.
```

---

## Task 3: Multipart S3 Operations (Initiate, Upload Part, Complete, Abort)

Implement the four core S3 HTTP operations as async functions.

**Files:**
- Modify: `ia-core/src/upload/multipart.rs`
- Create: `ia-core/tests/upload_multipart.rs`

### Step 1: Write wiremock integration tests for initiate

Create `ia-core/tests/upload_multipart.rs`:

```rust
//! Integration tests for multipart upload operations.

use ia_core::upload::multipart;
use ia_core::{IaClient, IaConfig};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Create an `IaClient` pointed at a wiremock server with S3 credentials.
fn test_client(server: &MockServer) -> IaClient {
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = host_port;
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

// ── Initiate ────────────────────────────────────────────────────────────

#[tokio::test]
async fn initiate_upload_success() {
    let server = MockServer::start().await;

    let resp_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult>
  <Bucket>test-item</Bucket>
  <Key>large-file.zip</Key>
  <UploadId>upload-id-123</UploadId>
</InitiateMultipartUploadResult>"#;

    Mock::given(method("POST"))
        .and(path("/test-item/large-file.zip"))
        .and(query_param("uploads", ""))
        .and(header("authorization", "LOW test-access:test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string(resp_xml))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let upload_id = multipart::initiate_upload(&client, "test-item", "large-file.zip")
        .await
        .unwrap();
    assert_eq!(upload_id, "upload-id-123");
}

#[tokio::test]
async fn initiate_upload_403_fails() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(403).set_body_string(
                "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
            ),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = multipart::initiate_upload(&client, "test-item", "file.zip").await;
    assert!(result.is_err());
}

// ── Upload Part ─────────────────────────────────────────────────────────

#[tokio::test]
async fn upload_part_success() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/file.zip"))
        .and(query_param("partNumber", "1"))
        .and(query_param("uploadId", "upload-123"))
        .and(header("authorization", "LOW test-access:test-secret"))
        .and(header("content-length", "11"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("ETag", "\"etag-part1\""),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let etag = multipart::upload_part(
        &client,
        "test-item",
        "file.zip",
        "upload-123",
        1,
        b"hello world".to_vec(),
    )
    .await
    .unwrap();
    assert_eq!(etag, "\"etag-part1\"");
}

#[tokio::test]
async fn upload_part_missing_etag_fails() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/file.zip"))
        .and(query_param("partNumber", "1"))
        .and(query_param("uploadId", "upload-123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = multipart::upload_part(
        &client,
        "test-item",
        "file.zip",
        "upload-123",
        1,
        b"data".to_vec(),
    )
    .await;
    assert!(result.is_err());
}

// ── Complete ────────────────────────────────────────────────────────────

#[tokio::test]
async fn complete_upload_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploadId", "upload-123"))
        .and(header("x-archive-keep-old-version", "1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = vec![
        (1, "\"etag1\"".to_string()),
        (2, "\"etag2\"".to_string()),
    ];
    let result =
        multipart::complete_upload(&client, "test-item", "file.zip", "upload-123", &parts, true)
            .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn complete_upload_without_backup() {
    let server = MockServer::start().await;

    // Expect NO x-archive-keep-old-version header
    Mock::given(method("POST"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploadId", "upload-123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = vec![(1, "\"etag1\"".to_string())];
    let result =
        multipart::complete_upload(&client, "test-item", "file.zip", "upload-123", &parts, false)
            .await;
    assert!(result.is_ok());
}

// ── Abort ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn abort_upload_success() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploadId", "upload-123"))
        .and(header("authorization", "LOW test-access:test-secret"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result =
        multipart::abort_upload(&client, "test-item", "file.zip", "upload-123").await;
    assert!(result.is_ok());
}

// ── List Uploads ────────────────────────────────────────────────────────

#[tokio::test]
async fn list_uploads_success() {
    let server = MockServer::start().await;

    let xml = r#"<ListMultipartUploadsResult>
  <Upload>
    <Key>file1.zip</Key>
    <UploadId>upload-1</UploadId>
    <Initiated>2026-03-06T12:00:00.000Z</Initiated>
  </Upload>
  <Upload>
    <Key>file2.zip</Key>
    <UploadId>upload-2</UploadId>
    <Initiated>2026-03-06T13:00:00.000Z</Initiated>
  </Upload>
</ListMultipartUploadsResult>"#;

    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .and(header("authorization", "LOW test-access:test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let uploads = multipart::list_uploads(&client, "test-item").await.unwrap();
    assert_eq!(uploads.len(), 2);
    assert_eq!(uploads[0].key, "file1.zip");
    assert_eq!(uploads[1].upload_id, "upload-2");
}

#[tokio::test]
async fn list_uploads_empty() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let uploads = multipart::list_uploads(&client, "test-item").await.unwrap();
    assert!(uploads.is_empty());
}

// ── List Parts ──────────────────────────────────────────────────────────

#[tokio::test]
async fn list_parts_success() {
    let server = MockServer::start().await;

    let xml = r#"<ListPartsResult>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>"etag1"</ETag>
    <Size>104857600</Size>
  </Part>
  <Part>
    <PartNumber>2</PartNumber>
    <ETag>"etag2"</ETag>
    <Size>52428800</Size>
  </Part>
</ListPartsResult>"#;

    Mock::given(method("GET"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploadId", "upload-123"))
        .and(header("authorization", "LOW test-access:test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = multipart::list_parts(&client, "test-item", "file.zip", "upload-123")
        .await
        .unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].part_number, 1);
    assert_eq!(parts[0].etag, "\"etag1\"");
    assert_eq!(parts[1].size, 52428800);
}
```

### Step 2: Run tests to verify they fail

Run: `cargo test -p ia-core --test upload_multipart`
Expected: compile error — functions not yet defined.

### Step 3: Implement S3 operations in `multipart.rs`

Add to `multipart.rs` (after the XML parsing functions, before `#[cfg(test)]`):

```rust
use crate::error::{IaError, Result};
use crate::upload::s3_error::parse_s3_error;
use crate::IaClient;

/// Build the S3 URL for multipart operations.
///
/// Same logic as `single.rs::build_s3_url` but public within the upload module.
fn build_s3_url(client: &IaClient, identifier: &str, key: &str) -> String {
    let encoded_key = key
        .split('/')
        .map(|seg| urlencoding::encode(seg))
        .collect::<Vec<_>>()
        .join("/");
    let protocol = client.protocol();
    let host = client.host();
    if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{identifier}/{encoded_key}")
    } else {
        format!("{protocol}://{host}/{identifier}/{encoded_key}")
    }
}

/// Build the S3 URL for item-level operations (list uploads).
fn build_s3_item_url(client: &IaClient, identifier: &str) -> String {
    let protocol = client.protocol();
    let host = client.host();
    if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{identifier}")
    } else {
        format!("{protocol}://{host}/{identifier}")
    }
}

/// Initiate a multipart upload. Returns the server-assigned upload ID.
///
/// `POST /{identifier}/{key}?uploads`
pub async fn initiate_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
) -> Result<String> {
    let (access, secret) = client.require_auth()?;
    let url = format!("{}?uploads=", build_s3_url(client, identifier, key));

    let resp = client
        .http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Length", "0")
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("initiate multipart: {e}"),
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {body}"));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("initiate multipart failed: {msg}"),
        });
    }

    parse_initiate_response(&body).ok_or_else(|| IaError::UploadFailed {
        identifier: identifier.into(),
        key: key.into(),
        message: "initiate response missing UploadId".into(),
    })
}

/// Upload a single part. Returns the ETag from the response.
///
/// `PUT /{identifier}/{key}?partNumber={N}&uploadId={ID}`
pub async fn upload_part(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    part_number: u32,
    body: Vec<u8>,
) -> Result<String> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?partNumber={}&uploadId={}",
        build_s3_url(client, identifier, key),
        part_number,
        upload_id,
    );
    let content_length = body.len();

    let resp = client
        .http()
        .put(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Length", content_length.to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("upload part {part_number}: {e}"),
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        let msg = parse_s3_error(&body_text)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {body_text}"));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("upload part {part_number} failed: {msg}"),
        });
    }

    // Extract ETag from response headers
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("upload part {part_number}: missing ETag in response"),
        })
}

/// Complete a multipart upload by sending the manifest.
///
/// `POST /{identifier}/{key}?uploadId={ID}` with XML body
pub async fn complete_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    parts: &[(u32, String)],
    keep_old_version: bool,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let manifest = build_complete_manifest(parts);
    let mut req = client
        .http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Type", "application/xml")
        .header("Content-Length", manifest.len().to_string());

    if keep_old_version {
        req = req.header("x-archive-keep-old-version", "1");
    }

    let resp = req
        .body(manifest)
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("complete multipart: {e}"),
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {body}"));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("complete multipart failed: {msg}"),
        });
    }
    Ok(())
}

/// Abort a multipart upload.
///
/// `DELETE /{identifier}/{key}?uploadId={ID}`
pub async fn abort_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let resp = client
        .http()
        .delete(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::MultipartAborted {
            identifier: identifier.into(),
            key: key.into(),
        })?;

    let status = resp.status();
    if !status.is_success() && status.as_u16() != 204 {
        let body = resp.text().await.unwrap_or_default();
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("abort multipart failed: HTTP {status}: {body}"),
        });
    }
    Ok(())
}

/// List all in-progress multipart uploads for an item.
///
/// `GET /{identifier}?uploads`
pub async fn list_uploads(
    client: &IaClient,
    identifier: &str,
) -> Result<Vec<MultipartUploadInfo>> {
    let (access, secret) = client.require_auth()?;
    let url = format!("{}?uploads=", build_s3_item_url(client, identifier));

    let resp = client
        .http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: String::new(),
            message: format!("list multipart uploads: {e}"),
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: String::new(),
            message: format!("list multipart uploads: HTTP {status}: {body}"),
        });
    }

    Ok(parse_list_uploads_response(&body))
}

/// List completed parts for a multipart upload.
///
/// `GET /{identifier}/{key}?uploadId={ID}`
pub async fn list_parts(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
) -> Result<Vec<PartInfo>> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let resp = client
        .http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("list parts: {e}"),
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("list parts: HTTP {status}: {body}"),
        });
    }

    Ok(parse_list_parts_response(&body))
}
```

### Step 4: Run integration tests

Run: `cargo test -p ia-core --test upload_multipart`
Expected: ALL PASS (10+ tests)

### Step 5: Run all tests

Run: `cargo test -p ia-core`
Expected: ALL PASS

### Step 6: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 7: Commit

```
feat(upload): implement multipart S3 operations

Add initiate_upload, upload_part, complete_upload, abort_upload,
list_uploads, and list_parts functions for the S3 multipart protocol.
All operations tested with wiremock.

Part of #214.
```

---

## Task 4: Full Multipart Upload Flow (upload_file_multipart)

The orchestrator that ties initiate → split → upload parts → complete together,
with per-part retry and progress reporting.

**Files:**
- Modify: `ia-core/src/upload/multipart.rs`
- Modify: `ia-core/tests/upload_multipart.rs`

### Step 1: Write integration test for full multipart flow

Add to `ia-core/tests/upload_multipart.rs`:

```rust
use ia_core::upload::{UploadOpts, UploadStatus};
use std::io::Write;
use tempfile::NamedTempFile;

/// Helper to create a temp file with given content.
fn temp_file(content: &[u8]) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content).unwrap();
    f.flush().unwrap();
    f
}

// ── Full multipart flow ─────────────────────────────────────────────────

#[tokio::test]
async fn upload_file_multipart_success() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    // File: 15 bytes, part size 10 → 2 parts (10 + 5)
    let content = b"hello world!!!!";
    let f = temp_file(content);

    // 1. Initiate
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-123</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    // 2. Part 1
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .and(query_param("uploadId", "mp-123"))
        .and(header("content-length", "10"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .expect(1)
        .mount(&server)
        .await;

    // 3. Part 2
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .and(query_param("uploadId", "mp-123"))
        .and(header("content-length", "5"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag2\""))
        .expect(1)
        .mount(&server)
        .await;

    // 4. Complete
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        10, // part_size override for testing
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 15);
    assert_eq!(result.identifier, "test-item");
    assert_eq!(result.key, "data.bin");
}

#[tokio::test]
async fn upload_file_multipart_part_retry_on_503() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let content = b"hello world"; // 11 bytes, 1 part with size >= 11
    let f = temp_file(content);

    // Initiate
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-456</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;

    // Part 1: first attempt 503, second attempt success
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(503).set_body_string("SlowDown"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;

    // Complete
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-456"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_sleep: std::time::Duration::from_millis(1), // fast for tests
        ..Default::default()
    };

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        1024, // single part
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert!(result.retries >= 1);
}

#[tokio::test]
async fn upload_file_multipart_aborts_on_permanent_error() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let f = temp_file(b"data");

    // Initiate
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-789</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;

    // Part 1: permanent 403
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .mount(&server)
        .await;

    // Abort (should be called on permanent failure)
    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-789"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        1024,
        None,
    )
    .await;

    assert!(result.is_err());
}
```

### Step 2: Run tests to verify they fail

Run: `cargo test -p ia-core --test upload_multipart -- upload_file_multipart`
Expected: compile error — `upload_file_multipart` not defined.

### Step 3: Implement `upload_file_multipart`

Add to `multipart.rs` (after `list_parts`, before `#[cfg(test)]`):

```rust
use crate::upload::check_limit::is_spam_response;
use crate::upload::types::{
    UploadOpts, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};
use std::path::Path;
use std::time::Instant;

/// Upload a file using the S3 multipart protocol.
///
/// Flow: initiate → split into parts → upload each part → complete.
/// On permanent part failure, aborts the upload (best-effort cleanup).
/// Retries individual parts on transient errors.
///
/// `part_size` controls the split size. Use `DEFAULT_PART_SIZE` for production.
/// A smaller value can be passed for testing.
pub async fn upload_file_multipart(
    client: &IaClient,
    identifier: &str,
    file: &Path,
    key: &str,
    opts: &UploadOpts,
    part_size: u64,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<UploadResult> {
    let start = Instant::now();
    let file_size = tokio::fs::metadata(file).await?.len();

    // Report verifying phase
    if let Some(cb) = progress {
        cb(UploadProgress {
            identifier: identifier.into(),
            key: key.into(),
            bytes_sent: 0,
            total_bytes: file_size,
            status: UploadProgressStatus::Verifying,
        });
    }

    // Initiate the multipart upload
    let upload_id = initiate_upload(client, identifier, key).await?;

    // Compute part boundaries
    let part_count = file_size.div_ceil(part_size).max(1) as u32;
    let mut completed_parts: Vec<(u32, String)> = Vec::with_capacity(part_count as usize);
    let mut total_retries = 0u32;

    // Upload each part
    for part_num in 1..=part_count {
        let offset = (part_num as u64 - 1) * part_size;
        let this_part_size = std::cmp::min(part_size, file_size - offset) as usize;

        // Read part data from file
        let data = read_file_range(file, offset, this_part_size).await?;

        // Report progress
        if let Some(cb) = progress {
            cb(UploadProgress {
                identifier: identifier.into(),
                key: key.into(),
                bytes_sent: offset,
                total_bytes: file_size,
                status: UploadProgressStatus::Uploading,
            });
        }

        // Per-part retry loop
        let mut part_retries = 0u32;
        let etag = loop {
            match upload_part(client, identifier, key, &upload_id, part_num, data.clone())
                .await
            {
                Ok(etag) => break etag,
                Err(e) => {
                    // Check if retryable
                    let is_retryable = matches!(
                        &e,
                        IaError::UploadFailed { message, .. }
                            if message.contains("503")
                                || message.contains("SlowDown")
                                || message.contains("InternalError")
                                || message.contains("ServiceUnavailable")
                    );

                    if is_retryable && part_retries < opts.retries {
                        part_retries += 1;
                        total_retries += 1;

                        if let Some(cb) = progress {
                            cb(UploadProgress {
                                identifier: identifier.into(),
                                key: key.into(),
                                bytes_sent: offset,
                                total_bytes: file_size,
                                status: UploadProgressStatus::WaitingRateLimit,
                            });
                        }

                        tokio::time::sleep(opts.retry_sleep).await;
                        continue;
                    }

                    // Non-retryable or retries exhausted: abort the upload
                    tracing::warn!(
                        identifier,
                        key,
                        part_num,
                        "multipart part failed, aborting upload"
                    );
                    let _ = abort_upload(client, identifier, key, &upload_id).await;
                    return Err(e);
                }
            }
        };

        completed_parts.push((part_num, etag));
    }

    // Complete the multipart upload
    let keep_old_version = !opts.no_backup;
    complete_upload(
        client,
        identifier,
        key,
        &upload_id,
        &completed_parts,
        keep_old_version,
    )
    .await?;

    // Report completion
    if let Some(cb) = progress {
        cb(UploadProgress {
            identifier: identifier.into(),
            key: key.into(),
            bytes_sent: file_size,
            total_bytes: file_size,
            status: UploadProgressStatus::Complete,
        });
    }

    // Delete local file if requested
    if opts.delete_after_upload {
        if let Err(e) = tokio::fs::remove_file(file).await {
            tracing::warn!("failed to delete {} after upload: {e}", file.display());
        }
    }

    Ok(UploadResult {
        identifier: identifier.into(),
        key: key.into(),
        status: UploadStatus::Uploaded,
        bytes: file_size,
        md5: None,
        elapsed_ms: start.elapsed().as_millis() as u64,
        retries: total_retries,
    })
}

/// Read a range of bytes from a file.
async fn read_file_range(file: &Path, offset: u64, len: usize) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(file).await?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).await?;
    Ok(buf)
}
```

### Step 4: Run multipart integration tests

Run: `cargo test -p ia-core --test upload_multipart`
Expected: ALL PASS

### Step 5: Run all tests

Run: `cargo test -p ia-core`
Expected: ALL PASS

### Step 6: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 7: Commit

```
feat(upload): implement multipart upload orchestrator

Add upload_file_multipart() that orchestrates the full S3 multipart
flow: initiate → split into parts → upload each part with retry →
complete. On permanent part failure, aborts the upload for cleanup.

Part of #214.
```

---

## Task 5: Resume Support

Automatically detect and resume incomplete multipart uploads.

**Files:**
- Modify: `ia-core/src/upload/multipart.rs`
- Modify: `ia-core/tests/upload_multipart.rs`

### Step 1: Write integration test for resume

Add to `ia-core/tests/upload_multipart.rs`:

```rust
#[tokio::test]
async fn upload_file_multipart_resumes_from_existing() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    // File: 30 bytes, part size 10 → 3 parts
    let content = b"aaaaabbbbbcccccdddddeeeeeffffff";
    assert_eq!(content.len(), 30);
    let f = temp_file(content);

    // 1. List uploads: finds existing upload for this key
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListMultipartUploadsResult>
  <Upload>
    <Key>data.bin</Key>
    <UploadId>resume-123</UploadId>
    <Initiated>2026-03-06T12:00:00.000Z</Initiated>
  </Upload>
</ListMultipartUploadsResult>"#,
        ))
        .mount(&server)
        .await;

    // 2. List parts: part 1 already uploaded
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "resume-123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>"existing-etag1"</ETag>
    <Size>10</Size>
  </Part>
</ListPartsResult>"#,
        ))
        .mount(&server)
        .await;

    // 3. Should NOT initiate a new upload (no POST ?uploads mock)

    // 4. Parts 2 and 3 uploaded (part 1 skipped)
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .and(query_param("uploadId", "resume-123"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag2\""))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "3"))
        .and(query_param("uploadId", "resume-123"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag3\""))
        .expect(1)
        .mount(&server)
        .await;

    // 5. Complete
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "resume-123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        10, // small parts for testing
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 30);
}

#[tokio::test]
async fn upload_file_multipart_no_resume_starts_fresh() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let f = temp_file(b"hello");

    // List uploads: empty (no existing uploads)
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult></ListMultipartUploadsResult>",
        ))
        .mount(&server)
        .await;

    // Falls through to fresh initiate
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>fresh-123</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"e1\""))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "fresh-123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        1024,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}
```

### Step 2: Run tests to verify they fail

Run: `cargo test -p ia-core --test upload_multipart -- resume`
Expected: FAIL (upload_file_multipart doesn't check for existing uploads yet).

### Step 3: Add resume logic to `upload_file_multipart`

Replace the initiation section of `upload_file_multipart` with resume-aware logic. The beginning of the function (after progress reporting) becomes:

```rust
    // Try to resume an existing upload
    let (upload_id, existing_parts) =
        try_resume(client, identifier, key).await?;

    let (upload_id, mut completed_parts) = match upload_id {
        Some(id) => {
            tracing::info!(
                identifier,
                key,
                upload_id = %id,
                existing_parts = existing_parts.len(),
                "resuming multipart upload"
            );
            let parts: Vec<(u32, String)> = existing_parts
                .into_iter()
                .map(|p| (p.part_number, p.etag))
                .collect();
            (id, parts)
        }
        None => {
            let id = initiate_upload(client, identifier, key).await?;
            (id, Vec::new())
        }
    };
```

And add a skip check in the part upload loop:

```rust
    for part_num in 1..=part_count {
        // Skip already-completed parts (resume)
        if completed_parts.iter().any(|(n, _)| *n == part_num) {
            continue;
        }

        // ... rest of part upload logic
```

And add the `try_resume` helper function:

```rust
/// Check for an existing in-progress upload for this key and return it.
///
/// If multiple uploads exist for the same key, returns the most recent one.
async fn try_resume(
    client: &IaClient,
    identifier: &str,
    key: &str,
) -> Result<(Option<String>, Vec<PartInfo>)> {
    let uploads = list_uploads(client, identifier).await?;

    // Find uploads matching this key, take the most recent
    let matching: Option<&MultipartUploadInfo> = uploads
        .iter()
        .filter(|u| u.key == key)
        .last(); // last = most recent (S3 returns chronological order)

    match matching {
        Some(info) => {
            let parts = list_parts(client, identifier, key, &info.upload_id).await?;
            Ok((Some(info.upload_id.clone()), parts))
        }
        None => Ok((None, Vec::new())),
    }
}
```

### Step 4: Run resume tests

Run: `cargo test -p ia-core --test upload_multipart -- resume`
Expected: ALL PASS

### Step 5: Run all tests

Run: `cargo test -p ia-core`
Expected: ALL PASS

### Step 6: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 7: Commit

```
feat(upload): add automatic multipart resume

Before initiating a new multipart upload, check the server for
existing in-progress uploads for the same key. If found, list
completed parts and skip them, uploading only missing parts.
No local state file needed — all state lives on the server.

Closes #215.
```

---

## Task 6: Dispatch in single.rs and Builder Wiring

Replace the Phase 2 guard with actual multipart dispatch. Wire the `multipart`
field through the builder.

**Files:**
- Modify: `ia-core/src/upload/single.rs`
- Modify: `ia-core/src/upload/types.rs`
- Modify: `ia-core/tests/upload_single.rs`

### Step 1: Write test for multipart dispatch

Add to `ia-core/tests/upload_single.rs`:

```rust
#[tokio::test]
async fn upload_file_multipart_flag_dispatches() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let content = b"test multipart dispatch";
    let f = temp_file(content);

    // List uploads (resume check): empty
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(wiremock::matchers::query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult></ListMultipartUploadsResult>",
        ))
        .mount(&server)
        .await;

    // Initiate
    Mock::given(method("POST"))
        .and(path("/test-item/file.bin"))
        .and(wiremock::matchers::query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>dispatch-test</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;

    // Part 1
    Mock::given(method("PUT"))
        .and(path("/test-item/file.bin"))
        .and(wiremock::matchers::query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"e1\""))
        .mount(&server)
        .await;

    // Complete
    Mock::given(method("POST"))
        .and(path("/test-item/file.bin"))
        .and(wiremock::matchers::query_param("uploadId", "dispatch-test"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let opts = UploadOpts {
        multipart: true,
        verify: false,
        ..Default::default()
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.bin",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}
```

### Step 2: Run test to verify it fails

Run: `cargo test -p ia-core --test upload_single -- multipart_flag`
Expected: FAIL — currently returns `Config("multipart upload is not yet implemented")`.

### Step 3: Replace guard in `single.rs`

In `ia-core/src/upload/single.rs`, replace lines 47-51:

```rust
if opts.multipart {
    return Err(IaError::Config(
        "multipart upload is not yet implemented (Phase 2)".into(),
    ));
}
```

With:

```rust
if opts.multipart {
    return crate::upload::multipart::upload_file_multipart(
        client,
        identifier,
        file,
        key,
        opts,
        crate::upload::multipart::DEFAULT_PART_SIZE,
        progress,
    )
    .await;
}
```

### Step 4: Add `multipart` setter to `UploadOptsBuilder`

Add to `ia-core/src/upload/types.rs` in the `UploadOptsBuilder` impl:

```rust
/// Set whether to use multipart upload.
pub fn multipart(mut self, multipart: bool) -> Self {
    self.opts.multipart = multipart;
    self
}
```

### Step 5: Run tests

Run: `cargo test -p ia-core --test upload_single -- multipart_flag`
Expected: PASS

Run: `cargo test -p ia-core`
Expected: ALL PASS

### Step 6: Clippy

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: 0 warnings

### Step 7: Commit

```
feat(upload): dispatch to multipart when --multipart is set

Replace the Phase 2 guard in single.rs with actual dispatch to
upload_file_multipart(). Add multipart() setter to UploadOptsBuilder.

Part of #214.
```

---

## Task 7: CLI Wiring — --multipart Flag

Wire the `--multipart` flag from the CLI through to `UploadOpts`.

**Files:**
- Modify: `ia-cli/src/commands/upload.rs`
- Modify: `ia-cli/tests/cli_upload.rs` (or create if not existing)

### Step 1: Add `--multipart` flag to `UploadArgs` and `ImportArgs`

In `ia-cli/src/commands/upload.rs`, add to `UploadArgs` (after the `dashboard` field):

```rust
/// Use multipart upload (recommended for files >5 GB)
#[arg(long)]
pub multipart: bool,
```

Add to `ImportArgs` (after the `dry_run` field):

```rust
/// Use multipart upload (recommended for files >5 GB)
#[arg(long)]
pub multipart: bool,
```

### Step 2: Wire the flag in `run_bare_upload`

In `run_bare_upload`, change line 384 from:

```rust
multipart: false,
```

To:

```rust
multipart: args.multipart,
```

### Step 3: Wire the flag in `run_import`

In `run_import`, the `UploadOpts` construction uses `..UploadOpts::default()`. Add `multipart: args.multipart` explicitly before the default spread.

### Step 4: Run CLI tests

Run: `cargo test -p ia-cli`
Expected: ALL PASS

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: 0 warnings

### Step 5: Commit

```
feat(upload): wire --multipart CLI flag

Add --multipart flag to `ia upload` and `ia upload import` commands.
Passes through to UploadOpts.multipart for dispatch to multipart
upload path.

Part of #214.
```

---

## Task 8: Cleanup Subcommand

Unhide and implement the `ia upload cleanup` subcommand.

**Files:**
- Modify: `ia-cli/src/commands/upload.rs`
- Modify: `ia-core/src/upload/mod.rs` (re-export)

### Step 1: Add `--abort-all` and `--json` to `CleanupArgs`

In `ia-cli/src/commands/upload.rs`, update `CleanupArgs`:

```rust
#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// Item identifier
    #[arg()]
    pub identifier: String,

    /// Specific file to clean up
    #[arg()]
    pub file: Option<String>,

    /// Abort all incomplete uploads without confirmation
    #[arg(long)]
    pub abort_all: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

### Step 2: Unhide the subcommand

Change `#[command(hide = true)]` on the `Cleanup` variant to:

```rust
#[command(
    long_about = "List or abort incomplete multipart uploads for an item. \
        Use this to clean up uploads that were interrupted or abandoned.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># List all incomplete uploads for an item</dim>\
         \n  <bold>$ ia upload cleanup my-item</bold>\
         \n\n  <dim># Abort a specific file's upload</dim>\
         \n  <bold>$ ia upload cleanup my-item file.zip</bold>\
         \n\n  <dim># Abort all incomplete uploads</dim>\
         \n  <bold>$ ia upload cleanup my-item --abort-all</bold>\n"
    ),
)]
```

### Step 3: Implement `run_cleanup`

Replace the stub with:

```rust
async fn run_cleanup(client: &IaClient, args: CleanupArgs) -> Result<()> {
    let uploads = ia_core::upload::multipart::list_uploads(client, &args.identifier).await?;

    if uploads.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!(
                "{} No incomplete multipart uploads for {}",
                style("✓").green(),
                args.identifier,
            );
        }
        return Ok(());
    }

    // Filter by file if specified
    let targets: Vec<_> = if let Some(ref file) = args.file {
        uploads.into_iter().filter(|u| u.key == *file).collect()
    } else {
        uploads
    };

    if targets.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!(
                "{} No incomplete uploads matching '{}' for {}",
                style("✓").green(),
                args.file.as_deref().unwrap_or(""),
                args.identifier,
            );
        }
        return Ok(());
    }

    // List mode: no file and no --abort-all → just list
    if args.file.is_none() && !args.abort_all {
        if args.json {
            let json = serde_json::to_string(&targets)?;
            println!("{json}");
        } else {
            eprintln!(
                "{} {} incomplete multipart upload(s) for {}:",
                style("▸").cyan(),
                targets.len(),
                args.identifier,
            );
            for u in &targets {
                eprintln!(
                    "  {} {} (initiated: {})",
                    u.upload_id, u.key, u.initiated,
                );
            }
            eprintln!(
                "\nUse --abort-all or specify a file to abort."
            );
        }
        return Ok(());
    }

    // Abort mode
    for u in &targets {
        ia_core::upload::multipart::abort_upload(client, &args.identifier, &u.key, &u.upload_id)
            .await?;
        if args.json {
            let json = serde_json::json!({
                "action": "aborted",
                "identifier": args.identifier,
                "key": u.key,
                "upload_id": u.upload_id,
            });
            println!("{}", serde_json::to_string(&json)?);
        } else {
            eprintln!(
                " {} aborted {}/{}",
                style("✓").green(),
                args.identifier,
                u.key,
            );
        }
    }

    Ok(())
}
```

### Step 4: Update `run` to pass client to cleanup

In the `run` function, change the `Cleanup` arm from:

```rust
Some(UploadCommand::Cleanup(sub)) => run_cleanup(sub),
```

To:

```rust
Some(UploadCommand::Cleanup(sub)) => run_cleanup(client, sub).await,
```

### Step 5: Run tests

Run: `cargo test -p ia-cli`
Expected: ALL PASS

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: 0 warnings

### Step 6: Commit

```
feat(upload): implement cleanup subcommand for multipart uploads

Unhide `ia upload cleanup` and implement list/abort functionality.
Supports listing incomplete multipart uploads, aborting specific
files or all uploads with --abort-all. Includes --json output.

Closes #216.
```

---

## Task 9: CLI Integration Tests

Add assert_cmd tests for the multipart flag acceptance and cleanup.

**Files:**
- Modify: `ia-cli/tests/cli_upload.rs` (or wherever CLI upload tests live)

### Step 1: Find existing CLI test file

Look for existing upload CLI tests:

Run: `ls ia-cli/tests/`

### Step 2: Add flag acceptance tests

Add tests that verify:
- `ia upload --multipart --dry-run test-item file` accepts the flag
- `ia upload import --multipart --dry-run file.csv` accepts the flag
- `ia upload cleanup --help` shows help text (not "not yet implemented")

The exact test code depends on the existing test patterns in the file. Use the
`assert_cmd` pattern from existing tests (the existing `cli_upload.rs` tests
show the pattern).

### Step 3: Run tests

Run: `cargo test -p ia-cli`
Expected: ALL PASS

### Step 4: Commit

```
test(upload): add CLI tests for --multipart flag and cleanup subcommand

Verify flag acceptance and cleanup help text.

Part of #214, #216.
```

---

## Task 10: Module Exports and Documentation

Ensure all new public types are exported and help text is accurate.

**Files:**
- Modify: `ia-core/src/upload/mod.rs`
- Verify: `ia-cli/src/commands/upload.rs` help strings

### Step 1: Update `mod.rs` exports

Ensure `ia-core/src/upload/mod.rs` has:

```rust
pub use types::{
    MultipartUploadInfo, PartInfo,
    UploadOpts, UploadOptsBuilder, UploadProgress, UploadProgressStatus, UploadResult,
    UploadStatus,
};
```

The `multipart` module should remain `pub mod multipart;` (its functions are
individually `pub`).

### Step 2: Run full test suite

Run: `cargo test -p ia-core -p ia-cli`
Expected: ALL PASS

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: 0 warnings

### Step 3: Commit

```
feat(upload): finalize Phase 2 module exports

Export MultipartUploadInfo and PartInfo from ia-core::upload for
external consumers (ia-gui).

Part of #214.
```

---

## Task 11: Pre-PR Verification

Final checks before creating the PR.

### Step 1: Full test suite

Run: `cargo test -p ia-core -p ia-cli`
Expected: ALL PASS

### Step 2: Clippy

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: 0 warnings

### Step 3: Verify git status

Run: `git status`
Expected: No uncommitted files that belong in the PR.

### Step 4: Verify all design/plan docs are committed

Run: `git log --oneline --name-only | head -30`
Verify `docs/plans/2026-03-06-upload-phase2-implementation-plan.md` is committed.

### Step 5: Update MEMORY.md

Update the upload Phase 2 status from "NOT STARTED" to "COMPLETE" with notes:
- Modules: multipart (initiate, upload_part, complete, abort, list_uploads, list_parts, resume, upload_file_multipart)
- CLI: `--multipart` flag, `cleanup` subcommand (unhidden)
- Tests: unit + wiremock integration + CLI

### Step 6: Create PR

PR title: `feat(upload): Phase 2 — multipart upload with resume`

Body should include:
```
## Summary
- Implement S3 multipart upload protocol (initiate, upload parts, complete)
- Automatic resume: detects in-progress uploads and skips completed parts
- `--multipart` flag for `ia upload` and `ia upload import`
- `ia upload cleanup` subcommand (list/abort incomplete uploads)
- Per-part retry with rate limit handling
- Full wiremock test coverage

## Test plan
- [ ] Unit tests for XML parsing, manifest builder, types
- [ ] Wiremock integration tests for all S3 operations
- [ ] Wiremock integration tests for full flow and resume
- [ ] CLI flag acceptance tests
- [ ] `cargo clippy -p ia-core -p ia-cli -- -D warnings` clean

Closes #214
Closes #215
Closes #216
```
