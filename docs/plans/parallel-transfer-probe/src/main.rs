//! Measures whether parallelism helps multipart uploads (#22) and single-file
//! downloads (#16) against real archive.org. A measurement tool, not product
//! code: the method is in ../parallel-transfer-measurement-2026-10-30.md.
//!
//! Usage:
//!   parallel-transfer-probe upload <512MiB-file> <results.jsonl>
//!   parallel-transfer-probe download <scratch-dir> <results.jsonl>
//!   parallel-transfer-probe cleanup <results.jsonl>
//!   parallel-transfer-probe single-h1 <bytes> <results.jsonl>

use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use bytes::Bytes;
use futures::StreamExt;
use md5::{Digest, Md5};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;
const S3: &str = "https://s3.us.archive.org";
const ITEM: &str = "ia-cli-parallel-probe-20261030";
const KEY: &str = "probe-512MiB.bin";
const UPLOAD_BUDGET: u64 = 6 * 1024 * MIB;
const DOWNLOAD_BUDGET: u64 = 20 * 1024 * MIB;
const DL_ITEM: &str = "ubuntu-18.04.5-desktop-amd64";
const DL_FILE: &str = "ubuntu-18.04.5-desktop-amd64.iso";
const NODE_CHECK_BYTES: u64 = 64 * MIB;
const PAUSE: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone)]
struct Keys {
    access: String,
    secret: String,
}

impl Keys {
    fn auth(&self) -> String {
        format!("LOW {}:{}", self.access, self.secret)
    }

    /// Same platform comment as ia-core's User-Agent, so archive.org
    /// attributes the traffic to the account; the product name says what
    /// sent it.
    fn user_agent(&self) -> String {
        format!(
            "ia-parallel-transfer-probe/0.1 ({} {}; N; en; {})",
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.access
        )
    }
}

/// The keys, found the way ia-core finds them: the environment pair first,
/// then the first config file in ia-core's search order, `[s3]` section.
fn load_keys() -> Result<Keys> {
    if let (Ok(access), Ok(secret)) = (
        std::env::var("IA_ACCESS_KEY_ID"),
        std::env::var("IA_SECRET_ACCESS_KEY"),
    ) {
        return Ok(Keys { access, secret });
    }
    let home = std::env::var("HOME").map(PathBuf::from).ok();
    let candidates = [
        std::env::var("IA_CONFIG_FILE").ok().map(PathBuf::from),
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(|x| PathBuf::from(x).join("internetarchive/ia.ini")),
        home.as_ref()
            .map(|h| h.join(".config/internetarchive/ia.ini")),
        home.as_ref().map(|h| h.join(".config/ia.ini")),
        home.as_ref().map(|h| h.join(".ia")),
    ];
    let path = candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .context("no ia config file found")?;
    let mut ini = configparser::ini::Ini::new();
    ini.load(&path).map_err(anyhow::Error::msg)?;
    let access = ini.get("s3", "access").context("no [s3] access")?;
    let secret = ini.get("s3", "secret").context("no [s3] secret")?;
    Ok(Keys { access, secret })
}

/// Append-only JSON lines record of everything measured.
struct Log(Mutex<File>);

impl Log {
    fn open(path: &Path) -> Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self(Mutex::new(f)))
    }
    fn write(&self, v: Value) {
        if let Ok(mut f) = self.0.lock() {
            let _ = writeln!(f, "{v}");
        }
    }
}

fn ms(since: Instant, at: Instant) -> f64 {
    at.duration_since(since).as_secs_f64() * 1000.0
}

fn mbps(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / 1e6 / secs
}

fn xml_field(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{tag}>"))?;
    Some(body[start..start + end].to_string())
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let keys = load_keys()?;
    match args.get(1).map(String::as_str) {
        Some("upload") if args.len() == 4 => {
            upload(&keys, Path::new(&args[2]), &Log::open(Path::new(&args[3]))?).await
        }
        Some("download") if args.len() == 4 => {
            download(&keys, Path::new(&args[2]), &Log::open(Path::new(&args[3]))?).await
        }
        Some("cleanup") if args.len() == 3 => {
            let client = s3_client(&keys)?;
            cleanup(&client, &keys, &Log::open(Path::new(&args[2]))?).await
        }
        Some("single-h1") if args.len() == 4 => single_h1(&keys, args[2].parse()?, &Log::open(Path::new(&args[3]))?).await,
        _ => bail!("usage: upload <file> <log> | download <dir> <log> | cleanup <log> | single-h1 <bytes> <log>"),
    }
}

