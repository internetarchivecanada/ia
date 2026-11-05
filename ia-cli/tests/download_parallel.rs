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
        "Range connections (--connections-per-file) open to one server at once, across every ia \
         process on this machine (default 6).",
        "IA_CACHE_DIR",
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

// ─── Files on one stream are not held back by the cap ────────────────────────

/// Open download connections, now and at most, every download request seen,
/// and every metadata request answered.
#[derive(Default)]
struct Gauge {
    open: AtomicUsize,
    most: AtomicUsize,
    seen: AtomicUsize,
    metadata: AtomicUsize,
}

impl Gauge {
    fn enter(&self) {
        self.seen.fetch_add(1, Ordering::SeqCst);
        let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }

    fn most(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }
}

const FILES: usize = 8;
const FILE_BODY: &[u8] = b"sixteen bytes!!!";

/// How long the server holds each download open. Long enough that every
/// request a run sends at once arrives inside it, even on a loaded runner.
const HOLD: Duration = Duration::from_millis(1000);

/// The metadata of an item named `identifier`: [`FILES`] small files, or one
/// file `big.bin` of [`SIZE`] bytes when `big` is set.
fn item_json(identifier: &str, big: bool) -> String {
    let files: Vec<serde_json::Value> = if big {
        vec![json!({
            "name": "big.bin",
            "source": "original",
            "size": SIZE.to_string(),
            "mtime": "1700000000"
        })]
    } else {
        (0..FILES)
            .map(|i| {
                json!({
                    "name": format!("f{i}.bin"),
                    "source": "original",
                    "size": FILE_BODY.len().to_string(),
                    "mtime": "1700000000"
                })
            })
            .collect()
    };
    json!({
        "metadata": {"identifier": identifier, "mediatype": "data"},
        "files": files
    })
    .to_string()
}

/// A loopback server, one request per connection. `/metadata/<identifier>`
/// answers at once: an item of one [`SIZE`]-byte file when `body` is the
/// large body, else of [`FILES`] small files. A download is counted in the
/// gauge and held for `hold(path)`, or until the client goes away when that
/// is `None`; then it is served, as a 206 for a `Range` request.
async fn holding_server(
    body: Option<Arc<Vec<u8>>>,
    hold: fn(&str) -> Option<Duration>,
) -> (String, Arc<Gauge>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let gauge = Arc::new(Gauge::default());
    let counted = Arc::clone(&gauge);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (gauge, body) = (Arc::clone(&counted), body.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                if let Some(rest) = path.strip_prefix("/metadata/") {
                    let identifier = rest.split('?').next().unwrap_or("");
                    let json = item_json(identifier, body.is_some());
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                    gauge.metadata.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                gauge.enter();
                match hold(&path) {
                    Some(hold) => tokio::time::sleep(hold).await,
                    // Until the client hangs up.
                    None => while !matches!(stream.read(&mut buf).await, Ok(0) | Err(_)) {},
                }
                // Counted down before the answer, so the client's next
                // request is never counted alongside this one.
                gauge.leave();
                let range = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if !name.trim().eq_ignore_ascii_case("range") {
                        return None;
                    }
                    let (a, b) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
                    Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                });
                let (status, extra, bytes): (&str, String, &[u8]) = match (&body, range) {
                    (Some(body), Some((a, b))) => (
                        "206 Partial Content",
                        format!("content-range: bytes {a}-{b}/{}\r\n", body.len()),
                        &body[a..=b],
                    ),
                    (Some(body), None) => ("200 OK", String::new(), &body[..]),
                    (None, _) => ("200 OK", String::new(), FILE_BODY),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(bytes).await;
                let _ = stream.flush().await;
            });
        }
    });
    (host, gauge)
}

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

