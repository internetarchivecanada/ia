# Bulk Metadata Fetching Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend `ia metadata` to fetch metadata for multiple items concurrently, outputting JSONL to stdout with live progress on stderr.

**Architecture:** Channel-based pipeline — producers (args, itemlist, search, stdin) feed identifiers into a bounded `tokio::mpsc::channel(256)`, a consumer pool fetches metadata concurrently (semaphore-bounded), results stream to stdout as JSONL.

**Tech Stack:** tokio mpsc channels, futures::stream::FuturesUnordered, indicatif (stderr progress), serde_json (JSONL output), existing ia-core metadata::get() + search::scrape() + joblog

**Design doc:** `docs/plans/2026-02-21-bulk-metadata-design.md`

---

### Task 1: Update MetadataArgs to Accept Multiple Identifiers

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:6-22`
- Modify: `ia-cli/src/main.rs:131`

**Step 1: Write the failing test**

No test yet — this is a structural change. We'll verify by compiling.

**Step 2: Update MetadataArgs struct**

In `ia-cli/src/commands/metadata.rs`, change the Args struct to accept multiple identifiers and add `--itemlist` and `--search` flags:

```rust
use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::PathBuf;

use ia_core::IaClient;

#[derive(Args)]
pub struct MetadataArgs {
    /// Item identifier(s)
    pub identifiers: Vec<String>,

    /// File containing item identifiers (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// Fetch metadata for items matching search query
    #[arg(short = 's', long)]
    pub search: Option<String>,

    /// Check if item exists (exit 0 if yes, 1 if no)
    #[arg(short = 'e', long)]
    pub exists: bool,

    /// List file formats in the item
    #[arg(short = 'F', long)]
    pub formats: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,
}
```

**Step 3: Update run() signature to receive global args**

In `ia-cli/src/commands/metadata.rs`, update `run()` to accept `quiet`, `jobs`, `joblog_path`, and `retry_failed` — matching download's pattern:

```rust
pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
```

**Step 4: Update main.rs to pass global args to metadata**

In `ia-cli/src/main.rs:131`, change:

```rust
Commands::Metadata(args) => commands::metadata::run(&client, args, cli.quiet, cli.jobs, cli.joblog, cli.retry_failed).await?,
```

**Step 5: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: compiles (existing single-identifier behavior temporarily broken — we'll fix in next task)

**Step 6: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/src/main.rs
git commit -m "refactor(metadata): accept multiple identifiers and global args"
```

---

### Task 2: Implement Single vs Bulk Mode Branching

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Write test for flag validation**

Add to `ia-cli/tests/metadata.rs` (or create it). This is a CLI integration test using `assert_cmd`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn metadata_rejects_exists_with_multiple_ids() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "id1", "id2", "--exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--exists"));
}

#[test]
fn metadata_rejects_formats_with_itemlist() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "--itemlist", "some.txt", "--formats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--formats"));
}

#[test]
fn metadata_rejects_pretty_with_search() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "--search", "nasa", "--pretty"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--pretty"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --test metadata`
Expected: FAIL (tests don't exist yet or behavior doesn't match)

**Step 3: Implement single vs bulk branching in run()**