// ── Upload (#22) ────────────────────────────────────────────────────────────

fn s3_client(keys: &Keys) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(keys.user_agent())
        .http1_only()
        .pool_max_idle_per_host(16)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()?)
}

async fn upload(keys: &Keys, file: &Path, log: &Log) -> Result<()> {
    let data = Bytes::from(std::fs::read(file)?);
    let client = s3_client(keys)?;
    ensure_item(&client, keys, log).await?;

    // (part size MiB, concurrency); rising then falling so drift over time
    // does not line up with concurrency, then two runs at the 100 MiB default.
    let schedule = [
        (16, 1),
        (16, 2),
        (16, 4),
        (16, 8),
        (16, 8),
        (16, 4),
        (16, 2),
        (16, 1),
        (100, 1),
        (100, 4),
    ];
    let sent = Arc::new(AtomicU64::new(0));
    for (run, (part_mib, conc)) in schedule.into_iter().enumerate() {
        if sent.load(Ordering::SeqCst) + data.len() as u64 > UPLOAD_BUDGET {
            println!("budget: skipping run {run} and the rest");
            break;
        }
        if run > 0 {
            tokio::time::sleep(PAUSE).await;
        }
        upload_run(&client, keys, log, &data, run, part_mib * MIB, conc, &sent).await?;
    }
    cleanup(&client, keys, log).await
}

