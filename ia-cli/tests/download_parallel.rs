//! `ia download --connections-per-file` (#16): parallel Range requests
//! within one file, and `--max-connections-per-server`, the cap on download
//! connections to one server.
//!
//! The part size and threshold are fixed in `ia-core` (64 MiB parts, from
//! 256 MiB), so the end-to-end test serves a 256 MiB body: the smallest
//! file that is split, into four parts.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
fn download_help_describes_both_flags_and_their_defaults() {
    let out = ia_cmd().args(["download", "--help"]).output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "--connections-per-file <N>",
        "Connections used for one file of 256 MiB or more, 1 to 8 (default 1). Counts against \
         --max-connections-per-server.",
        "--max-connections-per-server <N>",
        "Open connections to one server at once, counting every job and every connection per \
         file (default 6).",
        "Files downloaded at once, at most N (default 8). Connections to any one server are also \
         capped by --max-connections-per-server, so jobs on the same server may wait for a slot.",
    ] {
        assert!(
            help.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(expected),
            "missing from download --help: {expected}\n{help}"
        );
    }
    assert!(!help.contains("--streams"), "{help}");
}

#[test]
fn connections_per_file_outside_one_to_eight_is_a_usage_error() {
    for bad in ["0", "9"] {
        ia_cmd()
            .args(["download", "nasa", "--connections-per-file", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("1..=8"));
    }
}

#[test]
fn max_connections_per_server_zero_is_a_usage_error() {
    ia_cmd()
        .args(["download", "nasa", "--max-connections-per-server", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--max-connections-per-server"));
}

#[tokio::test]
async fn a_large_file_is_one_get_by_default_and_four_ranges_with_connections_per_file_4() {
    let body = Arc::new(big_body());
    let server = MockServer::start().await;

    mount_big_item(&server, &body).await;
    let ranges = download_ranges(&server, &body, &[]).await;
    assert_eq!(ranges, vec![None]);

    mount_big_item(&server, &body).await;
    let mut ranges = download_ranges(&server, &body, &["--connections-per-file", "4"]).await;
    ranges.sort();
    let part = 64 * MIB;
    let mut expected: Vec<Option<String>> = (0..4)
        .map(|k| Some(format!("bytes={}-{}", k * part, (k + 1) * part - 1)))
        .collect();
    expected.sort();
    assert_eq!(ranges, expected);
}

// ─── The per-server cap, counted on a loopback server ──────────────────────

/// Open download connections, now and at most.
#[derive(Default)]
struct Gauge {
    open: AtomicUsize,
    most: AtomicUsize,
}

const FILES: usize = 8;
const FILE_BODY: &[u8] = b"sixteen bytes!!!";

/// A loopback server, one request per connection: `/metadata/small-item`
/// answers at once with an item of [`FILES`] files; every other request is a
/// download, held open for a moment, counted in the gauge, then served.
async fn holding_server() -> (String, Arc<Gauge>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let gauge = Arc::new(Gauge::default());
    let files: Vec<serde_json::Value> = (0..FILES)
        .map(|i| {
            json!({
                "name": format!("f{i}.bin"),
                "source": "original",
                "size": FILE_BODY.len().to_string(),
                "mtime": "1700000000"
            })
        })
        .collect();
    let metadata = json!({
        "metadata": {"identifier": "small-item", "mediatype": "data"},
        "files": files
    })
    .to_string();
    let counted = Arc::clone(&gauge);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (gauge, metadata) = (Arc::clone(&counted), metadata.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head);
                let body: &[u8] = if head.starts_with("GET /metadata/") {
                    metadata.as_bytes()
                } else {
                    let now = gauge.open.fetch_add(1, Ordering::SeqCst) + 1;
                    gauge.most.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    gauge.open.fetch_sub(1, Ordering::SeqCst);
                    FILE_BODY
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.flush().await;
            });
        }
    });
    (host, gauge)
}

async fn download_small_item(extra: &[&str]) -> usize {
    let (host, gauge) = holding_server().await;
    let dir = TempDir::new().unwrap();
    let mut args = vec![
        "--insecure",
        "-H",
        &host,
        "download",
        "small-item",
        "--destdir",
        dir.path().to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let out = tokio::task::spawn_blocking({
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        move || ia_cmd().args(&args).output().unwrap()
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for i in 0..FILES {
        let got = fs::read(dir.path().join("small-item").join(format!("f{i}.bin"))).unwrap();
        assert_eq!(got, FILE_BODY);
    }
    gauge.most.load(Ordering::SeqCst)
}

#[tokio::test]
async fn eight_jobs_on_one_server_never_open_more_than_six_connections() {
    assert_eq!(download_small_item(&["-j", "8"]).await, 6);
}

#[tokio::test]
async fn max_connections_per_server_1_serialises_every_job() {
    assert_eq!(
        download_small_item(&["-j", "8", "--max-connections-per-server", "1"]).await,
        1
    );
}

#[tokio::test]
async fn max_connections_per_server_above_jobs_lets_every_job_run() {
    assert_eq!(
        download_small_item(&["-j", "8", "--max-connections-per-server", "16"]).await,
        8
    );
}