```rust
pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    let is_bulk = args.identifiers.len() > 1
        || args.itemlist.is_some()
        || args.search.is_some();

    // Flags not supported in bulk mode
    if is_bulk {
        if args.exists {
            bail!("--exists is not supported with multiple identifiers");
        }
        if args.formats {
            bail!("--formats is not supported with multiple identifiers");
        }
        if args.pretty {
            bail!("--pretty is not supported with multiple identifiers (pipe to `jq .` instead)");
        }
    }

    // Single-item mode: existing behavior
    if !is_bulk && !retry_failed {
        let identifier = args.identifiers.first()
            .ok_or_else(|| anyhow::anyhow!("no identifier provided"))?;
        return run_single(client, identifier, &args).await;
    }

    // Bulk mode
    run_bulk(client, args, quiet, jobs, joblog_path, retry_failed).await
}

async fn run_single(client: &IaClient, identifier: &str, args: &MetadataArgs) -> Result<()> {
    if args.exists {
        let exists = client
            .item_exists(identifier)
            .await
            .context(format!("failed to check existence of {identifier}"))?;
        if !exists {
            std::process::exit(1);
        }
        return Ok(());
    }

    let item = client
        .get_item(identifier)
        .await
        .context(format!("failed to fetch metadata for {identifier}"))?;

    if args.formats {
        let mut formats: Vec<String> = item
            .files
            .iter()
            .filter_map(|f| f.format.clone())
            .collect();
        formats.sort();
        formats.dedup();
        for fmt in formats {
            println!("{fmt}");
        }
        return Ok(());
    }

    let json = if args.pretty {
        serde_json::to_string_pretty(&item)?
    } else {
        serde_json::to_string(&item)?
    };
    println!("{json}");
    Ok(())
}

async fn run_bulk(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    // Placeholder — will be implemented in Task 3-5
    bail!("bulk metadata not yet implemented")
}
```

**Step 4: Also detect stdin in single-item mode**

If no identifiers, no `--itemlist`, no `--search`, and stdin is piped, that's also bulk mode. Add stdin detection to the `is_bulk` check:

```rust
let stdin_piped = atty::isnt(atty::Stream::Stdin)
    && args.identifiers.is_empty()
    && args.itemlist.is_none()
    && args.search.is_none();

let is_bulk = args.identifiers.len() > 1
    || args.itemlist.is_some()
    || args.search.is_some()
    || stdin_piped;
```

**Step 5: Run tests**

Run: `cargo test -p ia-cli --test metadata`
Expected: PASS for flag validation tests

**Step 6: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/metadata.rs
git commit -m "feat(metadata): single vs bulk mode branching with flag validation"
```

---

### Task 3: Implement Channel-Based Identifier Pipeline

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

This task wires up the producer side of the pipeline — collecting identifiers from all sources into a channel.

**Step 1: Write test for identifier collection from itemlist**

In `ia-cli/tests/metadata.rs`, add a wiremock-based test:

```rust
#[tokio::test]
async fn metadata_bulk_from_itemlist() {
    // Create a temp itemlist file with 3 identifiers
    let dir = tempfile::tempdir().unwrap();
    let itemlist = dir.path().join("items.txt");
    std::fs::write(&itemlist, "item1\nitem2\n# comment\n\nitem3\n").unwrap();

    // This is an integration test — we'll test the full CLI with wiremock
    // For now, just verify the file parses correctly
    let content = std::fs::read_to_string(&itemlist).unwrap();
    let ids: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(ids, vec!["item1", "item2", "item3"]);
}
```

**Step 2: Implement the producer functions and channel setup in run_bulk()**

```rust
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::search::SearchOpts;

