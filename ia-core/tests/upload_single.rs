//! Integration tests for single file upload (`upload_file`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

use ia_core::upload::multipart::MULTIPART_FALLBACK_MIN_SIZE;
use wiremock::matchers::{query_param, query_param_is_missing};

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

/// Helper to create a temp file with given content.
fn temp_file(content: &[u8]) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content).unwrap();
    f.flush().unwrap();
    f
}

// -- Basic upload success --

#[tokio::test]
async fn upload_single_file_success() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/hello.txt"))
        .and(header("authorization", "LOW test-access:test-secret"))
        .and(header("x-archive-keep-old-version", "1"))
        .and(header("expect", "100-continue"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"hello world");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.checksum = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "hello.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 11);
    assert_eq!(result.retries, 0);
    assert_eq!(result.identifier, "test-item");
    assert_eq!(result.key, "hello.txt");
    assert!(result.md5.is_none()); // verify=false, checksum=false
}

// -- Dry run --

#[tokio::test]
async fn upload_dry_run_no_http() {
    let server = MockServer::start().await;
    // Don't mount any mocks -- any request would cause unexpected behavior

    let f = temp_file(b"test data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.dry_run = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::DryRun));
    assert_eq!(result.bytes, 9);
    assert_eq!(result.retries, 0);
}

// -- Spam detection --

#[tokio::test]
async fn upload_503_spam_detection() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Your upload appears to be spam."))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, ia_core::IaError::SpamDetected { .. }));
}

// -- No backup omits header --

#[tokio::test]
async fn upload_no_backup_omits_keep_old_version() {
    let server = MockServer::start().await;

    // We can only verify the request was made; wiremock doesn't support
    // negative header matching. But we verify the upload succeeds.
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_backup = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
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

// -- Content-MD5 header --

#[tokio::test]
async fn upload_with_content_md5() {
    let server = MockServer::start().await;

    // Verify the Content-MD5 header is present when verify=true
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .and(header_exists("content-md5"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert!(result.md5.is_some());
    // MD5 of "hello" is 5d41402abc4b2a76b9719d911017c592
    assert_eq!(
        result.md5.as_deref(),
        Some("5d41402abc4b2a76b9719d911017c592")
    );
}

// -- Derive header on last file --

#[tokio::test]
async fn upload_derive_header_last_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-queue-derive", "1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true, // is_last_file = true -> derive=1
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_derive_header_not_last_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-queue-derive", "0"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        false, // is_last_file = false -> derive=0
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_no_derive_always_zero() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-queue-derive", "0"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_derive = true;
        o
    };

    // Even with is_last_file=true, no_derive forces derive=0
    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
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

// -- Auto make bucket on first file --

#[tokio::test]
async fn upload_auto_make_bucket_first_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-auto-make-bucket", "1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true, // is_first_file = true -> auto-make-bucket=1
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

// -- Metadata headers on first file --

#[tokio::test]
async fn upload_metadata_headers_on_first_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-meta00-mediatype", "texts"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.metadata = vec![("mediatype".into(), "texts".into())];
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true, // is_first_file = true -> metadata headers sent
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

// -- Size hint header --

#[tokio::test]
async fn upload_size_hint_header() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-archive-size-hint", "999999"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        Some(999999), // size_hint
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

// -- Auth error --

#[tokio::test]
async fn upload_no_auth_returns_error() {
    let server = MockServer::start().await;

    let f = temp_file(b"data");

    // Client without S3 credentials
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.general.host = host_port;
    config.general.secure = false;
    let client = IaClient::from_config_no_retry(config).unwrap();

    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ia_core::IaError::Auth(_)));
}

// -- Custom headers --

