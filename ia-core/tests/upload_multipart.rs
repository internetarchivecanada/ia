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

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    format!("{:x}", Md5::digest(bytes))
}

/// Create an `IaClient` pointed at a wiremock server with S3 credentials.
///
/// Built with `from_config`, the way production builds one, so every test
/// here observes the real middleware stack. A client without middleware
/// cannot tell whether a retry layer was stacked back onto the upload
/// transport; a test asserting an attempt count would pass regardless.
fn test_client(server: &MockServer) -> IaClient {
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = host_port;
    config.general.secure = false;
    IaClient::from_config(config).unwrap()
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

/// The public wrapper retries with the default budget. It used to get this
/// from the middleware; with that layer gone, a wrapper built with
/// `retries: 0` would compile unchanged for external callers and silently
/// fail on IA's most common response.
#[tokio::test]
async fn public_initiate_upload_retries_a_throttle() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(503).set_body_string(
                "<Error><Code>SlowDown</Code><Message>Reduce rate</Message></Error>",
            ),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/file.zip"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>upload-id-2</UploadId>\
             </InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let result = multipart::initiate_upload(&client, "test-item", "file.zip", &[]).await;

    // Count first: if the unwrap below panics, wiremock only logs a missed
    // `expect` instead of failing the test.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(result.unwrap(), "upload-id-2");
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
async fn upload_part_missing_etag_falls_back_to_body_md5() {
    // IA's S3 returns no ETag header on part PUTs; the completion check
    // compares against the part's MD5, so that is what we must report.
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
    .await
    .unwrap();
    // md5("data") = 8d777f385d3dfec8815d20f7496026dc, quoted like an S3 ETag
    assert_eq!(result, "\"8d777f385d3dfec8815d20f7496026dc\"");
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
        retry_min_delay: std::time::Duration::from_millis(1),
        retry_max_delay: std::time::Duration::from_millis(2), // fast for tests
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
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert!(result.retries >= 1);
}

/// A 503 that carries `Retry-After` is waited out for as long as the
/// server says, not for the backoff's millisecond test bounds.
#[tokio::test]
async fn upload_file_multipart_part_retry_honors_retry_after() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = temp_file(b"hello multipart world");

    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-ra</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "1")
                .set_body_string("SlowDown"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-ra"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_min_delay: std::time::Duration::from_millis(1),
        retry_max_delay: std::time::Duration::from_millis(2),
        ..Default::default()
    };

    let started = std::time::Instant::now();
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
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "Retry-After: 1 was not waited for ({:?})",
        started.elapsed()
    );
}

/// A 429 is a throttle too; its Retry-After is honored like a 503's.
#[tokio::test]
async fn upload_file_multipart_part_429_retry_honors_retry_after() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = temp_file(b"hello multipart world");

    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-429</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "1")
                .set_body_string("Too Many Requests"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-429"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_min_delay: std::time::Duration::from_millis(1),
        retry_max_delay: std::time::Duration::from_millis(2),
        ..Default::default()
    };

    let started = std::time::Instant::now();
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
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "Retry-After: 1 on a 429 was not waited for ({:?})",
        started.elapsed()
    );
    server.verify().await;
}

// ── A failed part leaves the upload on IA (#18) ─────────────────────────

/// Two-part fixture: list_uploads empty, initiate → `mp-keep`, part 1 OK,
/// and a `DELETE ?uploadId=` mock that must never be called.
async fn mount_two_part_upload_with_no_abort(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-keep</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(server)
        .await;
}

fn two_parts() -> NamedTempFile {
    temp_file(&[7u8; 1500])
}

/// A part refused for good (AccessDenied) does not abort the upload: the
/// parts IA holds stay, and the error names the upload ID and both ways
/// forward. (Before #18 this test asserted the abort.)
#[tokio::test]
async fn part_permanent_refusal_leaves_the_upload_for_cleanup() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = two_parts();
    mount_two_part_upload_with_no_abort(&server).await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 3,
        ..Default::default()
    };
    let err = multipart::upload_file_multipart(
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
        None,
    )
    .await
    .expect_err("AccessDenied on part 2 fails the upload");

    let msg = err.to_string();
    for needle in [
        "part 2 of 2",
        "refused",
        "AccessDenied",
        "mp-keep",
        "kept with 1 part",
        "rerun",
        "ia upload cleanup",
    ] {
        assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
    }
    assert!(
        matches!(
            err,
            ia_core::IaError::UploadFailed {
                status: Some(403),
                ..
            }
        ),
        "got {err:?}"
    );
    server.verify().await;
}

