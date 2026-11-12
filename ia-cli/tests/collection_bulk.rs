//! `ia collection create --spreadsheet <FILE>`: many collections from one
//! CSV or JSONL file (#147).
//!
//! All tests are hermetic. Each test writes a config whose host is the
//! wiremock server (or a closed loopback port when no request may be sent),
//! passes it with `--config-file`, and removes the `IA_*` environment
//! variables, so any request can only reach the mock. Servers that must see
//! no request other than the ones a test mounts have a catch-all with
//! `expect(0)`, checked when the server is dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{any, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn host_of(server: &MockServer) -> String {
    server.uri().replace("http://", "")
}

fn write_config(contents: &str) -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), contents).unwrap();
    f
}

/// A config for `host` over plain HTTP with test S3 keys.
fn config_with_keys(host: &str) -> NamedTempFile {
    write_config(&format!(
        "[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = test-access\nsecret = test-secret\n"
    ))
}

/// A config with no keys and a host where nothing listens.
fn config_without_keys() -> NamedTempFile {
    write_config("[general]\nhost = 127.0.0.1:1\nsecure = false\n")
}

/// `ia --config-file <cfg> collection create <args>`, run in `dir`.
fn run_create(cfg: &Path, dir: &Path, args: &[&str]) -> Output {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .env_remove("IA_CONFIG_FILE")
        .current_dir(dir)
        .arg("--config-file")
        .arg(cfg)
        .args(["collection", "create"])
        .args(args)
        .write_stdin("")
        .output()
        .unwrap()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, contents).unwrap();
    p
}

fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l}: {e}")))
        .collect()
}

/// A create PUT for `identifier` answered with `status`, expected `times`.
async fn mount_create(server: &MockServer, identifier: &str, status: u16, times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/{identifier}")))
        .respond_with(
            ResponseTemplate::new(status).set_body_string(if status == 200 {
                String::new()
            } else {
                "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>".into()
            }),
        )
        .expect(times)
        .mount(server)
        .await;
}

/// Any request no other mock matched fails the test.
async fn mount_unexpected(server: &MockServer) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(599))
        .expect(0)
        .with_priority(u8::MAX)
        .mount(server)
        .await;
}

/// The header value a create PUT for `identifier` carried, if any.
async fn header_of(server: &MockServer, identifier: &str, name: &str) -> Option<String> {
    let requests = server.received_requests().await.unwrap_or_default();
    requests
        .iter()
        .find(|r| r.url.path() == format!("/{identifier}"))
        .and_then(|r| r.headers.get(name))
        .map(|v| v.to_str().unwrap().to_string())
}

// ─── Creating ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn csv_creates_one_collection_per_row_with_merged_metadata() {
    let server = MockServer::start().await;
    mount_create(&server, "coll-one", 200, 1).await;
    mount_create(&server, "coll-two", 200, 1).await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier,title,description,collection\n\
         coll-one,First,The first one,\n\
         coll-two,Second,,other-parent\n",
    );

    let out = run_create(
        cfg.path(),
        dir.path(),
        &[
            "--spreadsheet",
            "cols.csv",
            "--collection",
            "shared-parent",
            "-m",
            "hidden:true",
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("created:") && stdout.contains("/details/coll-one"));
    assert!(stdout.contains("/details/coll-two"));
    assert!(
        stderr_of(&out).contains("2 created, 0 failed"),
        "{}",
        stderr_of(&out)
    );

    let one = |h: &'static str| header_of(&server, "coll-one", h);
    assert_eq!(
        one("x-archive-meta00-collection").await.as_deref(),
        Some("shared-parent")
    );
    assert_eq!(
        one("x-archive-meta00-title").await.as_deref(),
        Some("First")
    );
    assert_eq!(
        one("x-archive-meta00-description").await.as_deref(),
        // A value with spaces is sent percent-encoded inside `uri()`.
        Some("uri(The%20first%20one)")
    );
    assert_eq!(
        one("x-archive-meta00-hidden").await.as_deref(),
        Some("true")
    );
    assert_eq!(
        one("x-archive-meta00-mediatype").await.as_deref(),
        Some("collection")
    );
    assert_eq!(one("x-archive-queue-derive").await.as_deref(), Some("0"));
    assert_eq!(one("x-amz-auto-make-bucket").await.as_deref(), Some("1"));

    let two = |h: &'static str| header_of(&server, "coll-two", h);
    assert_eq!(
        two("x-archive-meta00-collection").await.as_deref(),
        Some("other-parent")
    );
    assert_eq!(two("x-archive-meta01-collection").await, None);
    assert_eq!(two("x-archive-meta00-description").await, None);
}

