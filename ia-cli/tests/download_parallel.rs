//! `ia download` and its connection contract (#16): at most 6 connections
//! to one datanode across every `ia` process, large files split
//! automatically into the connections a datanode has to spare, and the
//! back-off on 429 and 5xx.
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
fn download_help_states_the_jobs_rule_and_shows_no_connection_flags() {
    let out = ia_cmd().args(["download", "--help"]).output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "Files downloaded at once across all servers (default 8). Any one server gets at \
             most 6 connections, so files on the same server may wait."
        ),
        "{help}"
    );
    for hidden in [
        "--connections-per-file",
        "--max-connections-per-server",
        "--streams",
    ] {
        assert!(
            !help.contains(hidden),
            "{hidden} in download --help\n{help}"
        );
    }
}

#[test]
fn the_hidden_connections_per_file_is_checked_and_the_cap_is_no_flag() {
    for bad in ["0", "9"] {
        ia_cmd()
            .args(["download", "nasa", "--connections-per-file", bad])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("1..=8"));
    }
    ia_cmd()
        .args(["download", "nasa", "--max-connections-per-server", "2"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--max-connections-per-server"));
}

#[tokio::test]
async fn a_lone_large_file_splits_into_its_parts_and_the_hidden_pin_of_1_keeps_one_get() {
    let body = Arc::new(big_body());
    let server = MockServer::start().await;

    mount_big_item(&server, &body).await;
    let ranges = download_ranges(&server, &body, &[]).await;
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

    mount_big_item(&server, &body).await;
    let ranges = download_ranges(&server, &body, &["--connections-per-file", "1"]).await;
    assert_eq!(ranges, vec![None]);
}

// ─── The cap, counted on a loopback server ───────────────────────────────────

/// Open download connections, now and at most, every download request seen
/// and whether any carried `Range`, and every metadata request answered.
#[derive(Default)]
struct Gauge {
    open: AtomicUsize,
    most: AtomicUsize,
    seen: AtomicUsize,
    ranged: AtomicUsize,
    metadata: AtomicUsize,
}

impl Gauge {
    fn enter(&self) -> usize {
        let nth = self.seen.fetch_add(1, Ordering::SeqCst);
        let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        nth
    }

    fn leave(&self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }

    fn most(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }

    fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }
}

/// Files in each item of the loopback server.
const FILES: usize = 50;
const FILE_BODY: &[u8] = b"sixteen bytes!!!";

/// What the loopback server does with a download request.
#[derive(Clone, Copy)]
enum Reply {
    /// Hold it this long, then serve it.
    Hold(Duration),
    /// Hold it until the client goes away.
    Hang,
    /// Answer at once with this status and `Retry-After: 0`.
    Status(u16),
}

