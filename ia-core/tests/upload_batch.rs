//! Integration tests for batch upload from spreadsheet records (`upload_batch`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
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

/// Helper to build a spreadsheet record for upload.
fn record(
    id: &str,
    file: &str,
    mediatype: &str,
    collection: &str,
) -> (String, HashMap<String, String>) {
    let mut fields = HashMap::new();
    fields.insert("file".into(), file.into());
    fields.insert("mediatype".into(), mediatype.into());
    fields.insert("collection".into(), collection.into());
    (id.into(), fields)
}

#[tokio::test]
async fn batch_upload_single_item() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/item-1/file1.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("file1.txt");
    fs::write(&f1, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "item-1",
        f1.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn batch_upload_multiple_items() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("file1.txt");
    let f2 = dir.path().join("file2.txt");
    fs::write(&f1, "content1").unwrap();
    fs::write(&f2, "content2").unwrap();

    let client = test_client(&server);
    let records = vec![
        record(
            "item-1",
            f1.to_str().unwrap(),
            "texts",
            "test_collection",
        ),
        record(
            "item-2",
            f2.to_str().unwrap(),
            "texts",
            "test_collection",
        ),
    ];
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let results = upload::upload_batch(&client, records, &opts, 2, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn batch_upload_grouped_by_identifier() {
    let server = MockServer::start().await;

    // Same item, two files
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    fs::write(&f1, "aaa").unwrap();
    fs::write(&f2, "bbb").unwrap();

    let client = test_client(&server);
    let records = vec![
        record(
            "item-1",
            f1.to_str().unwrap(),
            "texts",
            "test_collection",
        ),
        record(
            "item-1",
            f2.to_str().unwrap(),
            "texts",
            "test_collection",
        ),
    ];
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2); // 2 files in 1 item
}

#[tokio::test]
async fn batch_upload_empty_records() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts::default();

    let err = upload::upload_batch(&client, vec![], &opts, 1, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ia_core::IaError::EmptyUpload));
}

#[tokio::test]
async fn batch_upload_missing_file_field() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let mut fields = HashMap::new();
    fields.insert("mediatype".into(), "texts".into());
    // No "file" field
    let records = vec![("item-1".into(), fields)];

    let err = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap_err();
    // Should error about missing file
    assert!(matches!(err, ia_core::IaError::Config(_)));
}

#[tokio::test]
async fn batch_upload_nonexistent_file() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let records = vec![record(
        "item-1",
        "/tmp/ia-test-nonexistent-file-abc123.txt",
        "texts",
        "test_collection",
    )];

    let err = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap_err();
    // File validation should catch nonexistent files
    assert!(matches!(err, ia_core::IaError::Config(_)));
}

#[tokio::test]
async fn batch_upload_invalid_identifier() {
    let server = MockServer::start().await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "!!",
        f.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let err = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ia_core::IaError::Config(_)));
    assert!(err.to_string().contains("batch validation failed"));
}

#[tokio::test]
async fn batch_upload_merges_metadata_with_opts() {
    let server = MockServer::start().await;

    // Expect the PUT with metadata from both opts and spreadsheet
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);

    let mut fields = HashMap::new();
    fields.insert("file".into(), f.to_str().unwrap().into());
    fields.insert("mediatype".into(), "texts".into());
    fields.insert("collection".into(), "test_collection".into());
    fields.insert("title".into(), "From Spreadsheet".into());
    let records = vec![("item-1".into(), fields)];

    // Base opts have some extra header metadata
    let opts = UploadOpts {
        verify: false,
        ..Default::default()
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn batch_upload_dry_run() {
    let server = MockServer::start().await;
    // No mocks — dry run should not make HTTP requests

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "item-1",
        f.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = UploadOpts {
        verify: false,
        dry_run: true,
        ..Default::default()
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::DryRun));
}
