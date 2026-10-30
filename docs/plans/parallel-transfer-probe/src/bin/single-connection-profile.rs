//! Follow-up to ../../../parallel-transfer-measurement-2026-10-30.md: the rate
//! profile of one connection over a transfer (measurement A) and repeated
//! direct samples of the item's two datanodes (measurement B).
//!
//! GET only and anonymous: this binary sends no request other than GET, no
//! Authorization header, and never reads the IA-S3 keys. The file is public.
//!
//! Usage:
//!   single-connection-profile <results.jsonl>

use anyhow::{bail, ensure, Context, Result};
use futures::StreamExt;
use md5::{Digest, Md5};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MIB: u64 = 1024 * 1024;
const DL_ITEM: &str = "ubuntu-18.04.5-desktop-amd64";
const DL_FILE: &str = "ubuntu-18.04.5-desktop-amd64.iso";
/// Total bytes received this session may not exceed this.
const BUDGET: u64 = 8_000_000_000;
/// Measurement A logs one record per this many bytes received.
const SLICE: u64 = 16 * MIB;
const SAMPLE_BYTES: u64 = 64 * MIB;
const SAMPLES: u32 = 7;
const SAMPLE_EVERY: Duration = Duration::from_secs(600);
/// Gap between any two requests.
const PAUSE: Duration = Duration::from_secs(30);
/// Rate assumed when deciding whether a transfer finishes before the next
/// sample is due (bytes per second).
const PLAN_RATE: f64 = 4e6;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Transport {
    Http1,
    Http2,
}

#[derive(Debug, Clone, Copy)]
struct Transfer {
    /// Bytes from offset 0; `None` is the whole file.
    bytes: Option<u64>,
    transport: Transport,
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn ms(since: Instant, at: Instant) -> f64 {
    at.duration_since(since).as_secs_f64() * 1000.0
}

fn mbps(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / 1e6 / secs
}

/// Append-only JSON lines record of everything measured.
struct Log(File);

impl Log {
    fn write(&mut self, v: Value) -> Result<()> {
        writeln!(self.0, "{v}")?;
        Ok(())
    }
}

/// A fresh client, so every request starts on new connections. Redirects
/// are followed by hand to record the node.
fn client(transport: Transport) -> Result<reqwest::Client> {
    let b = reqwest::Client::builder()
        .user_agent(format!(
            "ia-parallel-transfer-probe/0.1 ({} {}; N; en)",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60));
    Ok(match transport {
        Transport::Http1 => b.http1_only(),
        Transport::Http2 => b,
    }
    .build()?)
}

/// GET `url` for bytes [0, last], following redirects within *.archive.org,
/// and read the body to the end. Records time to the first byte, the node,
/// and, when `slices` is set, one entry per `SLICE` bytes received.
async fn get_range(
    client: &reqwest::Client,
    url: &str,
    last: u64,
    slices: bool,
    md5: bool,
) -> Value {
    let start = Instant::now();
    let started_unix_ms = unix_ms();
    let mut url = match reqwest::Url::parse(url) {
        Ok(u) => u,
        Err(e) => return json!({"error": e.to_string()}),
    };
    let mut hops = Vec::new();
    let resp = loop {
        if hops.len() >= 10 {
            return json!({"started_unix_ms": started_unix_ms, "hops": hops, "error": "too many redirects"});
        }
        let r = client
            .get(url.clone())
            .header("Range", format!("bytes=0-{last}"))
            .send()
            .await;
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                return json!({"started_unix_ms": started_unix_ms, "hops": hops, "error": e.to_string()})
            }
        };
        hops.push(json!({
            "host": url.host_str().unwrap_or_default(), "status": r.status().as_u16(),
            "version": format!("{:?}", r.version()), "at_ms": ms(start, Instant::now()),
        }));
        if !r.status().is_redirection() {
            break r;
        }
        let next = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .and_then(|loc| url.join(loc).ok());
        match next {
            Some(n) if n.host_str().is_some_and(|h| h == "archive.org" || h.ends_with(".archive.org")) => url = n,
            other => {
                return json!({"started_unix_ms": started_unix_ms, "hops": hops,
                              "error": format!("bad redirect: {other:?}")})
            }
        }
    };
    let headers_at = Instant::now();
    let status = resp.status().as_u16();
    let version = format!("{:?}", resp.version());
    let node = url.host_str().unwrap_or_default().to_string();
    let mut hasher = md5.then(Md5::new);
    let mut got: u64 = 0;
    let mut first_byte_ms = None;
    let mut records = Vec::new();
    let (mut prev_at, mut prev_bytes) = (headers_at, 0u64);
    let mut error = None;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => {
                let now = Instant::now();
                first_byte_ms.get_or_insert_with(|| ms(start, now));
                if let Some(h) = hasher.as_mut() {
                    h.update(&c);
                }
                got += c.len() as u64;
                if slices && got / SLICE > prev_bytes / SLICE {
                    records.push(json!({
                        "n": records.len() + 1, "at_ms": ms(start, now), "unix_ms": unix_ms(),
                        "bytes": got, "slice_mbps": mbps(got - prev_bytes, now.duration_since(prev_at).as_secs_f64()),
                    }));
                    (prev_at, prev_bytes) = (now, got);
                }
            }
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }
    let end = Instant::now();
    if slices && got > prev_bytes {
        records.push(json!({
            "n": records.len() + 1, "at_ms": ms(start, end), "unix_ms": unix_ms(), "bytes": got,
            "slice_mbps": mbps(got - prev_bytes, end.duration_since(prev_at).as_secs_f64()), "partial": true,
        }));
    }
    let wall = end.duration_since(start).as_secs_f64();
    json!({
        "started_unix_ms": started_unix_ms, "node": node, "status": status, "version": version,
        "hops": hops, "headers_ms": ms(start, headers_at), "first_byte_ms": first_byte_ms,
        "bytes": got, "complete": got == last + 1, "wall_s": wall, "mbps": mbps(got, wall),
        "body_mbps": mbps(got, end.duration_since(headers_at).as_secs_f64()),
        "md5": hasher.map(|h| h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()),
        "slices": records, "error": error,
    })
}