#[tokio::test]
async fn upload_custom_headers() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("x-custom-header", "custom-value"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.headers = vec![("x-custom-header".into(), "custom-value".into())];
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
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

// -- Delete after upload --

#[tokio::test]
async fn upload_delete_after_upload() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"delete me");
    let file_path = f.path().to_path_buf();
    // Keep the NamedTempFile alive but get the path
    assert!(file_path.exists());

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.delete_after_upload = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        &file_path,
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    // File should be deleted after upload
    assert!(!file_path.exists());
}

// -- Progress callback --

#[tokio::test]
async fn upload_progress_callback_fires() {
    use std::sync::{Arc, Mutex};

    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"progress data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = true;
        o
    };

    let statuses: Arc<Mutex<Vec<ia_core::upload::UploadProgressStatus>>> =
        Arc::new(Mutex::new(Vec::new()));
    let statuses_clone = statuses.clone();

    let cb: std::sync::Arc<dyn Fn(ia_core::upload::UploadProgress) + Send + Sync> =
        std::sync::Arc::new(move |p: ia_core::upload::UploadProgress| {
            statuses_clone.lock().unwrap().push(p.status);
        });

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        Some(cb),
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));

    let observed = statuses.lock().unwrap();
    // Should see Verifying, Uploading, Complete (in that order)
    assert!(
        observed.contains(&ia_core::upload::UploadProgressStatus::Verifying),
        "expected Verifying in progress updates: {observed:?}"
    );
    assert!(
        observed.contains(&ia_core::upload::UploadProgressStatus::Uploading),
        "expected Uploading in progress updates: {observed:?}"
    );
    assert!(
        observed.contains(&ia_core::upload::UploadProgressStatus::Complete),
        "expected Complete in progress updates: {observed:?}"
    );
}

// -- 503 rate limit retry with check_limit --

#[tokio::test]
async fn upload_503_rate_limit_retry() {
    let server = MockServer::start().await;

    // First PUT returns 503 (non-spam), second succeeds
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(503).set_body_string("Please reduce your request rate."),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // check_limit returns not-over-limit
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        // fast for tests
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
}

// -- Content-Length header always present --

#[tokio::test]
async fn upload_content_length_header() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("content-length", "13"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"hello, world!"); // 13 bytes
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 13);
}

// -- Non-503 error retry classification --

#[tokio::test]
async fn upload_403_is_not_retried() {
    let server = MockServer::start().await;

    // 403 should be returned immediately — NOT retried
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .expect(1) // exactly 1 request — no retries
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("AccessDenied"),
        "expected AccessDenied in error: {err}"
    );
}

#[tokio::test]
async fn upload_400_bad_digest_is_retried() {
    // IA's md5 of the body it received differs from Content-MD5: over https
    // that means a body IA did not receive whole, or an IA-side fault, and
    // a re-send fixes it. One BadDigest, then a 200.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>",
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = true;
        o.checksum = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .expect("a BadDigest is retried and the second PUT succeeds");
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    server.verify().await;
}

/// Every send gets BadDigest: the budget is spent and the error says how
/// many attempts were made, as the part path's message does.
#[tokio::test]
async fn upload_exhausted_bad_digest_names_the_attempt_count() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>",
        ))
        .expect(3)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.checksum = false;
        o.retries = 2;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .expect_err("every attempt was refused");
    let msg = err.to_string();
    assert!(msg.contains("BadDigest"), "{msg}");
    assert!(msg.contains("after 3 attempts"), "{msg}");
    server.verify().await;
}

// -- Checksum skip --

#[tokio::test]
async fn upload_checksum_skip_when_md5_matches() {
    let server = MockServer::start().await;

    // Mock metadata endpoint — file exists with matching MD5
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}
            ]
        })))
        .mount(&server)
        .await;

    // No PUT request should be made
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let f = temp_file(b"hello"); // MD5 = 5d41402abc4b2a76b9719d911017c592
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.checksum = true;
        o.verify = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Skipped));
}

#[tokio::test]
async fn upload_checksum_no_skip_when_md5_differs() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "0000000000000000000000000000000", "size": "5"}
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.checksum = true;
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
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