/// Create the item with one small file and wait until the metadata API
/// lists it, so the first multipart initiate is not also the item's creation.
async fn ensure_item(client: &reqwest::Client, keys: &Keys, log: &Log) -> Result<()> {
    if item_exists(client, ITEM).await? {
        return Ok(());
    }
    let readme = "Test item for a one-off measurement of parallel multipart part uploads \
                  (internetarchivecanada/ia #22). Multipart uploads made here are aborted, \
                  never completed.\n";
    let r = client
        .put(format!("{S3}/{ITEM}/README.txt"))
        .header("Authorization", keys.auth())
        .header("Content-Length", readme.len().to_string())
        .header("x-archive-auto-make-bucket", "1")
        .header("x-archive-queue-derive", "0")
        .header("x-archive-meta-collection", "test_collection")
        .header("x-archive-meta-mediatype", "data")
        .header(
            "x-archive-meta-title",
            "ia-cli parallel transfer probe 2026-10-30",
        )
        .body(readme)
        .send()
        .await?;
    let status = r.status().as_u16();
    let body = r.text().await.unwrap_or_default();
    log.write(json!({"event": "create_item", "status": status, "body": body}));
    ensure!(status == 200, "item creation answered {status}: {body}");
    for _ in 0..60 {
        if item_exists(client, ITEM).await? {
            log.write(json!({"event": "item_visible"}));
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    bail!("item not visible after 10 minutes")
}

async fn item_exists(client: &reqwest::Client, id: &str) -> Result<bool> {
    let v: Value = client
        .get(format!("https://archive.org/metadata/{id}"))
        .send()
        .await?
        .json()
        .await?;
    Ok(v.get("files").is_some())
}

#[allow(clippy::too_many_arguments)]
async fn upload_run(
    client: &reqwest::Client,
    keys: &Keys,
    log: &Log,
    data: &Bytes,
    run: usize,
    part_size: u64,
    conc: u32,
    sent: &Arc<AtomicU64>,
) -> Result<()> {
    let r = client
        .post(format!("{S3}/{ITEM}/{KEY}?uploads"))
        .header("Authorization", keys.auth())
        .header("Content-Length", "0")
        .send()
        .await?;
    let status = r.status().as_u16();
    let body = r.text().await.unwrap_or_default();
    let upload_id =
        xml_field(&body, "UploadId").with_context(|| format!("initiate {status}: {body}"))?;
    log.write(json!({"event": "initiate", "run": run, "status": status, "upload_id": upload_id}));

    let parts = (data.len() as u64).div_ceil(part_size) as u32;
    let next = Arc::new(AtomicU32::new(1));
    let t0 = Instant::now();
    let workers: Vec<_> = (0..conc)
        .map(|_| {
            let (client, keys, data, next, sent) = (
                client.clone(),
                keys.clone(),
                data.clone(),
                next.clone(),
                sent.clone(),
            );
            let upload_id = upload_id.clone();
            tokio::spawn(async move {
                let mut records = Vec::new();
                loop {
                    let n = next.fetch_add(1, Ordering::SeqCst);
                    if n > parts {
                        return records;
                    }
                    let start = (n as u64 - 1) * part_size;
                    let end = (start + part_size).min(data.len() as u64);
                    let body = data.slice(start as usize..end as usize);
                    for attempt in 1..=MAX_ATTEMPTS {
                        sent.fetch_add(body.len() as u64, Ordering::SeqCst);
                        let rec = put_part(&client, &keys, &upload_id, n, body.clone(), t0).await;
                        let ok = rec["status"] == 200;
                        let wait = rec["retry_after"].as_u64().unwrap_or(5);
                        records.push(json!({"attempt": attempt, "rec": rec}));
                        if ok {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                    }
                }
            })
        })
        .collect();

    let mut records = Vec::new();
    for w in workers {
        records.extend(w.await?);
    }
    let wall = t0.elapsed().as_secs_f64();

    let abort = client
        .delete(format!("{S3}/{ITEM}/{KEY}?uploadId={upload_id}"))
        .header("Authorization", keys.auth())
        .send()
        .await?;
    let abort_status = abort.status().as_u16();
    log.write(
        json!({"event": "abort", "run": run, "upload_id": upload_id, "status": abort_status}),
    );

    let ok: Vec<&Value> = records
        .iter()
        .map(|r| &r["rec"])
        .filter(|r| r["status"] == 200)
        .collect();
    let failed = records.len() - ok.len();
    let send_rates: Vec<f64> = ok.iter().filter_map(|r| r["send_mbps"].as_f64()).collect();
    let waits: Vec<f64> = ok
        .iter()
        .filter_map(|r| r["answer_wait_ms"].as_f64())
        .collect();
    let summary = json!({
        "event": "upload_run", "run": run, "part_mib": part_size / MIB, "concurrency": conc,
        "parts": parts, "bytes": data.len(), "wall_s": wall, "mbps": mbps(data.len() as u64, wall),
        "parts_ok": ok.len(), "non_2xx_answers": failed,
        "median_part_send_mbps": median(send_rates), "median_answer_wait_ms": median(waits),
        "abort_status": abort_status, "parts_detail": records,
    });
    println!(
        "upload run {run}: part {} MiB x{conc}: {wall:.1} s, {:.2} MB/s, median per-part send {:.2} MB/s, \
         median answer wait {:.0} ms, non-2xx {failed}, abort {abort_status}",
        part_size / MIB,
        summary["mbps"].as_f64().unwrap_or(0.0),
        summary["median_part_send_mbps"].as_f64().unwrap_or(0.0),
        summary["median_answer_wait_ms"].as_f64().unwrap_or(0.0),
    );
    log.write(summary);
    Ok(())
}

/// One part PUT, as ia-core sends it (explicit Content-Length, Content-MD5).
/// The body goes out as 64 KiB slices; the instant the last slice is handed
/// to the connection marks the end of the send, and the rest is the wait
/// for IA's answer.
async fn put_part(
    client: &reqwest::Client,
    keys: &Keys,
    upload_id: &str,
    n: u32,
    body: Bytes,
    t0: Instant,
) -> Value {
    let len = body.len();
    let md5_b64 = base64::engine::general_purpose::STANDARD.encode(Md5::digest(&body));
    let done: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let chunks: Vec<Bytes> = (0..len)
        .step_by(64 * 1024)
        .map(|i| body.slice(i..(i + 64 * 1024).min(len)))
        .collect();
    let last = chunks.len() - 1;
    let done_w = done.clone();
    let stream = futures::stream::iter(chunks.into_iter().enumerate()).map(move |(i, c)| {
        if i == last {
            let _ = done_w.set(Instant::now());
        }
        Ok::<_, std::io::Error>(c)
    });
    let start = Instant::now();
    let result = client
        .put(format!(
            "{S3}/{ITEM}/{KEY}?partNumber={n}&uploadId={upload_id}"
        ))
        .header("Authorization", keys.auth())
        .header("Content-Length", len.to_string())
        .header("Content-MD5", md5_b64)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await;
    let answered = Instant::now();
    let body_done = done.get().copied();
    let base = json!({
        "part": n, "bytes": len, "start_ms": ms(t0, start), "answer_ms": ms(t0, answered),
        "body_done_ms": body_done.map(|d| ms(t0, d)),
        "send_mbps": body_done.map(|d| mbps(len as u64, d.duration_since(start).as_secs_f64())),
        "answer_wait_ms": body_done.map(|d| ms(d, answered)),
    });
    let mut rec = base;
    match result {
        Ok(r) => {
            let status = r.status().as_u16();
            let retry_after = r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let text = r.text().await.unwrap_or_default();
            rec["status"] = json!(status);
            rec["retry_after"] = json!(retry_after);
            rec["code"] = json!(xml_field(&text, "Code"));
        }
        Err(e) => {
            rec["status"] = json!(0);
            rec["error"] = json!(e.to_string());
        }
    }
    rec
}

/// Abort every unfinished upload on the item and report how many remain.
async fn cleanup(client: &reqwest::Client, keys: &Keys, log: &Log) -> Result<()> {
    for round in 0..2 {
        let body = client
            .get(format!("{S3}/{ITEM}?uploads"))
            .header("Authorization", keys.auth())
            .send()
            .await?
            .text()
            .await?;
        let uploads: Vec<(String, String)> = body
            .split("<Upload>")
            .skip(1)
            .filter_map(|b| Some((xml_field(b, "Key")?, xml_field(b, "UploadId")?)))
            .collect();
        println!(
            "cleanup round {round}: {} unfinished uploads",
            uploads.len()
        );
        log.write(json!({"event": "list_uploads", "round": round, "count": uploads.len()}));
        if uploads.is_empty() {
            return Ok(());
        }
        for (key, id) in uploads {
            let s = client
                .delete(format!("{S3}/{ITEM}/{key}?uploadId={id}"))
                .header("Authorization", keys.auth())
                .send()
                .await?
                .status()
                .as_u16();
            log.write(json!({"event": "cleanup_abort", "key": key, "upload_id": id, "status": s}));
        }
    }
    bail!("uploads remain after cleanup")
}

// ── Download (#16, #15) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// One GET without Range, as `ia download` sends.
    Single,
    /// N Range requests, each on its own HTTP/1.1 connection.
    Http1,
    /// N Range requests on one shared client, multiplexed over HTTP/2.
    Http2,
}

fn dl_client(keys: &Keys, http1: bool) -> Result<reqwest::Client> {
    let b = reqwest::Client::builder()
        .user_agent(keys.user_agent())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60));
    Ok(if http1 { b.http1_only() } else { b }.build()?)
}

