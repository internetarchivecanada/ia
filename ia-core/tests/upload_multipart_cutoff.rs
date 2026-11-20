//! Multipart chosen by size (#181): a file larger than the cutoff is a
//! multipart upload from its first attempt, a file at or under it is a
//! single PUT, and a single PUT that dies is retried as a single PUT.
//!
//! The integration tests use a 1 MiB cutoff (`Multipart::Above`) so that no
//! test sends a 100 MiB body; the default cutoff is checked on the rule
//! itself (`Multipart::is_multipart`).

use ia_core::upload::{self, Multipart, UploadOpts, UploadStatus, MULTIPART_CUTOFF};
use ia_core::{IaClient, IaConfig};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

const CUTOFF: u64 = 1024 * 1024;

fn client_at(addr: std::net::SocketAddr) -> IaClient {
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = addr.to_string();
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

fn test_client(server: &MockServer) -> IaClient {
    client_at(support::mock_addr(server))
}

fn write_sized(dir: &TempDir, name: &str, size: u64) -> std::path::PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, vec![0x42u8; size as usize]).unwrap();
    p
}

fn opts(multipart: Multipart) -> UploadOpts {
    let mut o = UploadOpts::default();
    o.verify = false;
    o.checksum = false;
    o.no_collection_check = true;
    o.retries = 3;
    o.retry_min_delay = std::time::Duration::from_millis(1);
    o.retry_max_delay = std::time::Duration::from_millis(2);
    o.multipart = multipart;
    o
}

async fn upload(
    client: &IaClient,
    files: &[std::path::PathBuf],
    o: &UploadOpts,
) -> Vec<upload::UploadResult> {
    upload::upload_item(client, "test-item", files, o, None, None, None, 1, None)
        .await
        .unwrap()
}

fn status_of<'a>(results: &'a [upload::UploadResult], key: &str) -> &'a UploadStatus {
    &results.iter().find(|r| r.key == key).unwrap().status
}

/// The item's listing of unfinished multipart uploads, answered `times`
/// times: one per `(key, upload_id)` given.
async fn mount_listing(server: &MockServer, uploads: &[(&str, &str)], times: u64) {
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
        .expect(times)
        .mount(server)
        .await;
}

/// A fresh one-part multipart upload of `/test-item/<key>`.
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

/// IA holds the whole file as part 1 of `upload_id`: a resume sends no
/// initiate and no part, only the completion.
async fn mount_whole_file_as_part_one(server: &MockServer, key: &str, upload_id: &str, size: u64) {
    use md5::{Digest, Md5};
    let etag: String = Md5::digest(vec![0x42u8; size as usize])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("/test-item/{key}")))
        .and(query_param("uploadId", upload_id))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>\"{etag}\"</ETag>\
             <Size>{size}</Size></Part></ListPartsResult>"
        )))
        .expect(1)
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

/// No multipart request of any kind: no listing, no initiate or
/// completion (POST), no abort (DELETE). Mounted after any mock that is
/// meant to answer, since the first mock that matches answers.
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

// -- The rule --

#[test]
fn auto_is_multipart_only_above_100_mib() {
    assert_eq!(MULTIPART_CUTOFF, 100 * 1024 * 1024);
    assert!(!Multipart::Auto.is_multipart(0));
    assert!(!Multipart::Auto.is_multipart(MULTIPART_CUTOFF));
    assert!(Multipart::Auto.is_multipart(MULTIPART_CUTOFF + 1));
    assert_eq!(Multipart::default(), Multipart::Auto);
}

#[test]
fn above_is_multipart_only_above_its_cutoff() {
    assert!(!Multipart::Above(CUTOFF).is_multipart(CUTOFF));
    assert!(Multipart::Above(CUTOFF).is_multipart(CUTOFF + 1));
    assert!(!Multipart::Above(MULTIPART_CUTOFF).is_multipart(MULTIPART_CUTOFF));
    assert!(Multipart::Above(MULTIPART_CUTOFF).is_multipart(MULTIPART_CUTOFF + 1));
}

#[test]
fn always_and_never_ignore_the_size() {
    for size in [
        0,
        1,
        CUTOFF,
        MULTIPART_CUTOFF,
        MULTIPART_CUTOFF + 1,
        u64::MAX,
    ] {
        assert!(Multipart::Always.is_multipart(size), "{size}");
        assert!(!Multipart::Never.is_multipart(size), "{size}");
    }
}

// -- Files over and at the cutoff --

/// A file one byte over the cutoff is a multipart upload from its first
/// attempt (one listing of its name, then initiate, part, complete; no
/// single PUT). A file exactly at the cutoff is a single PUT.
#[tokio::test]
async fn over_the_cutoff_is_multipart_from_the_first_attempt_and_at_it_a_single_put() {
    let server = MockServer::start().await;
    mount_listing(&server, &[], 1).await;
    mount_one_part_multipart(&server, "over.bin", "fresh-over").await;
    mount_single_put(&server, "over.bin", 0).await;
    mount_single_put(&server, "at.bin", 1).await;
    Mock::given(method("POST"))
        .and(path("/test-item/at.bin"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "over.bin", CUTOFF + 1),
        write_sized(&dir, "at.bin", CUTOFF),
    ];
    let o = opts(Multipart::Above(CUTOFF));
    let results = upload(&test_client(&server), &files, &o).await;

    for key in ["over.bin", "at.bin"] {
        assert!(
            matches!(status_of(&results, key), UploadStatus::Uploaded),
            "{key}: {results:?}"
        );
    }
    server.verify().await;
}

