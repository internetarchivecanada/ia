//! Integration tests for `x-archive-size-hint` across a whole item upload
//! (`upload_item`): which request carries it, with which value, and how
//! `size_hint`, `no_size_hint` and a `--header` value interact.

use std::collections::HashSet;
use std::path::PathBuf;

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The value TV archive uploads pass, agreed with IA operations.
const TV_HINT: u64 = 2_000_000_499_999_744;

fn test_client(server: &MockServer) -> IaClient {
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = host_port;
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

fn opts() -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.checksum = false;
    o.no_collection_check = true;
    o.retries = 1;
    o.retry_min_delay = std::time::Duration::from_millis(1);
    o.retry_max_delay = std::time::Duration::from_millis(2);
    o
}

fn write_sized(dir: &TempDir, name: &str, size: usize) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, vec![0x42u8; size]).unwrap();
    p
}

/// Accept every single PUT.
async fn mount_puts(server: &MockServer) {
    Mock::given(method("PUT"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// Every `x-archive-size-hint` value the server received on the single PUT
/// of `/test-item/<key>`, one entry per header line.
async fn put_hints(server: &MockServer, key: &str) -> Vec<String> {
    request_hints(server, "PUT", key, |q| !q.contains("partNumber")).await
}

/// Every `x-archive-size-hint` value on requests with this method and key
/// whose query passes `query_ok`, flattened over the matching requests.
async fn request_hints(
    server: &MockServer,
    verb: &str,
    key: &str,
    query_ok: impl Fn(&str) -> bool,
) -> Vec<String> {
    let want = format!("/test-item/{key}");
    let requests = server.received_requests().await.unwrap();
    let matching: Vec<_> = requests
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == want)
        .filter(|r| query_ok(r.url.query().unwrap_or("")))
        .collect();
    assert!(
        !matching.is_empty(),
        "no {verb} {want} received; got {:?}",
        requests
            .iter()
            .map(|r| format!("{} {}", r.method, r.url))
            .collect::<Vec<_>>()
    );
    matching
        .iter()
        .flat_map(|r| {
            r.headers
                .get_all("x-archive-size-hint")
                .iter()
                .map(|v| v.to_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

async fn run(
    server: &MockServer,
    files: &[PathBuf],
    opts: &UploadOpts,
    skip: Option<&HashSet<(String, String)>>,
    file_concurrency: usize,
) -> Vec<upload::UploadResult> {
    upload::upload_item(
        &test_client(server),
        "test-item",
        files,
        opts,
        None,
        skip,
        None,
        file_concurrency,
        None,
    )
    .await
    .unwrap()
}

// -- The computed hint --

/// The hint is the total size of every file in the item, not the size of
/// the file that carries it, and only the first file carries it.
#[tokio::test]
async fn the_hint_is_the_total_of_all_files_on_the_first_file_only() {
    let server = MockServer::start().await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 20),
        write_sized(&dir, "c.bin", 30),
    ];

    let results = run(&server, &files, &opts(), None, 1).await;

    assert!(results
        .iter()
        .all(|r| matches!(r.status, UploadStatus::Uploaded)));
    assert_eq!(put_hints(&server, "a.bin").await, ["60"]);
    assert!(put_hints(&server, "b.bin").await.is_empty());
    assert!(put_hints(&server, "c.bin").await.is_empty());
}

/// On a resumed run whose first file was finished before, the hint goes on
/// the first file that actually uploads.
#[tokio::test]
async fn a_resumed_run_puts_the_hint_on_the_first_file_that_uploads() {
    let server = MockServer::start().await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 20),
        write_sized(&dir, "c.bin", 30),
    ];
    let mut skip = HashSet::new();
    skip.insert(("test-item".to_string(), "a.bin".to_string()));

    let results = run(&server, &files, &opts(), Some(&skip), 1).await;

    assert!(matches!(results[0].status, UploadStatus::Resumed));
    let puts: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .map(|r| r.url.path().to_string())
        .collect();
    assert!(!puts.contains(&"/test-item/a.bin".to_string()), "{puts:?}");
    assert_eq!(put_hints(&server, "b.bin").await, ["60"]);
    assert!(put_hints(&server, "c.bin").await.is_empty());
}

/// `no_size_hint` sends no hint on any file.
#[tokio::test]
async fn no_size_hint_sends_no_hint_on_any_file() {
    let server = MockServer::start().await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 20),
        write_sized(&dir, "c.bin", 30),
    ];
    let mut o = opts();
    o.no_size_hint = true;

    run(&server, &files, &o, None, 1).await;

    for key in ["a.bin", "b.bin", "c.bin"] {
        assert!(put_hints(&server, key).await.is_empty(), "{key}");
    }
}