/// A part whose transient budget runs out does not abort either: the
/// upload stays for a rerun to resume.
#[tokio::test]
async fn part_exhausted_budget_leaves_the_upload_for_resume() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = two_parts();
    mount_two_part_upload_with_no_abort(&server).await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "0")
                .set_body_string("<Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>"),
        )
        .expect(3)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 2,
        ..Default::default()
    };
    let err = multipart::upload_file_multipart(
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
        None,
    )
    .await
    .expect_err("a spent budget fails the upload");

    let msg = err.to_string();
    for needle in [
        "part 2 of 2",
        "after 3 attempts",
        "SlowDown",
        "mp-keep",
        "kept with 1 part",
        "rerun",
        "ia upload cleanup",
    ] {
        assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
    }
    assert!(
        matches!(
            err,
            ia_core::IaError::UploadFailed {
                status: Some(503),
                ..
            }
        ),
        "got {err:?}"
    );
    server.verify().await;
}

/// IA's spam rejection on a part is permanent and fatal for the whole item
/// (the item loop stops on `SpamDetected`); it must pass through unchanged,
/// not be reworded as a part failure that asks for a rerun. The upload is
/// still not aborted.
#[tokio::test]
async fn spam_rejection_on_a_part_stays_fatal_and_does_not_abort() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = two_parts();
    mount_two_part_upload_with_no_abort(&server).await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            "Upload rejected: this item appears to be spam. Please contact info@archive.org.",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        retries: 3,
        ..Default::default()
    };
    let err = multipart::upload_file_multipart(
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
        None,
    )
    .await
    .expect_err("spam rejection fails the upload");
    assert!(
        matches!(err, ia_core::IaError::SpamDetected { .. }),
        "got {err:?}"
    );
    server.verify().await;
}

/// The rerun after such a failure finds the upload and its part 1 on IA,
/// sends only part 2, and completes.
#[tokio::test]
async fn rerun_after_part_failure_resumes_from_existing_parts() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = two_parts();
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult><Upload><Key>data.bin</Key><UploadId>mp-keep</UploadId>\
             <Initiated>2026-10-02T00:00:00.000Z</Initiated></Upload></ListMultipartUploadsResult>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-keep"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>\"{}\"</ETag><Size>1024</Size></Part></ListPartsResult>",
            md5_hex(&[7u8; 1024])
        )))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "2"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag2\""))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-keep"))
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
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 0);
    server.verify().await;
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
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<ListPartsResult>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>"{}"</ETag>
    <Size>10</Size>
  </Part>
</ListPartsResult>"#,
            md5_hex(b"aaaaabbbbb")
        )))
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
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_file_multipart_new_item_no_such_bucket_starts_fresh() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let f = temp_file(b"hello");

    // List uploads on a brand-new item: IA S3 returns 404 NoSuchBucket.
    // This must be treated as "nothing to resume", not as a failure.
    Mock::given(method("GET"))
        .and(path("/new-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist.</Message></Error>",
        ))
        .mount(&server)
        .await;

    // Falls through to fresh initiate, which carries auto-make-bucket
    Mock::given(method("POST"))
        .and(path("/new-item/data.bin"))
        .and(query_param("uploads", ""))
        .and(header("x-archive-auto-make-bucket", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>fresh-123</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/new-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"e1\""))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/new-item/data.bin"))
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
        "new-item",
        f.path(),
        "data.bin",
        &opts,
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_file_multipart_list_uploads_other_error_still_fails() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let f = temp_file(b"hello");

    // Any error other than NoSuchBucket from the list call is still fatal.
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let err = multipart::upload_file_multipart(
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
        None,
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("AccessDenied"), "{err}");
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
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<ListPartsResult>
  <Part><PartNumber>1</PartNumber><ETag>"{}"</ETag><Size>10</Size></Part>
  <Part><PartNumber>3</PartNumber><ETag>"{}"</ETag><Size>10</Size></Part>
</ListPartsResult>"#,
            md5_hex(b"aaaaabbbbb"),
            md5_hex(b"eeeeefffff")
        )))
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
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
}

