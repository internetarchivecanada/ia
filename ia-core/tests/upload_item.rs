//! Integration tests for multi-file item upload (`upload_item`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig, IaError};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

/// 3 MiB: well under the 100 MiB multipart cutoff.
const BIG: u64 = 3 * 1024 * 1024;
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(
        &client,
        "test-item",
        &[f1, f2],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(
        &client,
        "test-item",
        &[dir.path().to_path_buf()],
        &opts,
        None,
        None,
        None,
        1,
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let err = upload::upload_item(&client, "test-item", &[], &opts, None, None, None, 1, None)
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

    let err = upload::upload_item(&client, "!!", &[], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.test_item = true;
        o.no_collection_check = true;
        o.metadata = vec![("mediatype".into(), "texts".into())];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.remote_dir = Some("scans".into());
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.dry_run = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.metadata = vec![("mediatype".into(), "texts".into())];
        o
    };

    let err = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = vec![("mediatype".into(), "texts".into())];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- Collection existence check --

#[tokio::test]
async fn upload_item_checks_collection_exists() {
    let server = MockServer::start().await;

    // Mock metadata endpoint — return 200 with empty JSON (item not found → 404-like)
    Mock::given(method("GET"))
        .and(path("/metadata/nonexistent-collection"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Item cannot be found"))
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("test.txt");
    fs::write(&f, "hello").unwrap();

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = false;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "nonexistent-collection".into()),
        ];
        o
    };

    let err = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ia_core::IaError::CollectionNotFound { .. }),
        "expected CollectionNotFound, got: {err}"
    );
}

#[tokio::test]
async fn upload_item_collection_check_passes_when_exists() {
    let server = MockServer::start().await;

    // Mock metadata endpoint — collection exists
    Mock::given(method("GET"))
        .and(path("/metadata/test_collection"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test_collection"},
            "files": []
        })))
        .mount(&server)
        .await;

    // Accept upload
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("test.txt");
    fs::write(&f, "hello").unwrap();

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = false;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- keep_directories preserves path in PUT URL --

#[tokio::test]
async fn upload_item_keep_directories_preserves_path() {
    let server = MockServer::start().await;

    // Accept any PUT
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).unwrap();
    let f = sub.join("deep_file.txt");
    fs::write(&f, "deep content").unwrap();

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.keep_directories = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    // Pass the directory so expand_files walks it; keep_directories uses the full path
    let results = upload::upload_item(
        &client,
        "test-item",
        &[dir.path().to_path_buf()],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));

    // Verify the PUT path includes the subdirectory component
    let requests = server.received_requests().await.unwrap();
    let put_req = requests
        .iter()
        .find(|r| r.method.as_str() == "PUT")
        .unwrap();
    let req_path = put_req.url.path();
    assert!(
        req_path.contains("subdir") && req_path.contains("deep_file.txt"),
        "keep_directories should preserve subdir in PUT path, got: {req_path}"
    );
}

// -- remote_name changes PUT path --

#[tokio::test]
async fn upload_item_remote_name_changes_put_path() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/test-item/custom.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("original.txt");
    fs::write(&f, "renamed content").unwrap();

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.remote_name = Some("custom.txt".into());
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
    assert_eq!(results[0].key, "custom.txt");
}

// -- test_item replaces existing collection metadata --

#[tokio::test]
async fn upload_item_test_item_replaces_existing_collection() {
    let server = MockServer::start().await;

    // Expect the metadata header to contain test_collection, NOT my-real-collection
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
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.test_item = true;
        o.no_collection_check = true;
        o.metadata = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "my-real-collection".into()),
        ];
        o
    };

    let results = upload::upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

// -- Shared helpers for the multipart tests --

fn client_at(addr: std::net::SocketAddr) -> IaClient {
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = addr.to_string();
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

fn write_sized(dir: &TempDir, name: &str, size: u64) -> std::path::PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, vec![0x42u8; size as usize]).unwrap();
    p
}

/// The multipart requests for a one-part upload of `/test-item/<key>`.
async fn mount_one_part_multipart(server: &MockServer, key: &str, upload_id: &str) {
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

/// The item's listing of unfinished multipart uploads, empty.
async fn mount_empty_resume_listing(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("connection", "close")
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .mount(server)
        .await;
}

async fn mount_single_put(server: &MockServer, key: &str, times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .expect(times)
        .mount(server)
        .await;
}

fn fast_opts() -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.checksum = false;
    o.no_collection_check = true;
    o.retries = 3;
    o.retry_min_delay = std::time::Duration::from_millis(1);
    o.retry_max_delay = std::time::Duration::from_millis(2);
    o
}