async fn run_bulk(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    // Handle --retry-failed
    let retry_ids = if retry_failed {
        if let Some(ref path) = joblog_path {
            let entries = ia_core::joblog::read(path)
                .context(format!("failed to read joblog: {}", path.display()))?;
            // Filter for metadata operations that failed
            let failed: Vec<String> = entries
                .iter()
                .filter(|e| e.op == "metadata" && e.status == "error")
                .map(|e| e.item.clone())
                .collect();
            let mut unique = failed;
            unique.sort();
            unique.dedup();
            if unique.is_empty() {
                eprintln!("{} No failed metadata items in joblog", console::style("✓").green());
                return Ok(());
            }
            Some(unique)
        } else {
            bail!("--retry-failed requires --joblog");
        }
    } else {
        None
    };

    // Open joblog writer
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Set up channel
    let (tx, rx) = mpsc::channel::<String>(256);

    // Spawn producers
    if let Some(ids) = retry_ids {
        // --retry-failed: use failed IDs as source
        let tx = tx.clone();
        tokio::spawn(async move {
            for id in ids {
                if tx.send(id).await.is_err() {
                    break;
                }
            }
        });
    } else {
        // Producer: command-line identifiers
        if !args.identifiers.is_empty() {
            let ids = args.identifiers.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                for id in ids {
                    if tx.send(id).await.is_err() {
                        break;
                    }
                }
            });
        }

        // Producer: --itemlist
        if let Some(path) = &args.itemlist {
            let content = std::fs::read_to_string(path)
                .context(format!("failed to read itemlist: {}", path.display()))?;
            let tx = tx.clone();
            tokio::spawn(async move {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        if tx.send(trimmed.to_string()).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }

        // Producer: --search (streams results as they arrive)
        if let Some(ref query) = args.search {
            let tx = tx.clone();
            let client = client.clone();
            let query = query.clone();
            tokio::spawn(async move {
                let opts = SearchOpts::default();
                let mut stream = ia_core::search::scrape(&client, &query, &opts);
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(item) => {
                            if tx.send(item.identifier).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("search error: {e}");
                            break;
                        }
                    }
                }
            });
        }

        // Producer: stdin
        if args.identifiers.is_empty()
            && args.itemlist.is_none()
            && args.search.is_none()
            && atty::isnt(atty::Stream::Stdin)
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                use std::io::BufRead;
                let stdin = std::io::stdin();
                for line in stdin.lock().lines() {
                    match line {
                        Ok(l) => {
                            let trimmed = l.trim().to_string();
                            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                                if tx.send(trimmed).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    }

    // Drop the original sender so channel closes when all producers finish
    drop(tx);

    // Consumer will be implemented in Task 4
    consume_metadata(client, rx, jobs, quiet, joblog).await
}
```

**Step 3: Add a stub consumer**

```rust
async fn consume_metadata(
    client: &IaClient,
    mut rx: mpsc::Receiver<String>,
    jobs: usize,
    quiet: u8,
    joblog: Option<JoblogWriter>,
) -> Result<()> {
    while let Some(id) = rx.recv().await {
        eprintln!("would fetch: {id}");
    }
    Ok(())
}
```

**Step 4: Verify the IaClient is Clone**

Check `ia-core/src/client.rs` — if `IaClient` doesn't implement `Clone`, we need to wrap it in `Arc`. The search producer needs to own a client reference. `reqwest_middleware::ClientWithMiddleware` is `Clone`, so `IaClient` should be too. If not, add `#[derive(Clone)]` to `IaClient`.

**Step 5: Run cargo check**

Run: `cargo check -p ia-cli`
Expected: compiles

**Step 6: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "feat(metadata): channel-based identifier pipeline with producers"
```

---

### Task 4: Implement Concurrent Metadata Consumer

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Write a wiremock integration test**

Create `ia-cli/tests/metadata_bulk.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_metadata_response(identifier: &str) -> serde_json::Value {
    serde_json::json!({
        "metadata": {
            "identifier": identifier,
            "title": format!("Title for {identifier}"),
            "mediatype": "texts"
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    })
}

#[tokio::test]
async fn bulk_metadata_outputs_jsonl() {
    let mock = MockServer::start().await;

    for id in ["item1", "item2", "item3"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_metadata_response(id)),
            )
            .mount(&mock)
            .await;
    }

    // Extract host:port from mock URI
    let uri = mock.uri();
    let host = uri.strip_prefix("http://").unwrap();

    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", host,
            "--insecure",
            "-q",
            "metadata", "item1", "item2", "item3",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 JSONL lines, got: {stdout}");

    // Each line should be valid JSON with an identifier
    for line in &lines {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("invalid JSON line: {e}\nline: {line}"));
        assert!(parsed["metadata"]["identifier"].is_string());
    }
}
```

**Step 2: Run the test to verify it fails**

Run: `cargo test -p ia-cli --test metadata_bulk`
Expected: FAIL (consumer is a stub)

**Step 3: Implement consume_metadata()**

Replace the stub in `ia-cli/src/commands/metadata.rs`:

```rust
use futures::stream::FuturesUnordered;
use std::io::Write;
use std::time::Instant;