/// The metadata of an item of [`FILES`] small files named `identifier`.
fn item_json(identifier: &str) -> String {
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

/// A loopback server, one request per connection. `/metadata/<identifier>`
/// answers at once with an item of [`FILES`] small files. A download is
/// counted in the gauge and handled as `reply(path, n)` says, `n` counting
/// the downloads before it. The count goes down before the answer, so it
/// never exceeds the connections the client holds.
async fn holding_server(reply: fn(&str, usize) -> Reply) -> (String, Arc<Gauge>) {
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
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                if let Some(rest) = path.strip_prefix("/metadata/") {
                    let json = item_json(rest.split('?').next().unwrap_or(""));
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = stream.write_all(answer.as_bytes()).await;
                    gauge.metadata.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                if head.to_ascii_lowercase().contains("\r\nrange:") {
                    gauge.ranged.fetch_add(1, Ordering::SeqCst);
                }
                let nth = gauge.enter();
                let answer = match reply(&path, nth) {
                    Reply::Hold(hold) => {
                        tokio::time::sleep(hold).await;
                        None
                    }
                    Reply::Hang => {
                        while !matches!(stream.read(&mut buf).await, Ok(0) | Err(_)) {}
                        None
                    }
                    Reply::Status(status) => Some(status),
                };
                gauge.leave();
                let reply = match answer {
                    Some(status) => format!(
                        "HTTP/1.1 {status} Busy\r\nretry-after: 0\r\ncontent-length: 0\r\n\
                         connection: close\r\n\r\n"
                    ),
                    None => format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        FILE_BODY.len()
                    ),
                };
                let _ = stream.write_all(reply.as_bytes()).await;
                if answer.is_none() {
                    let _ = stream.write_all(FILE_BODY).await;
                }
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
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    (child, dir)
}

/// Wait for `child` to finish, at most `limit`, require success, and return
/// its stdout and stderr.
async fn finish(child: std::process::Child, limit: Duration) -> (String, String) {
    let out = tokio::time::timeout(
        limit,
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .expect("ia download did not finish in time")
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    (String::from_utf8_lossy(&out.stdout).to_string(), stderr)
}

const LIMIT: Duration = Duration::from_secs(120);

fn assert_item(dir: &TempDir, item: &str) {
    for i in 0..FILES {
        let got = fs::read(dir.path().join(item).join(format!("f{i}.bin"))).unwrap();
        assert_eq!(got, FILE_BODY, "{item}/f{i}.bin");
    }
}

fn held(_: &str, _: usize) -> Reply {
    Reply::Hold(Duration::from_millis(200))
}

#[tokio::test]
async fn fifty_files_on_one_server_never_exceed_six_and_none_is_split() {
    let (host, gauge) = holding_server(held).await;
    let cache = TempDir::new().unwrap();
    let (child, dir) = spawn_ia(&host, "small-item", cache.path(), &["-j", "8"]);
    finish(child, LIMIT).await;
    assert_item(&dir, "small-item");
    // The count never exceeds the slots held, so this bound is exact; a
    // count below 6 would only mean the requests did not overlap.
    assert!(gauge.most() <= 6, "{} open at once", gauge.most());
    assert!(gauge.most() >= 3, "{} open at once", gauge.most());
    assert_eq!(gauge.ranged.load(Ordering::SeqCst), 0, "a range request");
    assert_eq!(gauge.seen(), FILES);
}

// ─── The cap across processes ───────────────────────────────────────────────

#[tokio::test]
async fn two_processes_on_one_server_never_exceed_six_together() {
    let (host, gauge) = holding_server(held).await;
    let cache = TempDir::new().unwrap();
    let (a, dir_a) = spawn_ia(&host, "item-a", cache.path(), &["-j", "8"]);
    let (b, dir_b) = spawn_ia(&host, "item-b", cache.path(), &["-j", "8"]);
    tokio::join!(finish(a, LIMIT), finish(b, LIMIT));
    assert_item(&dir_a, "item-a");
    assert_item(&dir_b, "item-b");
    // Without the slot files the two would hold up to 12.
    assert!(gauge.most() <= 6, "{} open at once", gauge.most());
    assert_eq!(gauge.seen(), 2 * FILES);
}

#[tokio::test]
async fn a_process_killed_while_holding_all_six_slots_frees_them() {
    // Every download of item-hang is held until its client goes away.
    let (host, gauge) = holding_server(|path, _| {
        if path.contains("/item-hang/") {
            Reply::Hang
        } else {
            Reply::Hold(Duration::from_millis(50))
        }
    })
    .await;
    let cache = TempDir::new().unwrap();

    let (mut first, _first_dir) = spawn_ia(&host, "item-hang", cache.path(), &["-j", "8"]);
    let started = std::time::Instant::now();
    while gauge.seen() < 6 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the first process never held six"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (second, second_dir) = spawn_ia(&host, "item-b", cache.path(), &["-j", "8"]);
    // Once the second process has its metadata it is about to ask for a
    // slot; give it two polls of the slot files (500 ms each, at most) and
    // more, so a check that passed only because it had not started yet
    // cannot happen.
    while gauge.metadata.load(Ordering::SeqCst) < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the second process never asked for its metadata"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        gauge.seen(),
        6,
        "the second process connected while the first held all six"
    );

    first.kill().unwrap();
    first.wait().unwrap();

    finish(second, LIMIT).await;
    assert_item(&second_dir, "item-b");
    assert!(gauge.most() <= 6, "{} open at once", gauge.most());
}

#[tokio::test]
async fn a_cache_that_cannot_hold_slot_files_falls_back_to_the_process_cap() {
    let (host, gauge) = holding_server(held).await;
    let not_a_dir = tempfile::NamedTempFile::new().unwrap();

    let (child, dir) = spawn_ia(&host, "small-item", not_a_dir.path(), &["-j", "8"]);
    let (_, stderr) = finish(child, LIMIT).await;

    assert_item(&dir, "small-item");
    // The cap still holds within the process.
    assert!(gauge.most() <= 6, "{} open at once", gauge.most());
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

// ─── The back-off notice ─────────────────────────────────────────────────────

/// The first download request is answered 429; the rest are held a moment.
fn first_is_429(_: &str, nth: usize) -> Reply {
    if nth == 0 {
        Reply::Status(429)
    } else {
        Reply::Hold(Duration::from_millis(100))
    }
}

#[tokio::test]
async fn a_429_prints_one_slow_down_line() {
    let (host, _gauge) = holding_server(first_is_429).await;
    let cache = TempDir::new().unwrap();
    let (child, dir) = spawn_ia(&host, "small-item", cache.path(), &["-j", "8"]);
    let (_, stderr) = finish(child, LIMIT).await;
    assert_item(&dir, "small-item");
    let server = host.clone();
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("asked us to slow down"))
        .collect();
    assert_eq!(
        lines,
        vec![format!("server {server} asked us to slow down; using 3 connections").as_str()],
        "{stderr}"
    );
}

#[tokio::test]
async fn a_429_with_json_gives_one_slow_down_event() {
    let (host, _gauge) = holding_server(first_is_429).await;
    let cache = TempDir::new().unwrap();
    let (child, dir) = spawn_ia(&host, "small-item", cache.path(), &["-j", "8", "--json"]);
    let (stdout, stderr) = finish(child, LIMIT).await;
    assert_item(&dir, "small-item");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event"] == "slow_down")
        .collect();
    assert_eq!(
        events,
        vec![json!({
            "event": "slow_down",
            "server": host,
            "status": 429,
            "connections_before": 6,
            "connections_after": 3,
        })],
        "{stdout}"
    );
    assert!(!stderr.contains("asked us to slow down"), "{stderr}");
}
