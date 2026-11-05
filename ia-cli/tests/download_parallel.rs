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

/// The cache directory the tests' `ia` processes keep their slot files in,
/// so a test run never writes to the user's own cache.
fn test_cache() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("ia-cache")
}

fn ia_cmd() -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .env("IA_CACHE_DIR", test_cache());
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
    let ranges = download_ranges(&server, &body, &["--connections-per-file", "4"]).await;
    // Each of the four 64 MiB parts is asked for whole. Under load a stream
    // may also re-request the rest of its part after a dropped body; such a
    // request starts inside the part and ends where the part ends.
    let part = 64 * MIB;
    let spans: Vec<(usize, usize)> = ranges
        .iter()
        .map(|r| {
            let r = r.as_deref().expect("every request carried Range");
            let (a, b) = r.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();
    for k in 0..4 {
        let whole = (k * part, (k + 1) * part - 1);
        assert!(
            spans.contains(&whole),
            "part {k} never asked for whole: {ranges:?}"
        );
    }
    assert!(
        spans
            .iter()
            .all(|&(a, b)| (b + 1) % part == 0 && a <= b && a >= b + 1 - part),
        "a request outside one part: {ranges:?}"
    );
}

// ─── The per-server cap, counted on a loopback server ──────────────────────

/// Open download connections, now and at most, and every download request
/// seen.
#[derive(Default)]
struct Gauge {
    open: AtomicUsize,
    most: AtomicUsize,
    seen: AtomicUsize,
}

const FILES: usize = 8;
const FILE_BODY: &[u8] = b"sixteen bytes!!!";

/// How long the server holds each download open. Long enough that every
/// request a run sends at once arrives inside it, even on a loaded runner.
const HOLD: Duration = Duration::from_millis(1000);

/// The metadata of an item of [`FILES`] files named `identifier`.
fn small_item(identifier: &str) -> String {
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
    json!({
        "metadata": {"identifier": identifier, "mediatype": "data"},
        "files": files
    })
    .to_string()
}

/// A loopback server, one request per connection: `/metadata/<identifier>`
/// answers at once with an item of [`FILES`] files; every other request is a
/// download, held open for [`HOLD`], counted in the gauge, then served.
async fn holding_server() -> (String, Arc<Gauge>) {
    holding_server_with(|_| Some(HOLD)).await
}

/// [`holding_server`], holding each download for `hold(path)`, or until the
/// client goes away when that is `None`.
async fn holding_server_with(hold: fn(&str) -> Option<Duration>) -> (String, Arc<Gauge>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let gauge = Arc::new(Gauge::default());
    let counted = Arc::clone(&gauge);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let gauge = Arc::clone(&counted);
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
                let body: Vec<u8> = match head.strip_prefix("GET /metadata/") {
                    Some(rest) => {
                        let identifier = rest.split(['?', ' ']).next().unwrap_or("");
                        small_item(identifier).into_bytes()
                    }
                    None => {
                        let path = head.split_whitespace().nth(1).unwrap_or("");
                        gauge.seen.fetch_add(1, Ordering::SeqCst);
                        let now = gauge.open.fetch_add(1, Ordering::SeqCst) + 1;
                        gauge.most.fetch_max(now, Ordering::SeqCst);
                        match hold(path) {
                            Some(hold) => tokio::time::sleep(hold).await,
                            None => {
                                // Until the client hangs up.
                                while !matches!(stream.read(&mut buf).await, Ok(0) | Err(_)) {}
                            }
                        }
                        gauge.open.fetch_sub(1, Ordering::SeqCst);
                        FILE_BODY.to_vec()
                    }
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.flush().await;
            });
        }
    });
    (host, gauge)
}

/// Run `ia download` with `source` (an identifier, or `--itemlist <file>`)
/// and `extra` against a holding server, into `disks` destination
/// directories (more than one makes a disk pool), check every file of
/// `items` arrived on one of them, and return the most download connections
/// the server saw at once.
async fn download_from_holding_server(
    source: &[&str],
    items: &[&str],
    disks: usize,
    extra: &[&str],
) -> usize {
    let (host, gauge) = holding_server().await;
    let dirs: Vec<TempDir> = (0..disks).map(|_| TempDir::new().unwrap()).collect();
    let mut args = vec!["--insecure", "-H", &host, "download"];
    args.extend_from_slice(source);
    for dir in &dirs {
        args.extend_from_slice(&["--destdir", dir.path().to_str().unwrap()]);
    }
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
    for item in items {
        for i in 0..FILES {
            let got = dirs
                .iter()
                .find_map(|dir| fs::read(dir.path().join(item).join(format!("f{i}.bin"))).ok())
                .unwrap_or_else(|| panic!("{item}/f{i}.bin is on no disk"));
            assert_eq!(got, FILE_BODY);
        }
    }
    gauge.most.load(Ordering::SeqCst)
}

async fn download_small_item(extra: &[&str]) -> usize {
    download_from_holding_server(&["small-item"], &["small-item"], 1, extra).await
}