/// With files uploading concurrently, a first file that fails leaves the
/// item to be created by the last file (the one that then carries
/// `x-archive-auto-make-bucket`); that file carries the hint too.
#[tokio::test]
async fn a_concurrent_run_puts_the_hint_on_the_file_that_creates_the_item() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/test-item/a.bin"))
        .respond_with(ResponseTemplate::new(400).set_body_string("<Error/>"))
        .mount(&server)
        .await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 20),
        write_sized(&dir, "c.bin", 30),
    ];

    let results = run(&server, &files, &opts(), None, 2).await;

    assert!(
        matches!(results[0].status, UploadStatus::Failed(_)),
        "{results:?}"
    );
    let requests = server.received_requests().await.unwrap();
    let last = requests
        .iter()
        .find(|r| r.method.as_str() == "PUT" && r.url.path() == "/test-item/c.bin")
        .unwrap();
    assert_eq!(
        last.headers
            .get("x-archive-auto-make-bucket")
            .map(|v| v.to_str().unwrap()),
        Some("1"),
        "the last file creates the item"
    );
    assert_eq!(put_hints(&server, "c.bin").await, ["60"]);
}

// -- size_hint --

/// `size_hint` replaces the computed total, passed through exactly, on the
/// first file only.
#[tokio::test]
async fn size_hint_replaces_the_total_on_the_first_file() {
    let server = MockServer::start().await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 20),
    ];
    let mut o = opts();
    o.size_hint = Some(TV_HINT);

    run(&server, &files, &o, None, 1).await;

    assert_eq!(put_hints(&server, "a.bin").await, ["2000000499999744"]);
    assert!(put_hints(&server, "b.bin").await.is_empty());
}

/// `size_hint` on a multipart upload: the initiate and the completion
/// request both carry the given value, once each.
#[tokio::test]
async fn size_hint_on_multipart_initiate_and_completion() {
    let server = MockServer::start().await;
    // The per-file listing of unfinished uploads: none.
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
        .and(path("/test-item/a.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>up-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/a.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploadId", "up-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 10)];
    let mut o = opts();
    o.multipart = true;
    o.size_hint = Some(TV_HINT);

    let results = run(&server, &files, &o, None, 1).await;

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    let initiate = request_hints(&server, "POST", "a.bin", |q| q.starts_with("uploads")).await;
    let complete = request_hints(&server, "POST", "a.bin", |q| q.contains("uploadId")).await;
    assert_eq!(initiate, ["2000000499999744"]);
    assert_eq!(complete, ["2000000499999744"]);
    server.verify().await;
}

/// A multipart upload resumed from parts already on IA sends no initiate
/// request; its completion request carries the given value.
#[tokio::test]
async fn size_hint_on_a_resumed_multipart_completion() {
    use md5::{Digest, Md5};
    let server = MockServer::start().await;
    let size = 10usize;
    let etag: String = Md5::digest(vec![0x42u8; size])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListMultipartUploadsResult><Upload><Key>a.bin</Key><UploadId>kept</UploadId>\
             <Initiated>2026-10-14T12:00:00.000Z</Initiated></Upload></ListMultipartUploadsResult>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploadId", "kept"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>\"{etag}\"</ETag>\
             <Size>{size}</Size></Part></ListPartsResult>"
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploadId", "kept"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", size)];
    let mut o = opts();
    o.multipart = true;
    o.size_hint = Some(TV_HINT);

    let results = run(&server, &files, &o, None, 1).await;

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    let complete = request_hints(&server, "POST", "a.bin", |q| q.contains("uploadId")).await;
    assert_eq!(complete, ["2000000499999744"]);
    server.verify().await;
}

// -- A --header x-archive-size-hint --

/// A custom `x-archive-size-hint` header replaces the computed hint on the
/// single PUT instead of adding a second header line.
#[tokio::test]
async fn a_size_hint_header_replaces_the_computed_hint_on_a_single_put() {
    let server = MockServer::start().await;
    mount_puts(&server).await;
    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 10)];
    let mut o = opts();
    o.headers = vec![("X-Archive-Size-Hint".into(), "123".into())];

    run(&server, &files, &o, None, 1).await;

    assert_eq!(put_hints(&server, "a.bin").await, ["123"]);
}

/// The same on the initiate and completion requests of a multipart upload.
#[tokio::test]
async fn a_size_hint_header_replaces_the_computed_hint_on_multipart() {
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
        .and(path("/test-item/a.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>up-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/a.bin"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploadId", "up-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 10)];
    let mut o = opts();
    o.multipart = true;
    o.headers = vec![("x-archive-size-hint".into(), "123".into())];

    run(&server, &files, &o, None, 1).await;

    let initiate = request_hints(&server, "POST", "a.bin", |q| q.starts_with("uploads")).await;
    let complete = request_hints(&server, "POST", "a.bin", |q| q.contains("uploadId")).await;
    assert_eq!(initiate, ["123"]);
    assert_eq!(complete, ["123"]);
}