#[tokio::test]
async fn upload_checksum_no_skip_when_file_not_on_remote() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "other.txt", "md5": "abc123", "size": "10"}
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.checksum = true;
        o.verify = false;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
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

#[tokio::test]
async fn upload_checksum_no_verify_still_computes_md5_for_skip() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.checksum = true;
        o.verify = false;
        // no Content-MD5 header, but still compute for skip
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Skipped));
}

// -- Empty file upload --

#[tokio::test]
async fn upload_empty_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("Content-Length", "0"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.txt");
    std::fs::write(&file, "").unwrap();

    let opts = UploadOpts::default();
    let result = upload::upload_file(
        &client,
        "test-item",
        &file,
        "empty.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 0);
}

// -- Pre-computed checksums used for Content-MD5 --

#[tokio::test]
async fn upload_precomputed_checksum_used_for_content_md5() {
    let server = MockServer::start().await;

    // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
    // Base64 of those 16 bytes = XUFAKrxLKna5cZ2REBfFkg==
    Mock::given(method("PUT"))
        .and(header("Content-MD5", "XUFAKrxLKna5cZ2REBfFkg=="))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let f = temp_file(b"hello");

    let mut checksum_file = std::collections::HashMap::new();
    checksum_file.insert("test.txt".into(), "5d41402abc4b2a76b9719d911017c592".into());

    let opts = {
        let mut o = UploadOpts::default();
        o.verify = true;
        o.checksum_file = Some(checksum_file);
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
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

// -- Dry run with verify computes MD5 --

// -- Network error retry --

#[tokio::test]
async fn upload_retries_on_server_error() {
    let server = MockServer::start().await;

    // First attempt: 500 with retryable S3 error
    Mock::given(method("PUT"))
        .and(path("/test-item/retry.txt"))
        .respond_with(ResponseTemplate::new(500).set_body_string(
            "<Error><Code>InternalError</Code><Message>Temporary</Message></Error>",
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    // Second attempt: success
    Mock::given(method("PUT"))
        .and(path("/test-item/retry.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // check_limit returns not-over-limit (called before retry attempt)
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .mount(&server)
        .await;

    let f = temp_file(b"retry content");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "retry.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
}

// -- 503 retries exhausted --

#[tokio::test]
async fn upload_503_retries_exhausted() {
    let server = MockServer::start().await;

    // check_limit returns not-over-limit so poll_check_limit clears quickly
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"over_limit": 0, "detail": {"rationing_level": 0}}"#),
        )
        .mount(&server)
        .await;

    // Always return 503 (non-spam) — retries will be exhausted
    Mock::given(method("PUT"))
        .and(path("/test-item/exhaust.txt"))
        .respond_with(
            ResponseTemplate::new(503).set_body_string("Please reduce your request rate."),
        )
        .mount(&server)
        .await;

    let f = temp_file(b"exhaust");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 2;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "exhaust.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("503"),
        "expected error to contain '503', got: {err}"
    );
    assert!(
        err.to_string().contains("after 3 attempts"),
        "the spent 503 budget must count attempts like every other branch: {err}"
    );
}

// -- no_auto_make_bucket omits header --

#[tokio::test]
async fn upload_no_auto_make_bucket_omits_header() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"bucket test");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_auto_make_bucket = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));

    let requests = server.received_requests().await.unwrap();
    let put_req = requests
        .iter()
        .find(|r| r.method.as_str() == "PUT")
        .unwrap();
    assert!(
        put_req.headers.get("x-archive-auto-make-bucket").is_none(),
        "header should not be set when no_auto_make_bucket=true"
    );
}

// -- Dry run with verify computes MD5 --

#[tokio::test]
async fn dry_run_with_verify_computes_md5() {
    let server = MockServer::start().await;
    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = true;
        o.dry_run = true;
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::DryRun));
    assert_eq!(
        result.md5.as_deref(),
        Some("5d41402abc4b2a76b9719d911017c592")
    );
}