async fn consume_metadata(
    client: &IaClient,
    mut rx: mpsc::Receiver<String>,
    jobs: usize,
    quiet: u8,
    joblog: Option<JoblogWriter>,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(jobs));
    let mut futures = FuturesUnordered::new();
    let stdout = std::io::stdout();
    let start = Instant::now();

    let mut total_sent = 0u64;
    let mut total_done = 0u64;
    let mut total_errors = 0u64;

    // Progress bar on stderr
    let progress = if quiet == 0 {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(
            indicatif::ProgressStyle::with_template(
                "{spinner:.cyan} Fetching metadata [{pos}/?] {msg}"
            ).unwrap()
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        Some(pb)
    } else {
        None
    };

    loop {
        tokio::select! {
            // Try to receive a new identifier
            id = rx.recv() => {
                match id {
                    Some(identifier) => {
                        total_sent += 1;
                        let permit = semaphore.clone().acquire_owned().await?;
                        let client = client.clone();
                        let id = identifier.clone();
                        futures.push(tokio::spawn(async move {
                            let start = Instant::now();
                            let result = ia_core::metadata::get(&client, &id).await;
                            let elapsed = start.elapsed();
                            drop(permit);
                            (id, result, elapsed)
                        }));
                    }
                    None => {
                        // Channel closed — all producers done
                        // Update progress bar to show total
                        if let Some(ref pb) = progress {
                            pb.set_length(total_sent);
                            pb.set_style(
                                indicatif::ProgressStyle::with_template(
                                    "{spinner:.cyan} Fetching metadata [{pos}/{len}] {bar:20.cyan/dim} {msg}"
                                ).unwrap()
                                .progress_chars("━╸─")
                            );
                        }
                        break;
                    }
                }
            }
            // Process completed futures
            Some(result) = futures.next(), if !futures.is_empty() => {
                let (id, fetch_result, elapsed) = result.context("metadata task panicked")?;
                total_done += 1;
                if let Some(ref pb) = progress {
                    pb.set_position(total_done);
                }

                match fetch_result {
                    Ok(item) => {
                        let json = serde_json::to_string(&item)?;
                        let mut out = stdout.lock();
                        writeln!(out, "{json}")?;

                        if let Some(ref jl) = joblog {
                            let bytes = json.len() as u64;
                            jl.write(
                                &JoblogEntry::new("metadata", &id, "")
                                    .ok(bytes, elapsed.as_millis() as u64),
                            );
                        }
                    }
                    Err(e) => {
                        total_errors += 1;
                        if quiet == 0 {
                            if let Some(ref pb) = progress {
                                pb.suspend(|| {
                                    eprintln!("{}: {e}", console::style(&id).red());
                                });
                            }
                        }

                        if let Some(ref jl) = joblog {
                            jl.write(
                                &JoblogEntry::new("metadata", &id, "")
                                    .error(&e.to_string(), 0),
                            );
                        }
                    }
                }
            }
        }
    }

    // Drain remaining futures after channel closed
    while let Some(result) = futures.next().await {
        let (id, fetch_result, elapsed) = result.context("metadata task panicked")?;
        total_done += 1;
        if let Some(ref pb) = progress {
            pb.set_position(total_done);
        }

        match fetch_result {
            Ok(item) => {
                let json = serde_json::to_string(&item)?;
                let mut out = stdout.lock();
                writeln!(out, "{json}")?;

                if let Some(ref jl) = joblog {
                    let bytes = json.len() as u64;
                    jl.write(
                        &JoblogEntry::new("metadata", &id, "")
                            .ok(bytes, elapsed.as_millis() as u64),
                    );
                }
            }
            Err(e) => {
                total_errors += 1;
                if quiet == 0 {
                    if let Some(ref pb) = progress {
                        pb.suspend(|| {
                            eprintln!("{}: {e}", console::style(&id).red());
                        });
                    }
                }

                if let Some(ref jl) = joblog {
                    jl.write(
                        &JoblogEntry::new("metadata", &id, "")
                            .error(&e.to_string(), 0),
                    );
                }
            }
        }
    }

    // Summary
    if let Some(ref pb) = progress {
        pb.finish_and_clear();
    }
    if quiet == 0 {
        let elapsed = start.elapsed().as_secs_f64();
        let error_str = if total_errors > 0 {
            format!(" ({} errors)", console::style(total_errors).red())
        } else {
            String::new()
        };
        eprintln!(
            "Fetched {} items{} in {:.1}s",
            console::style(total_done).bold(),
            error_str,
            elapsed,
        );
    }

    if total_errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}