struct Session {
    log: Log,
    received: u64,
    size: u64,
    md5: String,
    d1: String,
    d2: String,
    dir: String,
}

impl Session {
    /// Refuse any request that could take the session over the budget.
    fn allow(&mut self, bytes: u64, what: &str) -> Result<bool> {
        if self.received + bytes > BUDGET {
            println!("budget: skipping {what}");
            self.log.write(json!({"event": "budget_skip", "what": what, "received": self.received}))?;
            return Ok(false);
        }
        Ok(true)
    }

    async fn transfer(&mut self, t: Transfer, run: usize) -> Result<()> {
        let last = t.bytes.unwrap_or(self.size) - 1;
        if !self.allow(last + 1, &format!("run {run}"))? {
            return Ok(());
        }
        let url = format!("https://archive.org/download/{DL_ITEM}/{DL_FILE}?cnt=0");
        let rec = get_range(&client(t.transport)?, &url, last, true, t.bytes.is_none()).await;
        self.received += rec["bytes"].as_u64().unwrap_or(0);
        let md5_ok = t.bytes.is_none().then(|| rec["md5"] == self.md5.as_str());
        println!(
            "A run {run}: {} bytes {:?} via {} {}: {:.2} MB/s, first slice {:.2}, md5 ok {md5_ok:?}, error {}",
            rec["bytes"], t.transport, rec["node"], rec["version"], rec["mbps"].as_f64().unwrap_or(0.0),
            rec["slices"][0]["slice_mbps"].as_f64().unwrap_or(0.0), rec["error"],
        );
        self.log.write(json!({
            "event": "profile", "run": run, "requested_bytes": last + 1,
            "transport": format!("{:?}", t.transport), "md5_ok": md5_ok, "rec": rec,
        }))
    }