/// A multipart dry run sends nothing, the per-file listing included.
#[tokio::test]
async fn a_dry_run_sends_no_listing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 1024)];
    let mut opts = fast_opts();
    opts.dry_run = true;
    opts.multipart = upload::Multipart::Always;

    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert!(
        matches!(results[0].status, UploadStatus::DryRun),
        "{results:?}"
    );
    server.verify().await;
}

/// With `multipart` on, a file lists the item's unfinished uploads once,
/// before its parts.
#[tokio::test]
async fn a_multipart_run_lists_once_per_file_not_also_per_item() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_one_part_multipart(&server, "a.bin", "mp-a").await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 1024)];
    let mut opts = fast_opts();
    opts.multipart = ia_core::upload::Multipart::Always;

    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    server.verify().await;
}

// -- Multipart::Never: every file a single PUT (#179) --

/// No multipart request of any kind reaches the item: no listing of
/// unfinished uploads, no initiate or completion (POST), no abort
/// (DELETE). A part PUT needs an upload ID from one of those first.
async fn mount_no_multipart_requests(server: &MockServer) {
    Mock::given(method("GET"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

fn never_opts() -> UploadOpts {
    let mut o = fast_opts();
    o.multipart = upload::Multipart::Never;
    o
}

/// The first file's single PUT dies (its connection is dropped); it is
/// sent again as a single PUT, not continued as multipart. The second
/// file is a single PUT too. Nothing is listed.
#[tokio::test]
async fn never_retries_a_dead_single_put_as_a_single_put() {
    let server = MockServer::start().await;
    mount_no_multipart_requests(&server).await;
    mount_single_put(&server, "a.bin", 1).await;
    mount_single_put(&server, "b.bin", 1).await;
    // The first connection is the single PUT of a.bin: no listing before it.
    let proxy = support::dropping_proxy(support::mock_addr(&server), 0, 1).await;

    let dir = TempDir::new().unwrap();
    let big = BIG;
    let files = [
        write_sized(&dir, "a.bin", big),
        write_sized(&dir, "b.bin", big),
    ];

    let opts = never_opts();
    let results = upload::upload_item(
        &client_at(proxy),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 2);
    for r in &results {
        assert!(matches!(r.status, UploadStatus::Uploaded), "{r:?}");
    }
    assert_eq!(results[0].retries, 1);
    assert_eq!(results[1].retries, 0);
    server.verify().await;
}

/// A single PUT whose every send dies fails with the
/// single-PUT message once its retries are spent; nothing multipart is sent.
#[tokio::test]
async fn never_fails_a_dead_single_put_with_the_single_put_error() {
    let server = MockServer::start().await;
    mount_no_multipart_requests(&server).await;
    mount_single_put(&server, "a.bin", 0).await;
    let proxy = support::dropping_proxy(support::mock_addr(&server), 0, 100).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", BIG)];

    let opts = never_opts();
    let results = upload::upload_item(
        &client_at(proxy),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    let UploadStatus::Failed(message) = &results[0].status else {
        panic!("{results:?}");
    };
    assert!(message.contains("test-item/a.bin"), "{message}");
    assert!(message.contains("after 4 attempts"), "{message}");
    assert!(!message.contains("multipart"), "{message}");
    server.verify().await;
}

/// A rerun on an item where IA holds an unfinished multipart upload of the
/// file: the file is a single PUT, and the item is not listed.
#[tokio::test]
async fn never_sends_a_single_put_where_an_unfinished_upload_is_held() {
    let server = MockServer::start().await;
    // IA holds an unfinished upload of a.bin: the listing would name it.
    // Mounted first, so it is the one that answers a listing; it expects
    // none.
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult><Upload><Key>a.bin</Key><UploadId>kept-a</UploadId>\
             <Initiated>2026-10-14T12:00:00.000Z</Initiated></Upload></ListMultipartUploadsResult>",
        ))
        .expect(0)
        .mount(&server)
        .await;
    mount_no_multipart_requests(&server).await;
    mount_single_put(&server, "a.bin", 1).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", BIG)];

    let opts = never_opts();
    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    server.verify().await;
}

// -- Remote keys stay inside the item (#96) --

/// Options for the key tests: no verification, no collection check.
fn key_test_opts() -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.no_collection_check = true;
    o
}

/// Accept any PUT, `times` times.
async fn mount_any_put(server: &MockServer, times: u64) {
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(times)
        .mount(server)
        .await;
}

/// The path of every PUT the server received, after the checks that each
/// stays inside `identifier`: it starts with `/<identifier>/` and has no
/// empty segment.
async fn put_paths_inside(server: &MockServer, identifier: &str) -> Vec<String> {
    let prefix = format!("/{identifier}/");
    let paths: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .map(|r| r.url.path().to_string())
        .collect();
    for p in &paths {
        assert!(p.starts_with(&prefix), "PUT left the item: {p}");
        assert!(
            !p[1..].split('/').any(str::is_empty),
            "PUT has an empty segment: {p}"
        );
    }
    paths
}

#[tokio::test]
async fn keep_directories_with_a_dotdot_argument_stays_in_the_item() {
    let server = MockServer::start().await;
    mount_any_put(&server, 1).await;

    // Integration tests run in the package directory, so a directory made in
    // its parent (the workspace root) is reachable as `../<name>`.
    let dir = tempfile::Builder::new()
        .prefix("upload-keys-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
        .unwrap();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let scans = dir.path().join("scans");
    fs::create_dir(&scans).unwrap();
    fs::write(scans.join("a.bin"), "a").unwrap();
    let arg = PathBuf::from(format!("../{name}/scans"));

    let mut opts = key_test_opts();
    opts.keep_directories = true;
    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &[arg],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results[0].key, "scans/a.bin");
    assert_eq!(
        put_paths_inside(&server, "test-item").await,
        ["/test-item/scans/a.bin"]
    );
    server.verify().await;
}

#[tokio::test]
async fn keep_directories_with_an_absolute_argument_stays_in_the_item() {
    let server = MockServer::start().await;
    mount_any_put(&server, 1).await;

    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("scans").join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("a.bin"), "a").unwrap();

    let mut opts = key_test_opts();
    opts.keep_directories = true;
    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &[dir.path().join("scans")],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results[0].key, "scans/sub/a.bin");
    assert_eq!(
        put_paths_inside(&server, "test-item").await,
        ["/test-item/scans/sub/a.bin"]
    );
    server.verify().await;
}