// -- Multipart dispatch --

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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
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

    // The item's listing is read once, for the skip check before the
    // upload; nothing is read after completion.
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let opts = {
        let mut o = UploadOpts::default();
        o.multipart = true;
        o.verify = false;
        o
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
    server.verify().await;
}

// -- Regression: upload with retry middleware (from_config) must not fail --
//
// Before the fix, the retry middleware (reqwest-retry) tried to clone the
// request body via try_clone(). Streaming bodies (wrap_stream, File) are not
// cloneable, causing immediate "Request object is not cloneable" errors on
// every attempt. The upload code now uses raw_http() to bypass middleware.
//
// These tests use `from_config()` (NOT `from_config_no_retry()`) to exercise
// the production client path that was broken.

/// Create an `IaClient` with retry middleware, pointed at a wiremock server.
fn test_client_with_retry(server: &MockServer) -> IaClient {
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = host_port;
    config.general.secure = false;
    IaClient::from_config(config).unwrap()
}

#[tokio::test]
async fn upload_with_retry_middleware_and_progress_callback() {
    use std::sync::{Arc, Mutex};

    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"retry middleware regression test");
    let client = test_client_with_retry(&server);
    let opts = UploadOpts::default();

    let statuses: Arc<Mutex<Vec<ia_core::upload::UploadProgressStatus>>> =
        Arc::new(Mutex::new(Vec::new()));
    let statuses_clone = statuses.clone();

    let cb: Arc<dyn Fn(ia_core::upload::UploadProgress) + Send + Sync> =
        Arc::new(move |p: ia_core::upload::UploadProgress| {
            statuses_clone.lock().unwrap().push(p.status);
        });

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        Some(cb),
    )
    .await
    .unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));

    let observed = statuses.lock().unwrap();
    assert!(
        observed.contains(&ia_core::upload::UploadProgressStatus::Uploading),
        "expected Uploading in progress updates: {observed:?}"
    );
    assert!(
        observed.contains(&ia_core::upload::UploadProgressStatus::Complete),
        "expected Complete in progress updates: {observed:?}"
    );
}

#[tokio::test]
async fn upload_with_retry_middleware_no_progress() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let f = temp_file(b"no progress callback test");
    let client = test_client_with_retry(&server);
    let opts = UploadOpts::default();

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
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

/// A 503 that carries a non-retryable S3 code is a refusal, not a throttle.
/// The multipart path fails on the first attempt; the single-file path must
/// too, instead of polling check_limit and re-sending the whole file.
#[tokio::test]
async fn upload_503_with_non_retryable_code_is_not_retried() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await;

    let err = result.expect_err("AccessDenied must fail on the first attempt");
    assert!(
        err.to_string().contains("AccessDenied"),
        "expected AccessDenied in error: {err}"
    );
    server.verify().await;
}

/// A 503 that carries `Retry-After` is waited out for as long as the
/// server says before the check-limit poll and the retry, not for the
/// backoff's millisecond test bounds.
#[tokio::test]
async fn upload_503_retry_honors_retry_after() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "1")
                .set_body_string("Please reduce your request rate."),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .mount(&server)
        .await;

    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.retries = 3;
        o.retry_min_delay = std::time::Duration::from_millis(1);
        o.retry_max_delay = std::time::Duration::from_millis(2);
        o
    };

    let started = std::time::Instant::now();
    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
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

// -- Review findings on the backoff PR: Retry-After corners --

/// Collect every progress event, so a test can say which status was
/// reported for which key.
fn collect_progress() -> (
    Arc<dyn Fn(upload::UploadProgress) + Send + Sync>,
    Arc<Mutex<Vec<upload::UploadProgress>>>,
) {
    let events: Arc<Mutex<Vec<upload::UploadProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let cb: Arc<dyn Fn(upload::UploadProgress) + Send + Sync> = Arc::new(move |p| {
        sink.lock().unwrap().push(p);
    });
    (cb, events)
}