/// Wait for `child` to finish, at most `limit`, require success, and return
/// its stderr.
async fn finish(child: std::process::Child, limit: Duration) -> String {
    let out = tokio::time::timeout(
        limit,
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .expect("ia download did not finish in time")
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    stderr
}

const LIMIT: Duration = Duration::from_secs(120);

async fn download_small_item(extra: &[&str]) -> usize {
    let (host, gauge) = holding_server(None, |_| Some(HOLD)).await;
    let cache = TempDir::new().unwrap();
    let (child, dir) = spawn_ia(&host, "small-item", cache.path(), extra);
    finish(child, LIMIT).await;
    for i in 0..FILES {
        let got = fs::read(dir.path().join("small-item").join(format!("f{i}.bin"))).unwrap();
        assert_eq!(got, FILE_BODY);
    }
    gauge.most()
}

#[tokio::test]
async fn eight_jobs_on_one_stream_are_not_held_back_by_the_cap() {
    assert_eq!(download_small_item(&["-j", "8"]).await, 8);
}

#[tokio::test]
async fn a_cap_of_one_does_not_hold_back_jobs_on_one_stream() {
    assert_eq!(
        download_small_item(&["-j", "8", "--max-connections-per-server", "1"]).await,
        8
    );
}

// ─── Range connections, across processes ────────────────────────────────────

fn assert_big(dir: &TempDir, item: &str, body: &[u8]) {
    let got = fs::read(dir.path().join(item).join("big.bin")).unwrap();
    assert!(got == body, "{item}/big.bin differs from the body");
    assert!(!dir.path().join(item).join("big.bin.part.json").exists());
}

/// Two `ia download` processes at once, each of one 256 MiB file over 4
/// connections, against one server with one cache; the most range requests
/// the server held at once, and how many it saw.
///
/// The server counts a request only while it holds it, before answering,
/// so its count never exceeds the slots held: a count above the cap means
/// the cap failed, whatever the timing. A count below it proves nothing,
/// since a stream keeps its slot while it reads its 64 MiB body, which the
/// server does not count; how far requests overlap depends on how fast the
/// runner moves those bytes.
async fn two_processes(cap: &str, hold: fn(&str) -> Option<Duration>) -> (usize, usize) {
    let body = Arc::new(big_body());
    let (host, gauge) = holding_server(Some(Arc::clone(&body)), hold).await;
    let cache = TempDir::new().unwrap();
    let args = [
        "--connections-per-file",
        "4",
        "--max-connections-per-server",
        cap,
    ];
    let (a, dir_a) = spawn_ia(&host, "big-a", cache.path(), &args);
    let (b, dir_b) = spawn_ia(&host, "big-b", cache.path(), &args);
    tokio::join!(finish(a, LIMIT), finish(b, LIMIT));
    assert_big(&dir_a, "big-a", &body);
    assert_big(&dir_b, "big-b", &body);
    (gauge.most(), gauge.seen.load(Ordering::SeqCst))
}

#[tokio::test]
async fn two_processes_never_open_more_range_connections_together_than_the_cap() {
    // Each process wants 4 connections, so alone each stays at the cap of
    // 3; without the slot files the two hold up to 6 together.
    let (most, seen) = two_processes("3", |_| Some(HOLD)).await;
    assert!(most <= 3, "{most} range requests open at once");
    // Both processes fetched over ranges: four requests each, at least.
    assert!(seen >= 8, "{seen} range requests");
}

#[tokio::test]
async fn a_cap_of_one_across_two_processes_runs_one_range_connection_at_a_time() {
    let (most, seen) = two_processes("1", |_| Some(Duration::from_millis(100))).await;
    assert_eq!(most, 1);
    assert!(seen >= 8, "{seen} range requests");
}

#[tokio::test]
async fn a_process_killed_while_holding_the_only_slot_frees_it() {
    let body = Arc::new(big_body());
    // Every download of big-hang is held until its client goes away.
    let (host, gauge) = holding_server(Some(Arc::clone(&body)), |path| {
        (!path.contains("/big-hang/")).then_some(Duration::from_millis(100))
    })
    .await;
    let cache = TempDir::new().unwrap();
    let args = [
        "--connections-per-file",
        "4",
        "--max-connections-per-server",
        "1",
    ];

    let (mut first, _first_dir) = spawn_ia(&host, "big-hang", cache.path(), &args);
    let started = std::time::Instant::now();
    while gauge.seen.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the first process never connected"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (second, second_dir) = spawn_ia(&host, "big-b", cache.path(), &args);
    // Once the second process has its metadata it is about to ask for a
    // slot; give it two polls of the slot files (500 ms each, at most)
    // and more, so a check that passed only because it had not started
    // yet cannot happen.
    while gauge.metadata.load(Ordering::SeqCst) < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the second process never asked for its metadata"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        gauge.seen.load(Ordering::SeqCst),
        1,
        "the second process connected while the first held the only slot"
    );

    first.kill().unwrap();
    first.wait().unwrap();

    finish(second, LIMIT).await;
    assert_big(&second_dir, "big-b", &body);
    assert_eq!(gauge.most(), 1);
}

#[tokio::test]
async fn a_cache_that_cannot_hold_slot_files_falls_back_to_the_process_cap() {
    let body = Arc::new(big_body());
    let (host, gauge) = holding_server(Some(Arc::clone(&body)), |_| {
        Some(Duration::from_millis(300))
    })
    .await;
    let not_a_dir = tempfile::NamedTempFile::new().unwrap();

    let (child, dir) = spawn_ia(
        &host,
        "big-a",
        not_a_dir.path(),
        &[
            "--connections-per-file",
            "4",
            "--max-connections-per-server",
            "2",
        ],
    );
    let stderr = finish(child, LIMIT).await;

    assert_big(&dir, "big-a", &body);
    // The cap still holds within the process: never more than 2 requests
    // held at once (without it, the three streams after the first request
    // would be held together), and the file did go over ranges. As in
    // `two_processes`, a count below the cap depends on the runner's speed.
    let most = gauge.most();
    assert!(most <= 2, "{most} range requests open at once");
    let seen = gauge.seen.load(Ordering::SeqCst);
    assert!(seen >= 4, "{seen} range requests");
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