async fn download(keys: &Keys, dir: &Path, log: &Log) -> Result<()> {
    let meta: Value = dl_client(keys, false)?
        .get(format!("https://archive.org/metadata/{DL_ITEM}"))
        .send()
        .await?
        .json()
        .await?;
    let file = meta["files"]
        .as_array()
        .context("no files")?
        .iter()
        .find(|f| f["name"] == DL_FILE)
        .context("file not listed")?;
    let size: u64 = file["size"].as_str().context("no size")?.parse()?;
    let md5 = file["md5"].as_str().context("no md5")?.to_string();
    let (d1, d2, item_dir) = (
        meta["d1"].as_str().context("no d1")?.to_string(),
        meta["d2"].as_str().context("no d2")?.to_string(),
        meta["dir"].as_str().context("no dir")?.to_string(),
    );
    log.write(
        json!({"event": "dl_meta", "size": size, "md5": md5, "d1": d1, "d2": d2, "dir": item_dir}),
    );

    let received = AtomicU64::new(0);
    node_check(keys, log, &[&d1, &d2], &item_dir, "start", &received).await?;

    let schedule = [
        (Mode::Single, 1),
        (Mode::Http1, 2),
        (Mode::Http1, 4),
        (Mode::Http1, 8),
        (Mode::Http2, 4),
        (Mode::Http1, 8),
        (Mode::Http1, 4),
        (Mode::Http1, 2),
        (Mode::Single, 1),
    ];
    let dest = dir.join(DL_FILE);
    for (run, (mode, n)) in schedule.into_iter().enumerate() {
        if received.load(Ordering::SeqCst) + size > DOWNLOAD_BUDGET {
            println!("budget: skipping run {run} and the rest");
            break;
        }
        tokio::time::sleep(PAUSE).await;
        download_run(keys, log, &dest, size, &md5, run, mode, n, &received).await?;
    }
    tokio::time::sleep(PAUSE).await;
    node_check(keys, log, &[&d1, &d2], &item_dir, "end", &received).await?;
    let _ = std::fs::remove_file(&dest);
    println!(
        "downloaded {} bytes in total",
        received.load(Ordering::SeqCst)
    );
    Ok(())
}

