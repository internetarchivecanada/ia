//! Integration tests for single file upload (`upload_file`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::io::Write;
use tempfile::NamedTempFile;
use wiremock::matchers::{header, header_exists, method, path};
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    assert!(result.md5.is_none()); // verify=false
}

// -- Dry run --

#[tokio::test]
async fn upload_dry_run_no_http() {
    let server = MockServer::start().await;
    // Don't mount any mocks -- any request would cause unexpected behavior

    let f = temp_file(b"test data");
    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        dry_run: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        no_backup: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        no_derive: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        metadata: vec![("mediatype".into(), "texts".into())],
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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

    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        headers: vec![("x-custom-header".into(), "custom-value".into())],
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        delete_after_upload: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: true,
        ..Default::default()
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
    use std::time::Duration;

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
    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_sleep: Duration::from_millis(10), // fast for tests
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
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
    use std::time::Duration;

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
    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_sleep: Duration::from_millis(1),
        ..Default::default()
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
async fn upload_400_bad_digest_is_not_retried() {
    use std::time::Duration;

    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>",
        ))
        .expect(1) // exactly 1 request — no retries
        .mount(&server)
        .await;

    let f = temp_file(b"hello");
    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_sleep: Duration::from_millis(1),
        ..Default::default()
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
    assert!(result.unwrap_err().to_string().contains("BadDigest"));
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
    let opts = UploadOpts {
        checksum: true,
        verify: true,
        ..Default::default()
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
    let opts = UploadOpts {
        checksum: true,
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        checksum: true,
        verify: false,
        ..Default::default()
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
    let opts = UploadOpts {
        checksum: true,
        verify: false, // no Content-MD5 header, but still compute for skip
        ..Default::default()
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

    let opts = UploadOpts {
        verify: true,
        checksum_file: Some(checksum_file),
        ..UploadOpts::default()
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
    use std::time::Duration;

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
    let opts = UploadOpts {
        verify: false,
        retries: 3,
        retry_sleep: Duration::from_millis(10),
        ..Default::default()
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
    use std::time::Duration;

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
    let opts = UploadOpts {
        verify: false,
        retries: 2,
        retry_sleep: Duration::from_millis(10),
        ..Default::default()
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
    let opts = UploadOpts {
        verify: false,
        no_auto_make_bucket: true,
        ..Default::default()
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
    let opts = UploadOpts {
        verify: true,
        dry_run: true,
        ..Default::default()
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