#[tokio::test]
async fn keep_directories_with_a_relative_argument_keeps_its_path() {
    let server = MockServer::start().await;
    mount_any_put(&server, 1).await;

    // Integration tests run in the package directory, so a directory made
    // there is reachable by a plain relative path.
    let dir = tempfile::Builder::new()
        .prefix("upload-keys-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let sub = dir.path().join("scans").join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("a.bin"), "a").unwrap();
    let arg = PathBuf::from(format!("./{name}/scans"));

    let mut opts = key_test_opts();
    opts.keep_directories = true;
    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &[arg],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results[0].key, format!("{name}/scans/sub/a.bin"));
    assert_eq!(
        put_paths_inside(&server, "test-item").await,
        [format!("/test-item/{name}/scans/sub/a.bin")]
    );
    server.verify().await;
}

#[tokio::test]
async fn a_remote_name_that_leaves_the_item_is_refused_before_any_request() {
    let server = MockServer::start().await;
    mount_any_put(&server, 0).await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.bin");
    fs::write(&f, "a").unwrap();

    // With the collection check on: the key is refused before it is sent.
    let mut opts = key_test_opts();
    opts.no_collection_check = false;
    opts.metadata = vec![
        ("mediatype".into(), "texts".into()),
        ("collection".into(), "test_collection".into()),
    ];
    opts.remote_name = Some("../other-item/a.bin".into());
    let err = upload::upload_item(
        &test_client(&server),
        "test-item",
        &[f],
        &opts,
        None,
        None,
        None,
        1,
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, IaError::InvalidKey { .. }), "{err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// `upload_file` is public and takes its key from the caller, so the check
/// in the URL builder must refuse a bad key there too.
#[tokio::test]
async fn upload_file_refuses_a_key_that_leaves_the_item() {
    let server = MockServer::start().await;
    mount_any_put(&server, 0).await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.bin");
    fs::write(&f, "a").unwrap();

    let err = upload::upload_file(
        &test_client(&server),
        "test-item",
        &f,
        "../other-item/a.bin",
        &key_test_opts(),
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, IaError::InvalidKey { .. }), "{err:?}");
    // The skip check may read the item's metadata first; no PUT is sent.
    assert!(put_paths_inside(&server, "test-item").await.is_empty());
}

