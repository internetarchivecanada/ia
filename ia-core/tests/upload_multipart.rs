//! Integration tests for multipart upload operations.

use ia_core::upload::multipart;
use ia_core::upload::{UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::io::Write;
use tempfile::NamedTempFile;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helper to create a temp file with given content.
fn temp_file(content: &[u8]) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content).unwrap();
    f.flush().unwrap();
    f
}

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
    let upload_id = multipart::initiate_upload(&client, "test-item", "large-file.zip", &[])
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
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = multipart::initiate_upload(&client, "test-item", "file.zip", &[]).await;
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
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag-part1\""))
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
    let parts = vec![(1, "\"etag1\"".to_string()), (2, "\"etag2\"".to_string())];
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
    let result = multipart::complete_upload(
        &client,
        "test-item",
        "file.zip",
        "upload-123",
        &parts,
        false,
    )
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
    let result = multipart::abort_upload(&client, "test-item", "file.zip", "upload-123").await;
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

// ── Full multipart flow ─────────────────────────────────────────────────

#[tokio::test]
async fn upload_file_multipart_success() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    // File: 15 bytes, part size 10 → 2 parts (10 + 5)
    let content = b"hello world!!!!";
    let f = temp_file(content);

    // 0. List uploads (resume check): empty
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;

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
        true,
        true,
        None,
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

    // List uploads (resume check): empty
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;

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
        true,
        true,
        None,
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

    // List uploads (resume check): empty
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;

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
        true,
        true,
        None,
        None,
    )
    .await;

    assert!(result.is_err());
}

// ── Resume ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn upload_file_multipart_resumes_from_existing() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    // File: 30 bytes, part size 10 → 3 parts
    let content = b"aaaaabbbbbcccccdddddeeeeefffff";
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
        true,
        true,
        None,
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
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
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_file_multipart_resume_non_contiguous_parts() {
    // Regression: if parts 1 and 3 are done but 2 is missing, the manifest
    // must still be sorted by part number for S3 CompleteMultipartUpload.
    let server = MockServer::start().await;
    let client = test_client(&server);

    // File: 30 bytes, part size 10 → 3 parts
    let f = temp_file(b"aaaaabbbbbcccccdddddeeeeefffff");

    // List uploads: existing upload
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListMultipartUploadsResult>
  <Upload>
    <Key>data.bin</Key>
    <UploadId>gap-resume</UploadId>
    <Initiated>2026-03-06T12:00:00.000Z</Initiated>
  </Upload>
</ListMultipartUploadsResult>"#,
        ))
        .mount(&server)
        .await;

    // List parts: parts 1 and 3 done, part 2 missing
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "gap-resume"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult>
  <Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>10</Size></Part>
  <Part><PartNumber>3</PartNumber><ETag>"e3"</ETag><Size>10</Size></Part>
</ListPartsResult>"#,
        ))
        .mount(&server)
        .await;

    // Only part 2 should be uploaded
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .and(query_param("uploadId", "gap-resume"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"e2\""))
        .expect(1)
        .mount(&server)
        .await;

    // Complete — verify it's called (manifest must be sorted)
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "gap-resume"))
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
        10,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}