/// Mount a 503 with the given Retry-After value once, then a 200, and a
/// check_limit that clears at once.
async fn mount_503_then_200(server: &MockServer, retry_after: &str) {
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", retry_after)
                .set_body_string("Please reduce your request rate."),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .mount(server)
        .await;
}

fn fast_opts(retries: u32) -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.checksum = false;
    o.retries = retries;
    o.retry_min_delay = Duration::from_millis(1);
    o.retry_max_delay = Duration::from_millis(2);
    o
}

/// While the upload sleeps out a Retry-After before polling check_limit,
/// the UI must already show "waiting for rate limit", not stay on
/// "uploading". The poll emits that status itself, but only once it
/// starts, after the sleep; the event emitted before the sleep is what
/// covers the wait.
#[tokio::test]
async fn upload_503_retry_after_reports_waiting_before_the_sleep() {
    let server = MockServer::start().await;
    mount_503_then_200(&server, "1").await;
    let f = temp_file(b"data");
    let client = test_client(&server);
    let (cb, events) = collect_progress();

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &fast_opts(3),
        true,
        true,
        None,
        Some(cb),
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));

    let events = events.lock().unwrap();
    let waiting_keys: Vec<&str> = events
        .iter()
        .filter(|p| matches!(p.status, upload::UploadProgressStatus::WaitingRateLimit))
        .map(|p| p.key.as_str())
        .collect();
    assert!(
        waiting_keys.contains(&"file.txt"),
        "no WaitingRateLimit for the file before the Retry-After sleep; got {waiting_keys:?}"
    );
}

/// `Retry-After: 0` means re-send now: no backoff wait is substituted. On
/// a 500 (the non-503 path) a dropped header would fall back to the 10 s
/// draw, so this also proves the header was read.
#[tokio::test]
async fn upload_500_retry_after_zero_retries_at_once() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("Retry-After", "0")
                .set_body_string("internal error"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = fast_opts(3);
        o.retry_min_delay = Duration::from_secs(10);
        o.retry_max_delay = Duration::from_secs(10);
        o
    };

    let started = Instant::now();
    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "Retry-After: 0 was not honored as an immediate retry ({:?})",
        started.elapsed()
    );
}

/// The header's HTTP-date form is honored like the seconds form.
///
/// The date has one-second granularity and is measured against the clock
/// when the 503 is read, so it is computed right before the request with
/// 3 s of headroom: the wait is then at least 1 s as long as the request
/// takes under a second to reach the mock, which a loaded CI runner can
/// otherwise exceed when the date is fixed before the server is mounted.
#[tokio::test]
async fn upload_503_retry_after_http_date_is_honored() {
    let server = MockServer::start().await;
    let f = temp_file(b"data");
    let client = test_client(&server);
    let when = std::time::SystemTime::now() + Duration::from_secs(3);
    mount_503_then_200(&server, &httpdate::fmt_http_date(when)).await;

    let started = Instant::now();
    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &fast_opts(3),
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "HTTP-date Retry-After was not waited for ({:?})",
        started.elapsed()
    );
}

/// A 500 with Retry-After goes down the non-503 retry path; the header is
/// honored there too.
#[tokio::test]
async fn upload_500_retry_honors_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("Retry-After", "1")
                .set_body_string("internal error"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let f = temp_file(b"data");
    let client = test_client(&server);

    let started = Instant::now();
    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &fast_opts(3),
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "Retry-After: 1 on a 500 was not waited for ({:?})",
        started.elapsed()
    );
}

