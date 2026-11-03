//! `ia download --streams` (#16): parallel Range requests within one file.
//!
//! The part size and threshold are fixed in `ia-core` (64 MiB parts, from
//! 256 MiB), so the end-to-end test serves a 256 MiB body: the smallest
//! file that is split, into four parts.

use std::fs;
use std::sync::Arc;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MIB: usize = 1024 * 1024;
const SIZE: usize = 256 * MIB;

fn ia_cmd() -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY");
    cmd
}

fn range_of(request: &Request) -> Option<String> {
    request
        .headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Serves `body`: a 206 for a `bytes=A-B` or `bytes=A-` range, a 200
/// without one.
struct RangeServer(Arc<Vec<u8>>);

impl Respond for RangeServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let len = self.0.len();
        let range = range_of(request).and_then(|r| {
            let (a, b) = r.strip_prefix("bytes=")?.split_once('-')?;
            let a: usize = a.parse().ok()?;
            let b: usize = if b.is_empty() {
                len - 1
            } else {
                b.parse().ok()?
            };
            Some((a, b.min(len - 1)))
        });
        match range {
            Some((a, b)) => ResponseTemplate::new(206)
                .insert_header("content-range", format!("bytes {a}-{b}/{len}"))
                .set_body_bytes(self.0[a..=b].to_vec()),
            None => ResponseTemplate::new(200).set_body_bytes(self.0.to_vec()),
        }
    }
}

/// A 256 MiB body that differs from one MiB to the next.
fn big_body() -> Vec<u8> {
    (0..SIZE / MIB)
        .flat_map(|i| vec![(i % 251) as u8; MIB])
        .collect()
}

async fn mount_big_item(server: &MockServer, body: &Arc<Vec<u8>>) {
    // No md5 in the metadata: hashing 256 MiB in a debug build would
    // dominate the test, and verification is covered in ia-core.
    Mock::given(method("GET"))
        .and(path("/metadata/big-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"identifier": "big-item", "mediatype": "data"},
            "files": [{
                "name": "big.bin",
                "source": "original",
                "size": SIZE.to_string(),
                "mtime": "1700000000"
            }]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/big-item/big.bin"))
        .respond_with(RangeServer(Arc::clone(body)))
        .mount(server)
        .await;
}

async fn download_ranges(server: &MockServer, body: &[u8], extra: &[&str]) -> Vec<Option<String>> {
    let host = server.uri().replace("http://", "");
    let dir = TempDir::new().unwrap();
    let mut args = vec![
        "--insecure",
        "-H",
        &host,
        "download",
        "big-item",
        "--destdir",
        dir.path().to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    ia_cmd().args(&args).assert().success();

    let out = dir.path().join("big-item").join("big.bin");
    assert!(
        fs::read(&out).unwrap() == body,
        "the file differs from the body"
    );
    assert!(!dir.path().join("big-item/big.bin.part").exists());
    assert!(!dir.path().join("big-item/big.bin.part.json").exists());

    let ranges = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/download/big-item/big.bin")
        .map(range_of)
        .collect();
    server.reset().await;
    ranges
}

#[test]
fn download_help_describes_streams() {
    ia_cmd()
        .args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--streams <N>"))
        .stdout(predicate::str::contains("256 MiB"))
        .stdout(predicate::str::contains("[default: 4]"));
}

#[test]
fn streams_outside_one_to_eight_is_a_usage_error() {
    for bad in ["0", "9"] {
        ia_cmd()
            .args(["download", "nasa", "--streams", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("1..=8"));
    }
}

#[tokio::test]
async fn a_large_file_is_fetched_in_four_ranges_by_default_and_one_get_with_streams_1() {
    let body = Arc::new(big_body());
    let server = MockServer::start().await;

    mount_big_item(&server, &body).await;
    let mut ranges = download_ranges(&server, &body, &[]).await;
    ranges.sort();
    let part = 64 * MIB;
    let mut expected: Vec<Option<String>> = (0..4)
        .map(|k| Some(format!("bytes={}-{}", k * part, (k + 1) * part - 1)))
        .collect();
    expected.sort();
    assert_eq!(ranges, expected);

    mount_big_item(&server, &body).await;
    let ranges = download_ranges(&server, &body, &["--streams", "1"]).await;
    assert_eq!(ranges, vec![None]);
}