// ── part_size validation ────────────────────────────────────────────────

#[tokio::test]
async fn zero_part_size_returns_error_not_panic() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let f = temp_file(b"hello world");

    // Mocks so the pre-validation code path can proceed as far as the part
    // computation if validation is missing (instead of failing earlier on
    // an unmocked request).
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-0</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;

    let result = multipart::upload_file_multipart(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &UploadOpts::default(),
        0, // invalid part_size
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    let err = result.expect_err("part_size=0 must return an error, not panic");
    assert!(
        err.to_string().contains("part_size"),
        "error should mention part_size, got: {err}"
    );
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "validation must reject part_size=0 before any request is sent"
    );
}

// ── One IA-S3 retry policy, shared by every upload request ──────────────────
//
// Retry lives in upload::retry::send_with_retry and classifies on the S3
// error <Code>, not the HTTP status. These pin the three things that follow
// from that: a non-retryable code fails fast even when the status is 5xx, a
// throttle retries, and the attempt count is exactly what was asked for
// rather than multiplied by a second layer.

/// Boilerplate shared by the tests below: resume check empty, initiate ok.
async fn mock_resume_empty_and_initiate(server: &MockServer, upload_id: &str) {
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<InitiateMultipartUploadResult><UploadId>{upload_id}</UploadId></InitiateMultipartUploadResult>"
        )))
        .mount(server)
        .await;
}

fn fast_opts(retries: u32) -> UploadOpts {
    UploadOpts {
        verify: false,
        retries,
        retry_min_delay: std::time::Duration::from_millis(1),
        retry_max_delay: std::time::Duration::from_millis(2),
        ..Default::default()
    }
}

/// A non-retryable S3 code must fail on the first attempt even though the
/// status is 5xx. Classifying on status alone retried this until the budget
/// ran out, which for a 100 MiB part is minutes of pointless transfer.
#[tokio::test]
async fn part_with_non_retryable_code_fails_without_retrying() {
    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-1").await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    // The part fails, so the upload is aborted.
    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(5),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    assert!(result.is_err(), "AccessDenied must not be retried");
}

/// The attempt count is exactly first-try plus `retries`, with no second
/// layer multiplying it.
#[tokio::test]
async fn part_retries_exactly_the_configured_budget() {
    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-2").await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .expect(4) // 1 initial + 3 retries
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let _ = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    server.verify().await;
}

/// A throttled initiate must retry. IA answers throttling with 503 SlowDown,
/// so refusing to retry here makes --multipart fail on IA's most common
/// response while a plain upload survives it.
#[tokio::test]
async fn initiate_retries_on_slowdown_then_succeeds() {
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

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-3</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-3"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("a throttled initiate should retry and succeed");

    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

/// Mount a metadata response for test-item listing the given files.
async fn mock_item_files(server: &MockServer, files: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": files
        })))
        .mount(server)
        .await;
}

/// The completion applied and the response was lost. The retry gets
/// NoSuchUpload, because a completed upload is no longer in progress. A
/// retry alone does not prove that: every retry trigger (a dropped
/// connection, a 503 SlowDown) means the earlier attempt was NOT applied.
/// So NoSuchUpload after a retry is success only when the item's metadata
/// shows the object at the expected size.
#[tokio::test]
async fn complete_no_such_upload_after_a_retry_is_success_when_the_object_exists() {
    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-4").await;
    mock_item_files(
        &server,
        serde_json::json!([{"name": "data.bin", "size": "11", "md5": "5eb63bbbe01eeed093cb22bb8f5acdc3"}]),
    )
    .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;

    // First complete: 503 SlowDown, so it is retried.
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-4"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Retry: the completion had in fact applied, so the upload is gone.
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-4"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchUpload</Code><Message>no such upload</Message></Error>",
        ))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("NoSuchUpload after a retry, with the object present, is a completed upload");

    assert!(matches!(result.status, UploadStatus::Uploaded));
}