/// When the last check_limit poll still says over the limit, the upload
/// fails right away; there is no retry left for the sleep to precede.
/// (Before the fix the sleep was a jittered draw up to 10 s, so this test
/// fails most runs rather than every run.)
#[tokio::test]
async fn check_limit_exhaustion_does_not_sleep_after_the_last_poll() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(503).set_body_string("Please reduce your request rate."),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":1}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let f = temp_file(b"data");
    let client = test_client(&server);
    let opts = {
        let mut o = fast_opts(1);
        o.retry_min_delay = Duration::from_secs(10);
        o.retry_max_delay = Duration::from_secs(10);
        o
    };

    let started = Instant::now();
    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .expect_err("the rate limit never clears");
    assert!(
        matches!(err, ia_core::IaError::CheckLimitFailed { .. }),
        "expected CheckLimitFailed, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "slept after the final check_limit poll ({:?})",
        started.elapsed()
    );
    server.verify().await;
}

// -- Series review (2026-10-05): message and progress gaps --

/// A 503 with no `Retry-After` goes straight to the check_limit poll. The
/// poll's `WaitingRateLimit` event is then the only one the file gets, so
/// it must carry the file's key and size, or the per-file row sits on
/// "uploading" for the whole poll.
#[tokio::test]
async fn check_limit_poll_reports_waiting_for_the_file() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(
            ResponseTemplate::new(503).set_body_string("Please reduce your request rate."),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let f = temp_file(b"data");
    let client = test_client(&server);
    let (cb, events) = collect_progress();

    let result = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "file.txt",
        &fast_opts(3),
        true,
        true,
        None,
        Some(cb),
    )
    .await
    .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));

    let events = events.lock().unwrap();
    let waiting: Vec<(&str, u64)> = events
        .iter()
        .filter(|p| matches!(p.status, upload::UploadProgressStatus::WaitingRateLimit))
        .map(|p| (p.key.as_str(), p.total_bytes))
        .collect();
    assert!(
        !waiting.is_empty(),
        "no WaitingRateLimit event at all during the check_limit poll"
    );
    assert!(
        waiting
            .iter()
            .all(|(key, total)| *key == "file.txt" && *total == 4),
        "every WaitingRateLimit event must name the file and its size; got {waiting:?}"
    );
    server.verify().await;
}

/// Every connection is closed before a response: the budget is spent on
/// transport errors and the message says how many attempts were made, as
/// it does for a spent budget on an S3 error and as the part path does.
#[tokio::test]
async fn upload_exhausted_transport_error_names_the_attempt_count() {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        // Three attempts: the first try and two retries. Fewer would leave
        // this task waiting on accept, so the caller bounds the join.
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            drop(stream);
        }
    });

    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = format!("127.0.0.1:{port}");
    config.general.secure = false;
    let client = IaClient::from_config_no_retry(config).unwrap();

    let f = temp_file(b"hello");
    let err = upload::upload_file(
        &client,
        "test-item",
        f.path(),
        "test.txt",
        &fast_opts(2),
        true,
        true,
        None,
        None,
    )
    .await
    .expect_err("every connection was closed");
    let msg = err.to_string();
    assert!(
        msg.contains("after 3 attempts"),
        "the spent transport budget must name the attempt count: {msg}"
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("one connection per attempt: the listener saw fewer than three")
        .unwrap();
}

// -- Falling back to multipart on an unreliable path (#21) --

/// A client whose IA host is `addr` (a loopback proxy in these tests).
fn client_at(addr: std::net::SocketAddr) -> IaClient {
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = addr.to_string();
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

/// A file one mebibyte above the fallback threshold.
fn large_file() -> NamedTempFile {
    temp_file(&vec![
        0x42u8;
        (MULTIPART_FALLBACK_MIN_SIZE + 1024 * 1024) as usize
    ])
}

/// A file one mebibyte below the fallback threshold.
fn small_file() -> NamedTempFile {
    temp_file(&vec![
        0x42u8;
        (MULTIPART_FALLBACK_MIN_SIZE - 1024 * 1024) as usize
    ])
}

/// The multipart requests for a one-part upload of `/test-item/<key>`:
/// the resume listing (empty), initiate, part 1, complete.
async fn mount_one_part_multipart(server: &MockServer, key: &str, upload_id: &str) {
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
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<InitiateMultipartUploadResult><UploadId>{upload_id}</UploadId></InitiateMultipartUploadResult>"
        )))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("partNumber", "1"))
        .and(query_param("uploadId", upload_id))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploadId", upload_id))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
}

