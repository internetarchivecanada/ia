//! Integration tests for multi-file item upload (`upload_item`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
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

// -- Falling back to multipart across a run (#21) --

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

/// The item's listing of unfinished multipart uploads, empty. The answer
/// closes the connection, so the request after it opens a new one; the
/// fallback tests below drop that one to make a single PUT fail.
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

/// The item's listing of unfinished multipart uploads: one per `(key,
/// upload_id)` given.
async fn mount_unfinished_listing(server: &MockServer, uploads: &[(&str, &str)]) {
    let body: String = uploads
        .iter()
        .map(|(key, id)| {
            format!(
                "<Upload><Key>{key}</Key><UploadId>{id}</UploadId>\
                 <Initiated>2026-10-14T12:00:00.000Z</Initiated></Upload>"
            )
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListMultipartUploadsResult>{body}</ListMultipartUploadsResult>"
        )))
        .mount(server)
        .await;
}

/// IA holds the whole file as part 1 of `upload_id`: the parts listing
/// gives its md5 and size, so a resume sends no part and completes.
async fn mount_whole_file_as_part_one(server: &MockServer, key: &str, upload_id: &str, size: u64) {
    use md5::{Digest, Md5};
    let etag = format!("{:x}", Md5::digest(vec![0x42u8; size as usize]));
    Mock::given(method("GET"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploadId", upload_id))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>\"{etag}\"</ETag>\
             <Size>{size}</Size></Part></ListPartsResult>"
        )))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
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

async fn mount_single_put(server: &MockServer, key: &str, times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .expect(times)
        .mount(server)
        .await;
}

fn fallback_opts() -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.checksum = false;
    o.no_collection_check = true;
    o.retries = 3;
    o.retry_min_delay = std::time::Duration::from_millis(1);
    o.retry_max_delay = std::time::Duration::from_millis(2);
    o
}

fn status_of<'a>(results: &'a [upload::UploadResult], key: &str) -> &'a UploadStatus {
    &results.iter().find(|r| r.key == key).unwrap().status
}

/// Sequential run: the first file's single PUT dies, so it continues as
/// multipart; the second file, also above the threshold, is multipart from
/// the start with no single PUT attempted; the third, below the threshold,
/// is a single PUT.
#[tokio::test]
async fn a_run_switches_later_large_files_after_the_first_dead_send() {
    let server = MockServer::start().await;
    mount_empty_resume_listing(&server).await;
    mount_one_part_multipart(&server, "a.bin", "run-a").await;
    mount_one_part_multipart(&server, "b.bin", "run-b").await;
    mount_single_put(&server, "a.bin", 0).await;
    mount_single_put(&server, "b.bin", 0).await;
    mount_single_put(&server, "c.bin", 1).await;
    // The listing is the first connection; the single PUT of a.bin is the second.
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1, 1).await;

    let dir = TempDir::new().unwrap();
    let big = MULTIPART_FALLBACK_MIN_SIZE + 1024 * 1024;
    let small = MULTIPART_FALLBACK_MIN_SIZE - 1024 * 1024;
    let files = [
        write_sized(&dir, "a.bin", big),
        write_sized(&dir, "b.bin", big),
        write_sized(&dir, "c.bin", small),
    ];

    let opts = fallback_opts();
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

    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(matches!(r.status, UploadStatus::Uploaded), "{r:?}");
    }
    assert_eq!(results[0].retries, 1);
    assert_eq!(results[1].retries, 0);
    assert!(opts.multipart_fallback.is_on());
    server.verify().await;
}