/// Same sequence, but the object is not there: the upload id vanished for
/// another reason (expired, or aborted by a concurrent `ia upload cleanup`).
/// Reporting Uploaded here would be a lie, and with --delete-after-upload
/// it would delete the only copy of a file that was never stored.
#[tokio::test]
async fn complete_no_such_upload_after_a_retry_fails_when_the_object_is_missing() {
    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-4b").await;
    mock_item_files(
        &server,
        serde_json::json!([{"name": "other.bin", "size": "3"}]),
    )
    .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-4b"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-4b"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchUpload</Code><Message>no such upload</Message></Error>",
        ))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let mut opts = fast_opts(3);
    opts.delete_after_upload = true;
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    let err = result.expect_err("the object never landed, so this is a failure");
    assert!(
        err.to_string().contains("NoSuchUpload"),
        "error should name the S3 code: {err}"
    );
    assert!(
        f.path().exists(),
        "--delete-after-upload must not delete a file that was never stored"
    );
}

/// On the *first* attempt NoSuchUpload means what it says — an unknown or
/// already-aborted upload id — and must surface. Treating it as success
/// unconditionally would mask a genuine failure as a completed upload.
#[tokio::test]
async fn complete_surfaces_no_such_upload_on_the_first_attempt() {
    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-5").await;

    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-5"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchUpload</Code><Message>no such upload</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "a first-attempt NoSuchUpload is a real failure"
    );
}

// ── Transport-level failures ─────────────────────────────────────────────
//
// wiremock cannot close a socket mid-exchange, so these use a raw listener.

/// A server that reads one full HTTP request from a connection.
async fn read_full_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 8192];
    let mut acc = Vec::new();
    let mut body_len: Option<usize> = None;
    let mut header_end: Option<usize> = None;
    loop {
        let n = stream.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        acc.extend_from_slice(&buf[..n]);
        if header_end.is_none() {
            if let Some(pos) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&acc[..pos]).to_ascii_lowercase();
                body_len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok());
            }
        }
        if let (Some(he), Some(bl)) = (header_end, body_len) {
            if acc.len() >= he + bl {
                break;
            }
        }
    }
    acc
}

fn tcp_client(port: u16) -> IaClient {
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = format!("127.0.0.1:{port}");
    config.general.secure = false;
    IaClient::from_config(config).unwrap()
}

/// A connection that closes after the request was written, before any
/// response, is what a reset or a dropped keep-alive looks like to hyper
/// (IncompleteMessage). The middleware this branch removed retried it; the
/// shared policy must too, or one blip on one part aborts a whole upload.
#[tokio::test]
async fn part_put_retries_when_the_connection_closes_before_a_response() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Connection 1: read the whole PUT, then hang up without answering.
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let req = read_full_request(&mut stream).await;
            assert!(
                req.starts_with(b"PUT "),
                "first request should be the part PUT"
            );
            drop(stream);
        }
        // Connection 2: the retry. Answer it properly.
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let req = read_full_request(&mut stream).await;
            assert!(req.starts_with(b"PUT "), "retry should be the same PUT");
            let resp = "HTTP/1.1 200 OK\r\netag: \"etag-retry\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        }
    });

    let client = tcp_client(port);
    let etag = multipart::upload_part(
        &client,
        "test-item",
        "data.bin",
        "up-1",
        1,
        b"hello world".to_vec(),
    )
    .await
    .expect("a closed connection is transient and must be retried");

    assert_eq!(etag, "\"etag-retry\"");
    server.await.unwrap();
}

// ── Shared-policy behaviours pinned after review ─────────────────────────

/// IA's spam rejection is a plain-text 503 with no S3 <Code>. The status
/// fallback would retry it as a transient 5xx for the whole budget; the
/// single-file path returns SpamDetected at once, and so must this one.
#[tokio::test]
async fn initiate_spam_rejection_is_not_retried() {
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

    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            "<html><body>Your upload appears to be spam. Please contact info@archive.org.</body></html>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(5),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    assert!(
        matches!(result, Err(ia_core::IaError::SpamDetected { .. })),
        "got {result:?}"
    );
    server.verify().await;
}

/// The resume check is the first request of every multipart upload. It must
/// use the budget the user asked for, not a fixed one.
#[tokio::test]
async fn resume_check_uses_the_configured_retry_budget() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .expect(6) // 1 initial + 5 retries
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(5),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await;

    assert!(result.is_err());
    server.verify().await;
}

/// Mount a mock that answers once with a 503 SlowDown, then a second mock
/// that answers every later request with `ok`.
async fn mock_slowdown_then(
    server: &MockServer,
    m: &str,
    p: &str,
    param: (&str, &str),
    ok: ResponseTemplate,
) {
    Mock::given(method(m))
        .and(path(p))
        .and(query_param(param.0, param.1))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method(m))
        .and(path(p))
        .and(query_param(param.0, param.1))
        .respond_with(ok)
        .mount(server)
        .await;
}