/// An initiate that must never be sent.
async fn mount_no_initiate(server: &MockServer, key: &str) {
    Mock::given(method("POST"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

/// The single PUT of `/test-item/<key>`, expected `times` times.
async fn mount_single_put(server: &MockServer, key: &str, status: u16, times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(status))
        .expect(times)
        .mount(server)
        .await;
}

async fn upload_big(
    client: &IaClient,
    f: &NamedTempFile,
    opts: &UploadOpts,
) -> ia_core::Result<upload::UploadResult> {
    upload::upload_file(
        client,
        "test-item",
        f.path(),
        "big.bin",
        opts,
        true,
        true,
        None,
        None,
    )
    .await
}

/// A single PUT of a file above the threshold that ends in a transport
/// error continues as a multipart upload: no single PUT reaches the server,
/// the one-part upload completes, the result counts the spent attempt, and
/// the run's handle is on for the files that follow.
#[tokio::test]
async fn a_dead_single_put_of_a_large_file_switches_to_multipart() {
    let server = MockServer::start().await;
    mount_one_part_multipart(&server, "big.bin", "fb-1").await;
    mount_single_put(&server, "big.bin", 200, 0).await;
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1).await;

    let opts = fast_opts(3);
    assert!(!opts.multipart_fallback.is_on());
    let f = large_file();
    let result = upload_big(&client_at(proxy), &f, &opts).await.unwrap();

    assert!(
        matches!(result.status, UploadStatus::Uploaded),
        "{result:?}"
    );
    assert_eq!(result.retries, 1);
    assert!(opts.multipart_fallback.is_on());
    server.verify().await;
}

/// A file at or below the threshold is re-sent as a single PUT after a
/// transport error, and the handle stays off.
#[tokio::test]
async fn a_small_file_is_re_sent_as_a_single_put() {
    let server = MockServer::start().await;
    mount_single_put(&server, "big.bin", 200, 1).await;
    mount_no_initiate(&server, "big.bin").await;
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1).await;

    let opts = fast_opts(3);
    let f = small_file();
    let result = upload_big(&client_at(proxy), &f, &opts).await.unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.retries, 1);
    assert!(!opts.multipart_fallback.is_on());
    server.verify().await;
}

/// A 503 throttle is IA's load, not the path: the file stays a single PUT.
#[tokio::test]
async fn a_throttled_large_file_does_not_switch() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/big.bin"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "0")
                .set_body_string("<Error><Code>SlowDown</Code><Message>slow</Message></Error>"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_single_put(&server, "big.bin", 200, 1).await;
    Mock::given(method("GET"))
        .and(query_param("check_limit", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"bucket":"test-item","over_limit":0}"#),
        )
        .mount(&server)
        .await;
    mount_no_initiate(&server, "big.bin").await;

    let opts = fast_opts(3);
    let f = large_file();
    let result = upload_big(&test_client(&server), &f, &opts).await.unwrap();

    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert!(!opts.multipart_fallback.is_on());
    server.verify().await;
}

/// A refusal is the request's fault, not the path's: no switch.
#[tokio::test]
async fn a_refused_large_file_does_not_switch() {
    let server = MockServer::start().await;
    mount_single_put(&server, "big.bin", 403, 1).await;
    mount_no_initiate(&server, "big.bin").await;

    let opts = fast_opts(3);
    let f = large_file();
    let err = upload_big(&test_client(&server), &f, &opts)
        .await
        .unwrap_err();

    assert!(
        matches!(err, ia_core::IaError::UploadFailed { .. }),
        "{err:?}"
    );
    assert!(!opts.multipart_fallback.is_on());
    server.verify().await;
}