async fn download_two_items(disks: usize) -> usize {
    let list = tempfile::NamedTempFile::new().unwrap();
    fs::write(list.path(), "item-a\nitem-b\n").unwrap();
    download_from_holding_server(
        &["--itemlist", list.path().to_str().unwrap()],
        &["item-a", "item-b"],
        disks,
        &["-j", "8"],
    )
    .await
}

#[tokio::test]
async fn a_batch_of_two_items_on_one_server_shares_one_cap() {
    assert_eq!(download_two_items(1).await, 6);
}

#[tokio::test]
async fn a_batch_across_a_disk_pool_shares_one_cap() {
    // With several --destdir, each item gets its own options for its disk;
    // they must still share one table of server slots.
    assert_eq!(download_two_items(2).await, 6);
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

// ─── The per-server cap across processes ────────────────────────────────────

/// An `ia download` of `item` from `host` into a new directory, with its slot
/// files under `cache`, started and left running.
fn spawn_ia(
    host: &str,
    item: &str,
    cache: &std::path::Path,
    extra: &[&str],
) -> (std::process::Child, TempDir) {
    let dir = TempDir::new().unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_ia-cli"))
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .env("IA_CACHE_DIR", cache)
        .args(["--insecure", "-H", host, "download", item, "--destdir"])
        .arg(dir.path())
        .args(extra)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    (child, dir)
}

/// Wait for `child` to finish, at most `limit`, and require success.
async fn finish(child: std::process::Child, limit: Duration) {
    let out = tokio::time::timeout(
        limit,
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .expect("ia download did not finish in time")
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_item(dir: &TempDir, item: &str) {
    for i in 0..FILES {
        let got = fs::read(dir.path().join(item).join(format!("f{i}.bin"))).unwrap();
        assert_eq!(got, FILE_BODY, "{item}/f{i}.bin");
    }
}

/// Two `ia download` processes, each of an 8-file item with `-j 8`, at once
/// against one server with one cache; the most connections it saw at once.
async fn two_processes(extra: &[&str], hold: fn(&str) -> Option<Duration>) -> usize {
    let (host, gauge) = holding_server_with(hold).await;
    let cache = TempDir::new().unwrap();
    let mut args = vec!["-j", "8"];
    args.extend_from_slice(extra);
    let (a, dir_a) = spawn_ia(&host, "item-a", cache.path(), &args);
    let (b, dir_b) = spawn_ia(&host, "item-b", cache.path(), &args);
    let limit = Duration::from_secs(60);
    tokio::join!(finish(a, limit), finish(b, limit));
    assert_item(&dir_a, "item-a");
    assert_item(&dir_b, "item-b");
    gauge.most.load(Ordering::SeqCst)
}

#[tokio::test]
async fn two_processes_with_the_default_cap_never_open_more_than_six_together() {
    assert_eq!(two_processes(&[], |_| Some(HOLD)).await, 6);
}

#[tokio::test]
async fn a_cap_of_one_across_two_processes_runs_one_download_at_a_time() {
    let most = two_processes(&["--max-connections-per-server", "1"], |_| {
        Some(Duration::from_millis(100))
    })
    .await;
    assert_eq!(most, 1);
}

#[tokio::test]
async fn a_process_killed_while_holding_the_only_slot_frees_it() {
    // Every download of item-hang is held until its client goes away.
    let (host, gauge) = holding_server_with(|path| {
        (!path.contains("/item-hang/")).then_some(Duration::from_millis(100))
    })
    .await;
    let cache = TempDir::new().unwrap();
    let cap = ["--max-connections-per-server", "1"];

    let (mut first, _first_dir) = spawn_ia(&host, "item-hang", cache.path(), &cap);
    let started = std::time::Instant::now();
    while gauge.seen.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the first process never connected"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (second, second_dir) = spawn_ia(&host, "item-b", cache.path(), &cap);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        gauge.seen.load(Ordering::SeqCst),
        1,
        "the second process connected while the first held the only slot"
    );

    first.kill().unwrap();
    first.wait().unwrap();

    finish(second, Duration::from_secs(30)).await;
    assert_item(&second_dir, "item-b");
    assert_eq!(gauge.most.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_cache_that_cannot_hold_slot_files_falls_back_to_the_process_cap() {
    let (host, gauge) = holding_server().await;
    let not_a_dir = tempfile::NamedTempFile::new().unwrap();
    let dir = TempDir::new().unwrap();

    let out = tokio::task::spawn_blocking({
        let (host, cache, dest) = (
            host.clone(),
            not_a_dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_ia-cli"))
                .env_remove("IA_ACCESS_KEY_ID")
                .env_remove("IA_SECRET_ACCESS_KEY")
                .env("IA_CACHE_DIR", &cache)
                .args([
                    "--insecure",
                    "-H",
                    &host,
                    "download",
                    "small-item",
                    "-j",
                    "8",
                ])
                .arg("--destdir")
                .arg(&dest)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_item(&dir, "small-item");
    // The cap still holds within the process.
    assert_eq!(gauge.most.load(Ordering::SeqCst), 6);
    // One warning, naming the path.
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("slot files"))
        .collect();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(
        warnings[0].contains(&not_a_dir.path().display().to_string()),
        "{stderr}"
    );
}