/// UploadResult.retries is what the joblog and --json report. With retry
/// now application-level for every request, it must count the control
/// calls too, not only the parts.
#[tokio::test]
async fn retries_counts_control_call_attempts() {
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
    mock_slowdown_then(
        &server,
        "POST",
        "/test-item/data.bin",
        ("uploads", ""),
        ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-6</UploadId></InitiateMultipartUploadResult>",
        ),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;
    mock_slowdown_then(
        &server,
        "POST",
        "/test-item/data.bin",
        ("uploadId", "mp-6"),
        ResponseTemplate::new(200),
    )
    .await;

    let f = temp_file(b"hello world");
    let result = multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(
        result.retries, 2,
        "one initiate retry plus one complete retry"
    );
}

/// A backoff after a throttle is a rate-limit wait; a backoff after any
/// other failure is a plain retry. The progress bar prints different text
/// for the two, so the events must say which one happened, and the event
/// for the completion call must carry the bytes actually sent.
#[tokio::test]
async fn progress_distinguishes_rate_limit_waits_from_other_retries() {
    use ia_core::upload::{UploadProgress, UploadProgressStatus};
    use std::sync::{Arc, Mutex};

    let server = MockServer::start().await;
    mock_resume_empty_and_initiate(&server, "mp-7").await;

    // Part: 500 InternalError once (retryable, not a throttle), then 200.
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(
            ResponseTemplate::new(500).set_body_string(
                "<Error><Code>InternalError</Code><Message>oops</Message></Error>",
            ),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"etag1\""))
        .mount(&server)
        .await;
    // Complete: 503 SlowDown once (a throttle), then 200.
    mock_slowdown_then(
        &server,
        "POST",
        "/test-item/data.bin",
        ("uploadId", "mp-7"),
        ResponseTemplate::new(200),
    )
    .await;

    let events: Arc<Mutex<Vec<UploadProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let progress: Arc<dyn Fn(UploadProgress) + Send + Sync> =
        Arc::new(move |p| sink.lock().unwrap().push(p));

    let f = temp_file(b"hello world");
    multipart::upload_file_multipart(
        &test_client(&server),
        "test-item",
        f.path(),
        "data.bin",
        &fast_opts(3),
        1024,
        true,
        true,
        None,
        Some(progress),
        None,
    )
    .await
    .unwrap();

    let events = events.lock().unwrap();
    let retrying: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.status, UploadProgressStatus::Retrying))
        .collect();
    let waiting: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.status, UploadProgressStatus::WaitingRateLimit))
        .collect();
    assert_eq!(
        retrying.len(),
        1,
        "one Retrying for the InternalError: {events:#?}"
    );
    assert_eq!(retrying[0].bytes_sent, 0, "part 1 starts at offset 0");
    assert_eq!(
        waiting.len(),
        1,
        "one WaitingRateLimit for the SlowDown: {events:#?}"
    );
    assert_eq!(
        waiting[0].bytes_sent, 11,
        "completion backoff reports the whole file as sent"
    );
}

// ── Abort ────────────────────────────────────────────────────────────────

/// `ia upload cleanup --abort-all` aborts every listed upload in turn. One
/// that vanished between the listing and the DELETE is not a failure, and
/// must not stop the loop.
#[tokio::test]
async fn abort_of_a_vanished_upload_is_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "gone"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchUpload</Code><Message>no such upload</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    multipart::abort_upload(&test_client(&server), "test-item", "data.bin", "gone")
        .await
        .expect("an already-gone upload is already aborted");
    server.verify().await;
}

/// The public abort wrapper keeps the default budget, so a throttle on the
/// DELETE is retried.
#[tokio::test]
async fn abort_retries_a_throttle() {
    let server = MockServer::start().await;
    mock_slowdown_then(
        &server,
        "DELETE",
        "/test-item/data.bin",
        ("uploadId", "mp-8"),
        ResponseTemplate::new(204),
    )
    .await;

    multipart::abort_upload(&test_client(&server), "test-item", "data.bin", "mp-8")
        .await
        .expect("a throttled abort retries and succeeds");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "{requests:#?}");
}

