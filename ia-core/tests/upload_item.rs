//! Integration tests for multi-file item upload (`upload_item`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
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

// -- Single file upload --

#[tokio::test]
async fn upload_item_single_file() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/hello.txt"))
        .and(header("x-archive-auto-make-bucket", "1"))
        .and(header("x-archive-queue-derive", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("hello.txt");
    fs::write(&f, "hello world").unwrap();

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let results = upload::upload_item(
        &client,
        "test-item",
        &[f],
        &opts,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- Multiple files: first/last derive logic --

#[tokio::test]
async fn upload_item_multiple_files() {
    let server = MockServer::start().await;

    // First file gets auto-make-bucket + derive=0
    Mock::given(method("PUT"))
        .and(path("/test-item/a.txt"))
        .and(header("x-archive-auto-make-bucket", "1"))
        .and(header("x-archive-queue-derive", "0"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // Last file gets derive=1, no auto-make-bucket
    Mock::given(method("PUT"))
        .and(path("/test-item/b.txt"))
        .and(header("x-archive-queue-derive", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    fs::write(&f1, "file a").unwrap();
    fs::write(&f2, "file b").unwrap();

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f1, f2], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 2);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
    assert!(matches!(results[1].status, UploadStatus::Uploaded));
}

// -- Directory expansion --

#[tokio::test]
async fn upload_item_expands_directory() {
    let server = MockServer::start().await;

    // Accept any PUT to test-item
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2) // should find 2 files (not the dotfile)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file1.txt"), "content1").unwrap();
    fs::write(dir.path().join("file2.txt"), "content2").unwrap();
    fs::write(dir.path().join(".hidden"), "hidden").unwrap(); // should be skipped

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let results = upload::upload_item(
        &client,
        "test-item",
        &[dir.path().to_path_buf()],
        &opts,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 2);
}

// -- Empty files error --

#[tokio::test]
async fn upload_item_empty_files_error() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let err = upload::upload_item(&client, "test-item", &[], &opts, None)
        .await
        .unwrap_err();

    assert!(matches!(err, ia_core::IaError::EmptyUpload));
}

// -- Invalid identifier --

#[tokio::test]
async fn upload_item_invalid_identifier() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts::default();

    let err = upload::upload_item(&client, "!!", &[], &opts, None)
        .await
        .unwrap_err();

    assert!(matches!(err, ia_core::IaError::InvalidIdentifier { .. }));
}

// -- test_item injects collection --

#[tokio::test]
async fn upload_item_test_item_injects_collection() {
    let server = MockServer::start().await;

    // Should see collection:test_collection in metadata headers
    Mock::given(method("PUT"))
        .and(header("x-archive-meta00-collection", "test_collection"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("test.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        test_item: true,
        metadata: vec![("mediatype".into(), "texts".into())],
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- remote_dir --

#[tokio::test]
async fn upload_item_with_remote_dir() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/scans/file.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        remote_dir: Some("scans".into()),
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
}

// -- Dry run --

#[tokio::test]
async fn upload_item_dry_run() {
    let server = MockServer::start().await;
    // No mocks mounted -- dry run shouldn't make HTTP requests

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        dry_run: true,
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ],
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::DryRun));
}

// -- Missing metadata validation --

#[tokio::test]
async fn upload_item_missing_metadata_error() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    // Has mediatype but missing collection
    let opts = UploadOpts {
        verify: false,
        metadata: vec![("mediatype".into(), "texts".into())],
        ..Default::default()
    };

    let err = upload::upload_item(&client, "test-item", &[f], &opts, None)
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ia_core::IaError::MissingRequiredMetadata { .. }
    ));
}

// -- no_collection_check skips metadata validation --

#[tokio::test]
async fn upload_item_no_collection_check_skips_validation() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    // Missing collection, but no_collection_check=true should skip validation
    let opts = UploadOpts {
        verify: false,
        no_collection_check: true,
        metadata: vec![("mediatype".into(), "texts".into())],
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- Empty metadata skips validation --

#[tokio::test]
async fn upload_item_empty_metadata_skips_validation() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    // No metadata at all -- should not error on missing metadata
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let results =
        upload::upload_item(&client, "test-item", &[f], &opts, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}