/// Follow-up run added after review: one HTTP/1.1 stream through the
/// `/download/` redirect for the first `bytes` of the file, so the
/// single-stream baseline can be compared on the same transport as the
/// parallel runs.
async fn single_h1(keys: &Keys, bytes: u64, log: &Log) -> Result<()> {
    ensure!(bytes <= 1024 * MIB, "follow-up limited to 1 GiB");
    let url = format!("https://archive.org/download/{DL_ITEM}/{DL_FILE}?cnt=0");
    let out = Arc::new(tempfile_in_memory()?);
    let counter = Arc::new(AtomicU64::new(0));
    let rec = fetch_range(
        dl_client(keys, true)?,
        keys.clone(),
        url,
        0,
        bytes - 1,
        out,
        true,
        Instant::now(),
        counter,
    )
    .await;
    println!(
        "single-h1: {} MB/s, bytes {}, hops {}, errors {}",
        rec["mbps"], rec["bytes"], rec["hops"], rec["errors"]
    );
    log.write(json!({"event": "followup_single_h1", "rec": rec}));
    Ok(())
}

/// GET with the Authorization header kept across redirects to *.archive.org,
/// as ia-core's fetch_response does. Returns the final response and its host.
async fn fetch(
    client: &reqwest::Client,
    keys: &Keys,
    url: &str,
    range: Option<(u64, u64)>,
) -> Result<(reqwest::Response, String)> {
    let mut url = reqwest::Url::parse(url)?;
    for _ in 0..10 {
        let mut req = client.get(url.clone()).header("Authorization", keys.auth());
        if let Some((a, b)) = range {
            req = req.header("Range", format!("bytes={a}-{b}"));
        }
        let r = req.send().await?;
        if r.status().is_redirection() {
            let loc = r
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .context("redirect without location")?;
            let next = url.join(loc)?;
            let host = next.host_str().unwrap_or_default();
            ensure!(
                host == "archive.org" || host.ends_with(".archive.org"),
                "redirect off archive.org: {next}"
            );
            url = next;
            continue;
        }
        let host = url.host_str().unwrap_or_default().to_string();
        return Ok((r, host));
    }
    bail!("too many redirects")
}

/// Fetch bytes [a, b] into `out` at offset `a`, resuming from the byte reached
/// on a stream error (up to 3 times). Returns the per-request record.
#[allow(clippy::too_many_arguments)]
async fn fetch_range(
    client: reqwest::Client,
    keys: Keys,
    url: String,
    a: u64,
    b: u64,
    out: Arc<File>,
    range: bool,
    t0: Instant,
    received: Arc<AtomicU64>,
) -> Value {
    let start = Instant::now();
    let mut pos = a;
    let mut errors = Vec::new();
    let mut hops = Vec::new();
    while pos <= b && errors.len() <= 3 {
        let r = if range || pos > a {
            Some((pos, b))
        } else {
            None
        };
        let (resp, host) = match fetch(&client, &keys, &url, r).await {
            Ok(x) => x,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        let first_byte_ms = ms(start, Instant::now());
        let status = resp.status().as_u16();
        let version = format!("{:?}", resp.version());
        hops.push(json!({"node": host, "status": status, "version": version, "first_byte_ms": first_byte_ms}));
        if !(status == 200 || status == 206) {
            errors.push(format!("status {status}"));
            break;
        }
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    if let Err(e) = out.write_at(&c, pos) {
                        errors.push(e.to_string());
                        break;
                    }
                    pos += c.len() as u64;
                    received.fetch_add(c.len() as u64, Ordering::SeqCst);
                }
                Err(e) => {
                    errors.push(e.to_string());
                    break;
                }
            }
        }
    }
    let end = Instant::now();
    let bytes = pos - a;
    json!({
        "from": a, "to": b, "bytes": bytes, "complete": pos == b + 1, "start_ms": ms(t0, start),
        "end_ms": ms(t0, end), "mbps": mbps(bytes, end.duration_since(start).as_secs_f64()),
        "hops": hops, "errors": errors,
    })
}