/// Concurrent run: the first file's single PUT dies before the middle
/// files start, so every later file above the threshold is multipart from
/// the start and the small middle file stays a single PUT.
#[tokio::test]
async fn a_concurrent_run_switches_the_files_not_yet_started() {
    let server = MockServer::start().await;
    mount_empty_resume_listing(&server).await;
    mount_one_part_multipart(&server, "a.bin", "con-a").await;
    mount_one_part_multipart(&server, "c.bin", "con-c").await;
    mount_one_part_multipart(&server, "d.bin", "con-d").await;
    mount_single_put(&server, "a.bin", 0).await;
    mount_single_put(&server, "b.bin", 1).await;
    mount_single_put(&server, "c.bin", 0).await;
    mount_single_put(&server, "d.bin", 0).await;
    // The listing is the first connection; the single PUT of a.bin is the second.
    let proxy = support::dropping_proxy(support::mock_addr(&server), 1, 1).await;

    let dir = TempDir::new().unwrap();
    let big = MULTIPART_FALLBACK_MIN_SIZE + 1024 * 1024;
    let small = MULTIPART_FALLBACK_MIN_SIZE - 1024 * 1024;
    let files = [
        write_sized(&dir, "a.bin", big),
        write_sized(&dir, "b.bin", small),
        write_sized(&dir, "c.bin", big),
        write_sized(&dir, "d.bin", big),
    ];

    let opts = fallback_opts();
    let results = upload::upload_item(
        &client_at(proxy),
        "test-item",
        &files,
        &opts,
        None,
        None,
        None,
        2,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 4);
    for key in ["a.bin", "b.bin", "c.bin", "d.bin"] {
        assert!(
            matches!(status_of(&results, key), UploadStatus::Uploaded),
            "{key}: {results:?}"
        );
    }
    assert!(opts.multipart_fallback.is_on());
    server.verify().await;
}

// -- A rerun resumes a switched file without --multipart (#21) --

/// The item's listing at the start of the run names unfinished uploads for
/// a.bin (above the fallback threshold) and b.bin (below it), and IA holds
/// each whole file as part 1. Both resume as multipart with `multipart`
/// off: no single PUT, no initiate, no part is sent, and each completes.
/// c.bin, with no unfinished upload, is a single PUT. The run's switch stays
/// off: nothing died.
#[tokio::test]
async fn a_rerun_resumes_the_files_with_unfinished_uploads_without_the_flag() {
    let server = MockServer::start().await;
    let big = MULTIPART_FALLBACK_MIN_SIZE + 1024 * 1024;
    let small = MULTIPART_FALLBACK_MIN_SIZE - 1024 * 1024;
    mount_unfinished_listing(&server, &[("a.bin", "kept-a"), ("b.bin", "kept-b")]).await;
    mount_whole_file_as_part_one(&server, "a.bin", "kept-a", big).await;
    mount_whole_file_as_part_one(&server, "b.bin", "kept-b", small).await;
    mount_single_put(&server, "a.bin", 0).await;
    mount_single_put(&server, "b.bin", 0).await;
    mount_single_put(&server, "c.bin", 1).await;

    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", big),
        write_sized(&dir, "b.bin", small),
        write_sized(&dir, "c.bin", small),
    ];

    let opts = fallback_opts();
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

    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(matches!(r.status, UploadStatus::Uploaded), "{r:?}");
    }
    assert!(!opts.multipart_fallback.is_on());
    assert!(opts.multipart_fallback.has_unfinished("test-item", "a.bin"));
    assert!(opts.multipart_fallback.has_unfinished("test-item", "b.bin"));
    assert!(!opts.multipart_fallback.has_unfinished("test-item", "c.bin"));
    assert!(!opts
        .multipart_fallback
        .has_unfinished("other-item", "a.bin"));
    server.verify().await;
}

/// A listing that fails for good does not fail the run: the files are sent
/// as single PUTs, as a run without the listing would send them.
#[tokio::test]
async fn a_failed_listing_leaves_the_run_on_single_puts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(4)
        .mount(&server)
        .await;
    mount_single_put(&server, "a.bin", 1).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(
        &dir,
        "a.bin",
        MULTIPART_FALLBACK_MIN_SIZE + 1024,
    )];

    let opts = fallback_opts();
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
    assert!(!opts.multipart_fallback.has_unfinished("test-item", "a.bin"));
    server.verify().await;
}

/// A brand-new item has no bucket to list; nothing is resumed.
#[tokio::test]
async fn a_new_item_has_nothing_to_resume() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist.</Message></Error>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    mount_single_put(&server, "a.bin", 1).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 1024)];

    let results = upload::upload_item(
        &test_client(&server),
        "test-item",
        &files,
        &fallback_opts(),
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

/// A dry run sends nothing, the listing included.
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
    let mut opts = fallback_opts();
    opts.dry_run = true;

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

/// With `multipart` on, every file lists for itself before its parts; the
/// item is not listed a second time at the start of the run.
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
    let mut opts = fallback_opts();
    opts.multipart = true;

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