    async fn sample(&mut self, k: u32, started: Instant) -> Result<()> {
        let at_s = started.elapsed().as_secs_f64();
        let targets = [
            ("d1", format!("https://{}{}/{DL_FILE}", self.d1, self.dir)),
            ("d2", format!("https://{}{}/{DL_FILE}", self.d2, self.dir)),
            ("redirect", format!("https://archive.org/download/{DL_ITEM}/{DL_FILE}?cnt=0")),
        ];
        for (i, (which, url)) in targets.into_iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(PAUSE).await;
            }
            if !self.allow(SAMPLE_BYTES, &format!("sample {k} {which}"))? {
                continue;
            }
            let rec = get_range(&client(Transport::Http1)?, &url, SAMPLE_BYTES - 1, true, false).await;
            self.received += rec["bytes"].as_u64().unwrap_or(0);
            println!(
                "B sample {k} {which}: {} {} {:.2} MB/s, first byte {} ms, error {}",
                rec["node"], rec["status"], rec["mbps"].as_f64().unwrap_or(0.0), rec["first_byte_ms"], rec["error"],
            );
            self.log.write(json!({
                "event": "node_sample", "sample": k, "due_s": f64::from(k) * SAMPLE_EVERY.as_secs_f64(),
                "at_s": at_s, "target": which, "rec": rec,
            }))?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, out] = args.as_slice() else {
        bail!("usage: single-connection-profile <results.jsonl>");
    };
    let log = Log(OpenOptions::new().create(true).append(true).open(Path::new(out))?);

    let meta: Value = client(Transport::Http1)?
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
    let field = |k: &str| meta[k].as_str().map(str::to_string).with_context(|| format!("no {k}"));
    let mut s = Session {
        size: file["size"].as_str().context("no size")?.parse()?,
        md5: file["md5"].as_str().context("no md5")?.to_string(),
        d1: field("d1")?,
        d2: field("d2")?,
        dir: field("dir")?,
        log,
        received: 0,
    };
    s.log.write(json!({
        "event": "meta", "unix_ms": unix_ms(), "size": s.size, "md5": s.md5, "d1": s.d1, "d2": s.d2,
        "dir": s.dir, "server": meta["server"], "workable_servers": meta["workable_servers"],
    }))?;
    ensure!(s.d1 != s.d2, "item has one datanode");

    let h1 = |mib: u64| Transfer { bytes: Some(mib * MIB), transport: Transport::Http1 };
    let mut queue: VecDeque<Transfer> = VecDeque::from([
        h1(64),
        h1(256),
        h1(1024),
        Transfer { bytes: None, transport: Transport::Http1 },
        h1(64),
        h1(256),
        h1(1024),
        Transfer { bytes: Some(1024 * MIB), transport: Transport::Http2 },
    ]);

    let started = Instant::now();
    let mut next_sample = 0u32;
    let mut run = 0usize;
    let mut just_sampled = false;
    loop {
        let due = started + SAMPLE_EVERY * next_sample;
        if next_sample < SAMPLES && Instant::now() >= due {
            s.sample(next_sample, started).await?;
            next_sample += 1;
            just_sampled = true;
            tokio::time::sleep(PAUSE).await;
            continue;
        }
        if let Some(&t) = queue.front() {
            let est = Duration::from_secs_f64(t.bytes.unwrap_or(s.size) as f64 / PLAN_RATE) + PAUSE;
            // A transfer that does not fit before the next sample still runs
            // straight after a sample, the longest gap there is; that
            // sample is then late, and its actual time is recorded.
            if next_sample >= SAMPLES || Instant::now() + est <= due || just_sampled {
                queue.pop_front();
                s.transfer(t, run).await?;
                run += 1;
                just_sampled = false;
                tokio::time::sleep(PAUSE).await;
                continue;
            }
        } else if next_sample >= SAMPLES {
            break;
        }
        just_sampled = false;
        tokio::time::sleep_until(due.into()).await;
    }
    println!("received {} bytes in total", s.received);
    s.log.write(json!({"event": "done", "unix_ms": unix_ms(), "received": s.received}))
}