/// `Always` sends every file as multipart, a file under any cutoff too.
#[tokio::test]
async fn always_sends_every_file_as_multipart() {
    let server = MockServer::start().await;
    mount_listing(&server, &[], 2).await;
    mount_one_part_multipart(&server, "a.bin", "on-a").await;
    mount_one_part_multipart(&server, "b.bin", "on-b").await;
    mount_single_put(&server, "a.bin", 0).await;
    mount_single_put(&server, "b.bin", 0).await;

    let dir = TempDir::new().unwrap();
    let files = [
        write_sized(&dir, "a.bin", 10),
        write_sized(&dir, "b.bin", 1024),
    ];
    let results = upload(&test_client(&server), &files, &opts(Multipart::Always)).await;

    for r in &results {
        assert!(matches!(r.status, UploadStatus::Uploaded), "{r:?}");
    }
    server.verify().await;
}

// -- No switch on failure --

/// With the default setting, a file under the cutoff whose single PUT dies
/// (its connection dropped) is sent again as a single PUT; a later file
/// stays a single PUT; no multipart request is sent, and no listing.
/// 3 MiB: above the old 2 MiB fallback threshold.
#[tokio::test]
async fn auto_retries_a_dead_single_put_under_the_cutoff_as_a_single_put() {
    let server = MockServer::start().await;
    mount_single_put(&server, "a.bin", 1).await;
    mount_single_put(&server, "b.bin", 1).await;
    mount_no_multipart_requests(&server).await;
    // The first connection is the single PUT of a.bin.
    let proxy = support::dropping_proxy(support::mock_addr(&server), 0, 1).await;

    let dir = TempDir::new().unwrap();
    let size = 3 * 1024 * 1024;
    let files = [
        write_sized(&dir, "a.bin", size),
        write_sized(&dir, "b.bin", size),
    ];
    let results = upload(&client_at(proxy), &files, &opts(Multipart::Auto)).await;

    assert_eq!(results.len(), 2);
    for r in &results {
        assert!(matches!(r.status, UploadStatus::Uploaded), "{r:?}");
    }
    assert_eq!(results[0].retries, 1);
    assert_eq!(results[1].retries, 0);
    server.verify().await;
}

/// The same with a custom cutoff: a single PUT that dies on every attempt
/// fails with the single-PUT error once its retries are spent.
#[tokio::test]
async fn a_dead_single_put_at_the_cutoff_fails_as_a_single_put() {
    let server = MockServer::start().await;
    mount_single_put(&server, "a.bin", 0).await;
    mount_no_multipart_requests(&server).await;
    // Exactly the single PUT's four attempts (retries 3) are dropped: no
    // listing or multipart request may take one of them.
    let proxy = support::dropping_proxy(support::mock_addr(&server), 0, 4).await;

    let dir = TempDir::new().unwrap();
    // 3 MiB, at its cutoff and above the old 2 MiB fallback threshold.
    let size = 3 * 1024 * 1024;
    let files = [write_sized(&dir, "a.bin", size)];
    let results = upload(&client_at(proxy), &files, &opts(Multipart::Above(size))).await;

    let UploadStatus::Failed(message) = &results[0].status else {
        panic!("{results:?}");
    };
    assert!(message.contains("test-item/a.bin"), "{message}");
    assert!(message.contains("after 4 attempts"), "{message}");
    assert!(!message.contains("multipart"), "{message}");
    server.verify().await;
}

// -- Reruns --

/// A rerun of a file over the cutoff resumes from the parts IA holds: its
/// name is listed once (by the file, not also by the item), the part IA
/// holds checks out, and only the completion is sent.
#[tokio::test]
async fn a_rerun_of_a_file_over_the_cutoff_resumes_from_its_parts() {
    let server = MockServer::start().await;
    let size = CUTOFF + 1;
    mount_listing(&server, &[("a.bin", "kept-a")], 1).await;
    mount_whole_file_as_part_one(&server, "a.bin", "kept-a", size).await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    mount_single_put(&server, "a.bin", 0).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", size)];
    let results = upload(
        &test_client(&server),
        &files,
        &opts(Multipart::Above(CUTOFF)),
    )
    .await;

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    server.verify().await;
}

/// A file at or under the cutoff is a single PUT even where IA holds an
/// unfinished multipart upload of it (for example one left by an older
/// ia-cli's fallback): nothing is listed, resumed or aborted.
#[tokio::test]
async fn a_file_under_the_cutoff_is_a_single_put_whatever_ia_holds() {
    let server = MockServer::start().await;
    // A listing, if one were sent, would name the file; none is expected.
    mount_listing(&server, &[("a.bin", "kept-a")], 0).await;
    mount_single_put(&server, "a.bin", 1).await;
    mount_no_multipart_requests(&server).await;

    let dir = TempDir::new().unwrap();
    let files = [write_sized(&dir, "a.bin", 1024)];
    let results = upload(&test_client(&server), &files, &opts(Multipart::Auto)).await;

    assert!(
        matches!(results[0].status, UploadStatus::Uploaded),
        "{results:?}"
    );
    server.verify().await;
}
