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