/// The multipart requests build their URLs the same way, so a bad key is
/// refused there too, and no request names it.
#[tokio::test]
async fn a_multipart_upload_refuses_a_key_that_leaves_the_item() {
    let server = MockServer::start().await;
    mount_any_put(&server, 0).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    // The resume check lists the item's uploads first; that names no key.
    mount_empty_resume_listing(&server).await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.bin");
    fs::write(&f, "a").unwrap();

    let mut opts = key_test_opts();
    opts.multipart = ia_core::upload::Multipart::Always;
    let err = upload::upload_file(
        &test_client(&server),
        "test-item",
        &f,
        "../other-item/a.bin",
        &opts,
        true,
        true,
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, IaError::InvalidKey { .. }), "{err:?}");
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|r| !r.url.path().contains("a.bin")),
        "a request named the key: {:?}",
        requests
            .iter()
            .map(|r| r.url.to_string())
            .collect::<Vec<_>>()
    );
    server.verify().await;
}

#[test]
fn invalid_key_has_its_own_json_code() {
    let err = IaError::InvalidKey {
        key: "../a.bin".into(),
        reason: "it has a '..' segment".into(),
    };
    assert!(!err.is_retryable());
    let json = serde_json::to_value(err.to_json_error()).unwrap();
    assert_eq!(json["error"]["code"], "invalid_key");
    assert_eq!(json["error"]["key"], "../a.bin");
    assert_eq!(json["error"]["reason"], "it has a '..' segment");
}

#[test]
fn validate_key_refuses_keys_that_do_not_name_a_file_in_the_item() {
    let refused = [
        ("", "it is empty"),
        ("/a.bin", "it starts with '/'"),
        ("/data/scans/a.bin", "it starts with '/'"),
        ("..", "it has a '..' segment"),
        ("../scans/a.bin", "it has a '..' segment"),
        ("scans/../../a.bin", "it has a '..' segment"),
        (".", "it has a '.' segment"),
        ("./a.bin", "it has a '.' segment"),
        ("scans/./a.bin", "it has a '.' segment"),
        ("scans//a.bin", "it has an empty segment"),
        ("scans/", "it has an empty segment"),
    ];
    for (key, reason) in refused {
        let err = upload::validate_key(key).unwrap_err();
        assert!(matches!(err, IaError::InvalidKey { .. }), "{key}: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains(&format!("'{key}'")), "{key}: {msg}");
        assert!(msg.contains(reason), "{key}: {msg}");
        assert!(
            msg.contains("a remote name is a path inside the item"),
            "{key}: {msg}"
        );
    }

    for key in [
        "a.bin",
        "scans/sub/a.bin",
        ".hidden",
        "...",
        "a..b/c.bin",
        "my file.pdf",
    ] {
        assert!(upload::validate_key(key).is_ok(), "{key}");
    }
}

// -- --remote-dir with surrounding slashes (#100) --

#[tokio::test]
async fn remote_dir_with_surrounding_slashes_gives_one_separator() {
    for remote_dir in ["scans/", "/scans/", "scans"] {
        let server = MockServer::start().await;
        mount_any_put(&server, 1).await;

        let dir = TempDir::new().unwrap();
        let f = dir.path().join("a.bin");
        fs::write(&f, "a").unwrap();

        let mut opts = key_test_opts();
        opts.remote_dir = Some(remote_dir.into());
        let results = upload::upload_item(
            &test_client(&server),
            "test-item",
            &[f],
            &opts,
            None,
            None,
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results[0].key, "scans/a.bin", "--remote-dir {remote_dir}");
        assert_eq!(
            put_paths_inside(&server, "test-item").await,
            ["/test-item/scans/a.bin"],
            "--remote-dir {remote_dir}"
        );
        server.verify().await;
    }
}