```

**Step 4: Run the test**

Run: `cargo test -p ia-cli --test metadata_bulk`
Expected: PASS

**Step 5: Run all tests**

Run: `cargo test --workspace`
Expected: all pass

**Step 6: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/metadata_bulk.rs
git commit -m "feat(metadata): concurrent bulk metadata consumer with JSONL output"
```

---

### Task 5: Integration Tests for Error Handling and Joblog

**Files:**
- Modify: `ia-cli/tests/metadata_bulk.rs`

**Step 1: Write test for 404 errors going to stderr only**

```rust
#[tokio::test]
async fn bulk_metadata_errors_go_to_stderr() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/good-item"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_metadata_response("good-item")),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/bad-item"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let host = mock.uri().strip_prefix("http://").unwrap().to_string();

    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", &host,
            "--insecure",
            "metadata", "good-item", "bad-item",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // stdout should only have the good item
    let lines: Vec<&str> = stdout.trim().lines()
        .filter(|l| l.starts_with('{'))
        .collect();
    assert_eq!(lines.len(), 1);

    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["metadata"]["identifier"], "good-item");

    // stderr should mention the bad item
    assert!(stderr.contains("bad-item"));
}
```

**Step 2: Write test for joblog integration**

```rust
#[tokio::test]
async fn bulk_metadata_writes_joblog() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/item1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_metadata_response("item1")),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/item2"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");
    let host = mock.uri().strip_prefix("http://").unwrap().to_string();

    Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", &host,
            "--insecure",
            "-q",
            "--joblog", joblog_path.to_str().unwrap(),
            "metadata", "item1", "item2",
        ])
        .output()
        .unwrap();

    // Read joblog and verify entries
    let content = std::fs::read_to_string(&joblog_path).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 joblog entries");

    for line in &lines {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(entry["op"], "metadata");
        let status = entry["status"].as_str().unwrap();
        assert!(status == "ok" || status == "error");
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p ia-cli --test metadata_bulk`
Expected: PASS

**Step 4: Commit**

```bash
git add ia-cli/tests/metadata_bulk.rs
git commit -m "test(metadata): integration tests for bulk error handling and joblog"
```

---

### Task 6: Integration Test for Itemlist and Stdin Input

**Files:**
- Modify: `ia-cli/tests/metadata_bulk.rs`

**Step 1: Write test for --itemlist**

```rust
#[tokio::test]
async fn bulk_metadata_from_itemlist() {
    let mock = MockServer::start().await;

    for id in ["alpha", "beta"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_metadata_response(id)),
            )
            .mount(&mock)
            .await;
    }

    let dir = tempfile::tempdir().unwrap();
    let itemlist = dir.path().join("items.txt");
    std::fs::write(&itemlist, "alpha\n# comment\n\nbeta\n").unwrap();

    let host = mock.uri().strip_prefix("http://").unwrap().to_string();

    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", &host,
            "--insecure",
            "-q",
            "metadata",
            "--itemlist", itemlist.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}
```