#[allow(clippy::too_many_arguments)]
async fn download_run(
    keys: &Keys,
    log: &Log,
    dest: &Path,
    size: u64,
    md5: &str,
    run: usize,
    mode: Mode,
    n: u64,
    received: &AtomicU64,
) -> Result<()> {
    let out = Arc::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .open(dest)?,
    );
    out.set_len(size)?;
    let url = format!("https://archive.org/download/{DL_ITEM}/{DL_FILE}?cnt=0");
    let shared = dl_client(keys, mode == Mode::Http1)?;
    let counter = Arc::new(AtomicU64::new(0));
    let chunk = size / n;
    let t0 = Instant::now();
    let tasks: Vec<_> = (0..n)
        .map(|i| {
            let a = i * chunk;
            let b = if i == n - 1 { size - 1 } else { a + chunk - 1 };
            // HTTP/1.1 mode: a client per worker, so each has its own connection.
            let client = if mode == Mode::Http1 {
                dl_client(keys, true)
            } else {
                Ok(shared.clone())
            };
            let (keys, url, out, counter) =
                (keys.clone(), url.clone(), out.clone(), counter.clone());
            tokio::spawn(async move {
                match client {
                    Ok(c) => {
                        fetch_range(c, keys, url, a, b, out, mode != Mode::Single, t0, counter)
                            .await
                    }
                    Err(e) => json!({"errors": [e.to_string()]}),
                }
            })
        })
        .collect();
    let mut requests = Vec::new();
    for t in tasks {
        requests.push(t.await?);
    }
    let wall = t0.elapsed().as_secs_f64();
    out.sync_all()?;
    drop(out);
    received.fetch_add(counter.load(Ordering::SeqCst), Ordering::SeqCst);

    let got = md5_file(dest)?;
    let rates: Vec<f64> = requests.iter().filter_map(|r| r["mbps"].as_f64()).collect();
    let nodes: Vec<&Value> = requests
        .iter()
        .flat_map(|r| {
            r["hops"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|h| &h["node"])
        })
        .collect();
    let versions: Vec<&Value> = requests
        .iter()
        .flat_map(|r| {
            r["hops"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|h| &h["version"])
        })
        .collect();
    let summary = json!({
        "event": "download_run", "run": run, "mode": format!("{mode:?}"), "streams": n, "bytes": size,
        "wall_s": wall, "mbps": mbps(size, wall), "md5_ok": got == md5, "md5": got,
        "per_stream_mbps": rates, "nodes": nodes, "versions": versions, "requests": requests,
    });
    println!(
        "download run {run}: {mode:?} x{n}: {wall:.1} s, {:.2} MB/s, md5 {}, per-stream {:?}, nodes {:?}",
        mbps(size, wall),
        if got == md5 { "ok" } else { "MISMATCH" },
        summary["per_stream_mbps"].as_array().map(|a| a.iter().map(|v| (v.as_f64().unwrap_or(0.0) * 100.0).round() / 100.0).collect::<Vec<_>>()),
        nodes,
    );
    log.write(summary);
    Ok(())
}

fn md5_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = File::open(path)?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 8 * MIB as usize];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A 64 MiB Range request straight to each datanode, bypassing the
/// `/download/` redirect (input for #15).
async fn node_check(
    keys: &Keys,
    log: &Log,
    nodes: &[&str],
    dir: &str,
    when: &str,
    received: &AtomicU64,
) -> Result<()> {
    let client = dl_client(keys, true)?;
    let scratch = Arc::new(tempfile_in_memory()?);
    for node in nodes {
        let url = format!("https://{node}{dir}/{DL_FILE}");
        let counter = Arc::new(AtomicU64::new(0));
        let rec = fetch_range(
            client.clone(),
            keys.clone(),
            url,
            0,
            NODE_CHECK_BYTES - 1,
            scratch.clone(),
            true,
            Instant::now(),
            counter.clone(),
        )
        .await;
        received.fetch_add(counter.load(Ordering::SeqCst), Ordering::SeqCst);
        println!(
            "node check {when} {node}: {} MB/s, hops {}, errors {}",
            rec["mbps"], rec["hops"], rec["errors"]
        );
        log.write(json!({"event": "node_check", "when": when, "node": node, "rec": rec}));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}

/// A scratch file for the node check, unlinked at once.
fn tempfile_in_memory() -> Result<File> {
    let p = std::env::temp_dir().join(format!("probe-node-check-{}", std::process::id()));
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&p)?;
    std::fs::remove_file(&p)?;
    Ok(f)
}