// ── Resume validation (#19) ─────────────────────────────────────────────
//
// 30-byte file "aaaaabbbbbcccccdddddeeeeefffff", part size 10: three parts
// whose md5s are known. A listed part is reused only when its size and
// md5 match the local range; otherwise a fresh upload starts and the stale
// one is left for `ia upload cleanup`.

const THIRTY: &[u8] = b"aaaaabbbbbcccccdddddeeeeefffff";

fn uploads_xml(entries: &[(&str, &str)]) -> String {
    let mut xml = String::from("<ListMultipartUploadsResult>");
    for (key, id) in entries {
        xml.push_str(&format!(
            "<Upload><Key>{key}</Key><UploadId>{id}</UploadId><Initiated>2026-10-02T00:00:00.000Z</Initiated></Upload>"
        ));
    }
    xml.push_str("</ListMultipartUploadsResult>");
    xml
}

async fn mount_list_uploads(server: &MockServer, entries: &[(&str, &str)]) {
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(uploads_xml(entries)))
        .mount(server)
        .await;
}

async fn mount_list_parts(server: &MockServer, upload_id: &str, parts_xml: &str) {
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", upload_id))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<ListPartsResult>{parts_xml}</ListPartsResult>")),
        )
        .mount(server)
        .await;
}

/// A fresh upload after a rejected resume: initiate → `fresh-1`, every
/// part PUT once under it, complete once, and never an abort.
async fn mount_fresh_upload_of_three_parts(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>fresh-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(server)
        .await;
    for n in 1..=3 {
        Mock::given(method("PUT"))
            .and(path("/test-item/data.bin"))
            .and(query_param("partNumber", n.to_string()))
            .and(query_param("uploadId", "fresh-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "fresh-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/test-item/data.bin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(server)
        .await;
}

async fn upload_thirty(server: &MockServer) -> ia_core::upload::UploadResult {
    let client = test_client(server);
    let f = temp_file(THIRTY);
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };
    multipart::upload_file_multipart(
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
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn resume_rejects_a_part_whose_md5_differs() {
    let server = MockServer::start().await;
    mount_list_uploads(&server, &[("data.bin", "stale-1")]).await;
    mount_list_parts(
        &server,
        "stale-1",
        r#"<Part><PartNumber>1</PartNumber><ETag>"0123456789abcdef0123456789abcdef"</ETag><Size>10</Size></Part>"#,
    )
    .await;
    mount_fresh_upload_of_three_parts(&server).await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

#[tokio::test]
async fn resume_rejects_a_part_whose_size_differs() {
    let server = MockServer::start().await;
    mount_list_uploads(&server, &[("data.bin", "stale-1")]).await;
    mount_list_parts(
        &server,
        "stale-1",
        &format!(
            r#"<Part><PartNumber>1</PartNumber><ETag>"{}"</ETag><Size>9</Size></Part>"#,
            md5_hex(b"aaaaabbbbb")
        ),
    )
    .await;
    mount_fresh_upload_of_three_parts(&server).await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

#[tokio::test]
async fn resume_rejects_a_part_number_past_the_count() {
    let server = MockServer::start().await;
    mount_list_uploads(&server, &[("data.bin", "stale-1")]).await;
    mount_list_parts(
        &server,
        "stale-1",
        &format!(
            r#"<Part><PartNumber>5</PartNumber><ETag>"{}"</ETag><Size>10</Size></Part>"#,
            md5_hex(b"aaaaabbbbb")
        ),
    )
    .await;
    mount_fresh_upload_of_three_parts(&server).await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

/// Two unfinished uploads for the key: the newer one does not match, the
/// older one does. The older one is resumed; nothing is initiated.
#[tokio::test]
async fn resume_picks_the_newest_valid_upload() {
    let server = MockServer::start().await;
    mount_list_uploads(&server, &[("data.bin", "old-ok"), ("data.bin", "new-bad")]).await;
    mount_list_parts(
        &server,
        "new-bad",
        r#"<Part><PartNumber>1</PartNumber><ETag>"0123456789abcdef0123456789abcdef"</ETag><Size>10</Size></Part>"#,
    )
    .await;
    mount_list_parts(
        &server,
        "old-ok",
        &format!(
            r#"<Part><PartNumber>1</PartNumber><ETag>"{}"</ETag><Size>10</Size></Part>"#,
            md5_hex(b"aaaaabbbbb")
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    for n in 2..=3 {
        Mock::given(method("PUT"))
            .and(path("/test-item/data.bin"))
            .and(query_param("partNumber", n.to_string()))
            .and(query_param("uploadId", "old-ok"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "old-ok"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

/// A listing without `<Size>` is not held against the part; the md5 is
/// the stronger check and it matches.
#[tokio::test]
async fn resume_with_a_missing_size_relies_on_the_md5() {
    let server = MockServer::start().await;
    mount_list_uploads(&server, &[("data.bin", "nosize-1")]).await;
    mount_list_parts(
        &server,
        "nosize-1",
        &format!(
            r#"<Part><PartNumber>1</PartNumber><ETag>"{}"</ETag></Part>"#,
            md5_hex(b"aaaaabbbbb")
        ),
    )
    .await;
    for n in 2..=3 {
        Mock::given(method("PUT"))
            .and(path("/test-item/data.bin"))
            .and(query_param("partNumber", n.to_string()))
            .and(query_param("uploadId", "nosize-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "nosize-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

// ── Pagination (#19) ────────────────────────────────────────────────────
//
// S3 returns at most 1000 entries per listing and marks the page with
// IsTruncated and a marker for the next request. Whether IA paginates is
// unknown; the protocol is followed either way.

/// Page 1 is truncated with NextPartNumberMarker 1; page 2, requested
/// with part-number-marker=1, holds part 2.
#[tokio::test]
async fn list_parts_follows_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "paged"))
        .and(query_param("part-number-marker", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult><IsTruncated>false</IsTruncated>
<Part><PartNumber>2</PartNumber><ETag>"e2"</ETag><Size>10</Size></Part></ListPartsResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "paged"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1</NextPartNumberMarker>
<Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>10</Size></Part></ListPartsResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = multipart::list_parts(&client, "test-item", "data.bin", "paged")
        .await
        .unwrap();
    let numbers: Vec<u32> = parts.iter().map(|p| p.part_number).collect();
    assert_eq!(numbers, [1, 2]);
    server.verify().await;
}

/// Page 1 is truncated with key and upload-id markers; page 2 is requested
/// with key-marker and upload-id-marker.
#[tokio::test]
async fn list_uploads_follows_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .and(query_param("key-marker", "a.bin"))
        .and(query_param("upload-id-marker", "u1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListMultipartUploadsResult><IsTruncated>false</IsTruncated>
<Upload><Key>b.bin</Key><UploadId>u2</UploadId></Upload></ListMultipartUploadsResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListMultipartUploadsResult><IsTruncated>true</IsTruncated>
<NextKeyMarker>a.bin</NextKeyMarker><NextUploadIdMarker>u1</NextUploadIdMarker>
<Upload><Key>a.bin</Key><UploadId>u1</UploadId></Upload></ListMultipartUploadsResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let uploads = multipart::list_uploads(&client, "test-item").await.unwrap();
    let ids: Vec<&str> = uploads.iter().map(|u| u.upload_id.as_str()).collect();
    assert_eq!(ids, ["u1", "u2"]);
    server.verify().await;
}

/// A page that claims to be truncated but gives no marker ends the walk
/// with what was read rather than asking for the same page again.
#[tokio::test]
async fn list_parts_truncated_without_a_marker_stops() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "odd"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult><IsTruncated>true</IsTruncated>
<Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>10</Size></Part></ListPartsResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = multipart::list_parts(&client, "test-item", "data.bin", "odd")
        .await
        .unwrap();
    assert_eq!(parts.len(), 1);
    server.verify().await;
}

/// A server that keeps answering the same marker would otherwise be asked
/// for the same page forever; the walk stops when the marker repeats.
#[tokio::test]
async fn list_parts_stops_when_the_marker_repeats() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "loop"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1</NextPartNumberMarker>
<Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>10</Size></Part></ListPartsResult>"#,
        ))
        .expect(2)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let parts = multipart::list_parts(&client, "test-item", "data.bin", "loop")
        .await
        .unwrap();
    assert_eq!(parts.len(), 2, "two pages were read, then the walk stopped");
    server.verify().await;
}

/// "Newest" follows each upload's Initiated time, not the listing order:
/// IA may list newest first. Two valid uploads, the newer listed first →
/// the newer is resumed.
#[tokio::test]
async fn resume_prefers_the_newest_by_initiated_time() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult>\
             <Upload><Key>data.bin</Key><UploadId>newer</UploadId><Initiated>2026-10-02T02:00:00.000Z</Initiated></Upload>\
             <Upload><Key>data.bin</Key><UploadId>older</UploadId><Initiated>2026-10-02T01:00:00.000Z</Initiated></Upload>\
             </ListMultipartUploadsResult>",
        ))
        .mount(&server)
        .await;
    let part1 = format!(
        r#"<Part><PartNumber>1</PartNumber><ETag>"{}"</ETag><Size>10</Size></Part>"#,
        md5_hex(b"aaaaabbbbb")
    );
    mount_list_parts(&server, "newer", &part1).await;
    mount_list_parts(&server, "older", &part1).await;
    for n in 2..=3 {
        Mock::given(method("PUT"))
            .and(path("/test-item/data.bin"))
            .and(query_param("partNumber", n.to_string()))
            .and(query_param("uploadId", "newer"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "newer"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let result = upload_thirty(&server).await;
    assert!(matches!(result.status, UploadStatus::Uploaded));
    server.verify().await;
}

// ── The skip check and one read apply to --multipart too (#20) ──────────
//
// These go through `upload::upload_file` with `multipart: true`, the way
// the CLI does, because the skip-if-already-uploaded check lives there.

async fn mount_metadata_with(server: &MockServer, md5: Option<&str>, size: u64) {
    let mut file =
        serde_json::json!({"name": "data.bin", "size": size.to_string(), "source": "original"});
    if let Some(md5) = md5 {
        file["md5"] = serde_json::Value::String(md5.to_string());
    }
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [file]
        })))
        .mount(server)
        .await;
}

/// The whole single-part multipart flow under the default part size:
/// list_uploads empty, initiate `mp-1`, part 1, complete.
async fn mount_fresh_single_part_upload(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>mp-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/data.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/data.bin"))
        .and(query_param("uploadId", "mp-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
}

async fn upload_file_multipart_via_upload_file(
    server: &MockServer,
    checksum: bool,
    verify: bool,
) -> ia_core::upload::UploadResult {
    let client = test_client(server);
    let f = temp_file(THIRTY);
    let opts = UploadOpts {
        multipart: true,
        checksum,
        verify,
        ..Default::default()
    };
    ia_core::upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "data.bin",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap()
}

/// A file whose md5 the item already lists is skipped, as for a single
/// PUT; no S3 request is made.
#[tokio::test]
async fn multipart_skips_a_file_whose_md5_matches() {
    let server = MockServer::start().await;
    mount_metadata_with(&server, Some(&md5_hex(THIRTY)), 30).await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .expect(0)
        .mount(&server)
        .await;
    let result = upload_file_multipart_via_upload_file(&server, true, true).await;
    assert!(matches!(result.status, UploadStatus::Skipped), "{result:?}");
    assert_eq!(result.md5.as_deref(), Some(md5_hex(THIRTY).as_str()));
    server.verify().await;
}

/// `--clobber` uploads despite the matching md5, and the result carries the
/// local md5 like a single PUT's does.
#[tokio::test]
async fn multipart_clobber_uploads_despite_a_matching_md5_and_sets_md5() {
    let server = MockServer::start().await;
    mount_metadata_with(&server, Some(&md5_hex(THIRTY)), 30).await;
    mount_fresh_single_part_upload(&server).await;
    let result = upload_file_multipart_via_upload_file(&server, false, true).await;
    assert!(
        matches!(result.status, UploadStatus::Uploaded),
        "{result:?}"
    );
    assert_eq!(result.md5.as_deref(), Some(md5_hex(THIRTY).as_str()));
    server.verify().await;
}

/// `--clobber --no-verify` reads nothing before uploading: no metadata
/// lookup, no md5 in the result.
#[tokio::test]
async fn multipart_clobber_no_verify_reads_nothing_before_uploading() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": []})))
        .expect(0)
        .mount(&server)
        .await;
    mount_fresh_single_part_upload(&server).await;
    let result = upload_file_multipart_via_upload_file(&server, false, false).await;
    assert!(
        matches!(result.status, UploadStatus::Uploaded),
        "{result:?}"
    );
    assert_eq!(result.md5, None);
    server.verify().await;
}