**Step 2: Write test for stdin piping**

```rust
#[tokio::test]
async fn bulk_metadata_from_stdin() {
    let mock = MockServer::start().await;

    for id in ["fromstdin1", "fromstdin2"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_metadata_response(id)),
            )
            .mount(&mock)
            .await;
    }

    let host = mock.uri().strip_prefix("http://").unwrap().to_string();

    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", &host,
            "--insecure",
            "-q",
            "metadata",
        ])
        .write_stdin("fromstdin1\nfromstdin2\n")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}
```

**Step 3: Run tests**

Run: `cargo test -p ia-cli --test metadata_bulk`
Expected: PASS

**Step 4: Commit**

```bash
git add ia-cli/tests/metadata_bulk.rs
git commit -m "test(metadata): integration tests for itemlist and stdin input"
```

---

### Task 7: Verify IaClient is Clone and Handle Edge Cases

**Files:**
- Possibly modify: `ia-core/src/client.rs` (add `Clone` if needed)
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Check if IaClient derives Clone**

Run: `grep -n 'derive.*Clone' ia-core/src/client.rs`

If it doesn't, add `#[derive(Clone)]` to `IaClient`. `reqwest_middleware::ClientWithMiddleware` is `Clone`, `IaConfig` is `Clone` (or needs to be), and `String` is `Clone`, so this should work.

**Step 2: Handle the edge case — no identifiers and nothing piped**

When no identifiers, no `--itemlist`, no `--search`, and stdin is a TTY, show a helpful error:

```rust
if args.identifiers.is_empty()
    && args.itemlist.is_none()
    && args.search.is_none()
    && !retry_failed
    && atty::is(atty::Stream::Stdin)
{
    bail!("no identifier provided. Pass identifiers as arguments, use --itemlist, --search, or pipe to stdin.");
}
```

This goes at the top of `run()`, after the `is_bulk` / flag validation checks.

**Step 3: Write a test for this edge case**

```rust
#[test]
fn metadata_no_args_shows_error() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifier"));
}
```

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: all pass

**Step 5: Commit**

```bash
git add ia-core/src/client.rs ia-cli/src/commands/metadata.rs ia-cli/tests/metadata_bulk.rs
git commit -m "fix(metadata): ensure IaClient is Clone, handle no-args edge case"
```

---

### Task 8: Run Full Test Suite, Clippy, and Final Cleanup

**Files:**
- Possibly modify: `ia-cli/src/commands/metadata.rs` (clippy fixes)

**Step 1: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings. Fix any that arise.

**Step 2: Run full test suite**

Run: `cargo test --workspace`
Expected: all pass

**Step 3: Verify single-item mode still works**

Run: `cargo run -p ia-cli -- metadata --help`
Expected: shows updated help with `[identifiers]...`, `--itemlist`, `--search`

**Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "chore(metadata): clippy and cleanup"
```

---

### Task 9: Update Documentation

**Files:**
- Modify: `CLAUDE.md` (if any new conventions)
- Modify: memory `MEMORY.md`

**Step 1: Update MEMORY.md**

Add to the Key CLI Files section:
- `commands/metadata.rs` — Metadata with bulk mode: channel-based pipeline, --itemlist, --search, stdin, JSONL output

Add to Implementation Status or relevant section:
- Bulk metadata fetch feature

**Step 2: Commit**

```bash
git add CLAUDE.md docs/ MEMORY.md
git commit -m "docs: update for bulk metadata feature"
```

---

## Dependency Graph

```
Task 1 (args struct) → Task 2 (branching) → Task 3 (pipeline) → Task 4 (consumer) → Task 5 (error tests) → Task 6 (input tests) → Task 7 (edge cases) → Task 8 (cleanup) → Task 9 (docs)
```

All tasks are sequential — each builds on the previous.