/// A spam rejection stays fatal for the item: no switch.
#[tokio::test]
async fn a_spam_rejected_large_file_does_not_switch() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/big.bin"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Your upload appears to be spam."))
        .expect(1)
        .mount(&server)
        .await;
    mount_no_initiate(&server, "big.bin").await;

    let opts = fast_opts(3);
    let f = large_file();
    let err = upload_big(&test_client(&server), &f, &opts)
        .await
        .unwrap_err();

    assert!(
        matches!(err, ia_core::IaError::SpamDetected { .. }),
        "{err:?}"
    );
    assert!(!opts.multipart_fallback.is_on());
    server.verify().await;
}

/// With no retry left there is nothing to continue with: the file fails as
/// it always did.
#[tokio::test]
async fn a_dead_single_put_on_the_last_attempt_does_not_switch() {
    let server = MockServer::start().await;
    mount_no_initiate(&server, "big.bin").await;
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1).await;

    let opts = fast_opts(0);
    let f = large_file();
    let err = upload_big(&client_at(proxy), &f, &opts).await.unwrap_err();

    assert!(
        matches!(err, ia_core::IaError::UploadFailed { .. }),
        "{err:?}"
    );
    assert!(!opts.multipart_fallback.is_on());
    server.verify().await;
}

/// The multipart requests get the retries the single PUT did not spend:
/// with `retries` 2 and one attempt gone, each request has one retry left,
/// so a part that always fails is tried exactly twice.
#[tokio::test]
async fn the_switched_file_keeps_only_the_remaining_budget() {
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
        .and(path("/test-item/big.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>fb-2</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/big.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(500))
        .expect(2)
        .mount(&server)
        .await;
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1).await;

    let opts = fast_opts(2);
    let f = large_file();
    let err = upload_big(&client_at(proxy), &f, &opts).await.unwrap_err();

    let text = err.to_string();
    assert!(text.contains("after 2 attempts"), "{text}");
    assert!(text.contains("fb-2"), "{text}");
    assert!(
        text.contains("rerun the same command with --multipart to resume"),
        "{text}"
    );
    server.verify().await;
}

/// Once the handle is on, a file above the threshold takes the multipart
/// path from the start: the skip check runs as usual and no single PUT is
/// attempted.
#[tokio::test]
async fn a_large_file_after_the_switch_is_multipart_from_the_start() {
    let server = MockServer::start().await;
    mount_one_part_multipart(&server, "big.bin", "fb-3").await;
    mount_single_put(&server, "big.bin", 200, 0).await;
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut opts = fast_opts(3);
    opts.verify = true;
    opts.checksum = true;
    // The handle has no public setter: it is turned on the way a run turns
    // it on, by an earlier file whose single PUT dies. A clone shares it.
    {
        let earlier = MockServer::start().await;
        mount_one_part_multipart(&earlier, "earlier.bin", "earlier-1").await;
        let proxy = support::dropping_proxy(support::mock_addr(&earlier), 1).await;
        let f = large_file();
        // No skip check on this run, so the dropped connection is the PUT.
        let mut shared = opts.clone();
        shared.checksum = false;
        shared.verify = false;
        upload::upload_file(
            &client_at(proxy),
            "test-item",
            f.path(),
            "earlier.bin",
            &shared,
            true,
            true,
            None,
            None,
        )
        .await
        .unwrap();
    }
    assert!(opts.multipart_fallback.is_on());

    let f = large_file();
    let result = upload_big(&test_client(&server), &f, &opts).await.unwrap();

    assert!(
        matches!(result.status, UploadStatus::Uploaded),
        "{result:?}"
    );
    assert_eq!(result.retries, 0);
    server.verify().await;
}