#[tokio::test]
async fn jsonl_creates_one_collection_per_line() {
    let server = MockServer::start().await;
    mount_create(&server, "jsonl-one", 200, 1).await;
    mount_create(&server, "jsonl-two", 200, 1).await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.jsonl",
        "{\"identifier\":\"jsonl-one\",\"title\":\"One\",\"collection\":\"parent\"}\n\
         {\"identifier\":\"jsonl-two\",\"title\":\"Two\",\"collection\":\"parent\",\"subject\":\"topic\"}\n",
    );

    let out = run_create(cfg.path(), dir.path(), &["--spreadsheet", "cols.jsonl"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        header_of(&server, "jsonl-two", "x-archive-meta00-subject")
            .await
            .as_deref(),
        Some("topic")
    );
    assert_eq!(
        header_of(&server, "jsonl-one", "x-archive-meta00-title")
            .await
            .as_deref(),
        Some("One")
    );
}

#[tokio::test]
async fn image_column_uploads_the_item_image() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/img-coll/img-coll_itemimage.png"))
        .and(header("x-archive-meta00-collection", "parent"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(&dir, "logo.png", "\u{89}PNG fake");
    write_file(
        &dir,
        "cols.csv",
        "identifier,collection,image\nimg-coll,parent,logo.png\n",
    );

    let out = run_create(cfg.path(), dir.path(), &["--spreadsheet", "cols.csv"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(stdout_of(&out).contains("/details/img-coll"));
}

#[tokio::test]
async fn a_failed_row_does_not_stop_the_others() {
    let server = MockServer::start().await;
    mount_create(&server, "good-one", 200, 1).await;
    mount_create(&server, "refused", 403, 1).await;
    mount_create(&server, "good-two", 200, 1).await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier\ngood-one\nrefused\ngood-two\n",
    );

    let out = run_create(
        cfg.path(),
        dir.path(),
        &[
            "--spreadsheet",
            "cols.csv",
            "--collection",
            "parent",
            "-j",
            "1",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("error: refused:"), "{stderr}");
    assert!(stderr.contains("2 created, 1 failed"), "{stderr}");
    let stdout = stdout_of(&out);
    assert!(stdout.contains("/details/good-one") && stdout.contains("/details/good-two"));
}

// ─── Validation ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_bad_row_is_reported_and_nothing_is_sent() {
    let server = MockServer::start().await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier,collection,image\n\
         fine-row,parent,\n\
         ab,parent,\n\
         no-parent,,\n\
         dup,parent,\n\
         dup,parent,\n\
         no-image,parent,missing.png\n",
    );

    let out = run_create(cfg.path(), dir.path(), &["--spreadsheet", "cols.csv"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("batch validation failed"), "{stderr}");
    for needle in [
        "ab",
        "no-parent: no parent collection",
        "dup: on more than one row",
        "no-image: image file not found",
    ] {
        assert!(stderr.contains(needle), "missing {needle:?} in {stderr}");
    }
    assert!(!stderr.contains("fine-row"), "{stderr}");
    assert!(stdout_of(&out).is_empty());
}

#[tokio::test]
async fn validation_error_is_json_on_stderr_with_json() {
    let server = MockServer::start().await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier\nno-parent\n");

    let out = run_create(
        cfg.path(),
        dir.path(),
        &["--spreadsheet", "cols.csv", "--json"],
    );
    assert_eq!(out.status.code(), Some(1));
    let err: Value = serde_json::from_str(stderr_of(&out).trim()).unwrap();
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no-parent: no parent collection"));
    assert!(stdout_of(&out).is_empty());
}

#[test]
fn an_empty_spreadsheet_is_an_error() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\n");
    let out = run_create(cfg.path(), dir.path(), &["--spreadsheet", "cols.csv"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("no records"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn missing_keys_stop_the_command_before_any_request() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\ncoll-one,parent\n");
    let out = run_create(cfg.path(), dir.path(), &["--spreadsheet", "cols.csv"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = stderr_of(&out).to_lowercase();
    assert!(
        stderr.contains("credentials") || stderr.contains("s3"),
        "{stderr}"
    );
    assert!(!stderr.contains("coll-one:"), "{stderr}");
}

// ─── Arguments ───────────────────────────────────────────────────────────────

#[test]
fn identifier_and_spreadsheet_conflict() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\nc-one,p\n");
    let out = run_create(
        cfg.path(),
        dir.path(),
        &["one-coll", "--spreadsheet", "cols.csv", "--collection", "p"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("cannot be used with"));
}

#[test]
fn spreadsheet_does_not_need_collection_flag() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\nc-one,p\n");
    let out = run_create(
        cfg.path(),
        dir.path(),
        &["--spreadsheet", "cols.csv", "--dry-run"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
}

// ─── Output ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn json_prints_one_object_per_row() {
    let server = MockServer::start().await;
    mount_create(&server, "json-ok", 200, 1).await;
    mount_create(&server, "json-bad", 403, 1).await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier,collection\njson-ok,p\njson-bad,p\n",
    );

    let out = run_create(
        cfg.path(),
        dir.path(),
        &["--spreadsheet", "cols.csv", "--json"],
    );
    assert_eq!(out.status.code(), Some(1));
    let mut records = json_lines(&stdout_of(&out));
    records.sort_by_key(|r| r["identifier"].as_str().unwrap().to_string());
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["identifier"], "json-bad");
    assert_eq!(records[0]["error"]["code"], "upload_failed");
    assert!(records[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Access"));
    assert_eq!(records[1]["identifier"], "json-ok");
    assert_eq!(records[1]["status"], 200);
    assert!(records[1]["url"]
        .as_str()
        .unwrap()
        .ends_with("/details/json-ok"));
    assert!(!stderr_of(&out).contains("created,"));
}

#[test]
fn dry_run_sends_nothing_and_needs_no_keys() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier,collection,title\ndry-one,p,One\ndry-two,,Two\n",
    );
    let out = run_create(
        cfg.path(),
        dir.path(),
        &[
            "--spreadsheet",
            "cols.csv",
            "--collection",
            "shared",
            "--dry-run",
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("dry-run:") && stdout.contains("dry-one"));
    assert!(stdout.contains("dry-two") && stdout.contains("shared"));
    assert!(
        stderr_of(&out).contains("would create 2 collection(s)"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn dry_run_json_prints_metadata_per_row() {
    let cfg = config_without_keys();
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\ndry-json,p\n");
    let out = run_create(
        cfg.path(),
        dir.path(),
        &["--spreadsheet", "cols.csv", "--dry-run", "--json"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let records = json_lines(&stdout_of(&out));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["identifier"], "dry-json");
    assert_eq!(records[0]["mediatype"], "collection");
    assert_eq!(records[0]["metadata"]["collection"], "p");
}

// ─── Job log ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn joblog_resume_skips_created_rows_and_retries_failed_ones() {
    let dir = TempDir::new().unwrap();
    write_file(
        &dir,
        "cols.csv",
        "identifier,collection\nlog-ok,p\nlog-bad,p\n",
    );
    let log = dir.path().join("create.jsonl");
    let log_arg = log.to_str().unwrap();

    // First run: one created, one refused.
    {
        let server = MockServer::start().await;
        mount_create(&server, "log-ok", 200, 1).await;
        mount_create(&server, "log-bad", 403, 1).await;
        mount_unexpected(&server).await;
        let cfg = config_with_keys(&host_of(&server));
        let out = run_create(
            cfg.path(),
            dir.path(),
            &["--spreadsheet", "cols.csv", "--joblog", log_arg],
        );
        assert_eq!(out.status.code(), Some(1));
    }
    let entries = json_lines(&fs::read_to_string(&log).unwrap());
    assert_eq!(entries.len(), 2);
    for e in &entries {
        assert_eq!(e["op"], "collection-create");
        let want = if e["item"] == "log-ok" { "ok" } else { "error" };
        assert_eq!(e["status"], want, "{e}");
    }

    // Second run: only the failed row is sent.
    {
        let server = MockServer::start().await;
        mount_create(&server, "log-bad", 200, 1).await;
        mount_unexpected(&server).await;
        let cfg = config_with_keys(&host_of(&server));
        let out = run_create(
            cfg.path(),
            dir.path(),
            &["--spreadsheet", "cols.csv", "--joblog", log_arg],
        );
        assert!(out.status.success(), "stderr: {}", stderr_of(&out));
        assert!(
            stderr_of(&out).contains("skipping 1 collection(s) already created"),
            "{}",
            stderr_of(&out)
        );
    }

    // Third run with --no-resume: both rows are sent again.
    {
        let server = MockServer::start().await;
        mount_create(&server, "log-ok", 200, 1).await;
        mount_create(&server, "log-bad", 200, 1).await;
        mount_unexpected(&server).await;
        let cfg = config_with_keys(&host_of(&server));
        let out = run_create(
            cfg.path(),
            dir.path(),
            &[
                "--spreadsheet",
                "cols.csv",
                "--joblog",
                log_arg,
                "--no-resume",
            ],
        );
        assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    }
}

#[tokio::test]
async fn every_row_already_created_exits_zero() {
    let dir = TempDir::new().unwrap();
    write_file(&dir, "cols.csv", "identifier,collection\ndone-row,p\n");
    let log = write_file(
        &dir,
        "create.jsonl",
        "{\"ts\":\"2026-11-12T00:00:00Z\",\"op\":\"collection-create\",\"item\":\"done-row\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":1}\n",
    );
    let server = MockServer::start().await;
    mount_unexpected(&server).await;
    let cfg = config_with_keys(&host_of(&server));
    let out = run_create(
        cfg.path(),
        dir.path(),
        &[
            "--spreadsheet",
            "cols.csv",
            "--joblog",
            log.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
}
