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
