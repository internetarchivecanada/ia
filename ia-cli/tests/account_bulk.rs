//! `ia account` with a list of accounts: `--itemlist <FILE>`, `--itemlist -`
//! and standard input; `--jobs`; one account named by several lines;
//! `--dry-run`; `--joblog` and resume; quiet levels; the rate-limit pause.
//!
//! All tests are hermetic, as in `tests/account.rs`, whose helpers are
//! repeated here under the same names. Each test writes a config whose host
//! is the wiremock server, passes it with `--config-file`, passes
//! `--insecure -H <mock>` as well, and removes the `IA_*` environment
//! variables, so any request can only reach the mock. Lookups are matched
//! on their form body, one mock per input line, and every server has a
//! catch-all with `expect(0)`, so a request the test did not intend fails
//! the test when the server is dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{any, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const COOKIES: &str = "[cookies]\nlogged-in-user = test%40example.com\nlogged-in-sig = test-sig\n";

const PAUSE_MESSAGE: &str = "Rate limited. Pausing all workers for 1s";

// ─── Helpers shared with tests/account.rs ────────────────────────────────────

fn host_of(server: &MockServer) -> String {
    server.uri().replace("http://", "")
}

fn write_config(contents: &str) -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), contents).unwrap();
    f
}

/// A config for `host` over plain HTTP with the test cookies.
fn config_with_cookies(host: &str) -> NamedTempFile {
    write_config(&format!(
        "[general]\nhost = {host}\nsecure = false\n\n{COOKIES}"
    ))
}

/// A config for `host` with S3 keys but no cookies.
fn config_without_cookies(host: &str) -> NamedTempFile {
    write_config(&format!(
        "[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = test-access\nsecret = test-secret\n"
    ))
}

/// The binary with the hermetic environment and no arguments yet.
fn bare_cmd() -> assert_cmd::Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .env_remove("IA_CONFIG_FILE");
    cmd
}

/// `ia --config-file <cfg> --insecure -H <host> account <args>`, with an
/// empty pipe on standard input.
fn run_account(cfg: &Path, host: &str, args: &[&str]) -> Output {
    bare_cmd()
        .arg("--config-file")
        .arg(cfg)
        .args(["--insecure", "-H", host, "account"])
        .args(args)
        .output()
        .unwrap()
}

/// As [`run_account`], with `stdin` piped to standard input.
fn run_account_stdin(cfg: &Path, host: &str, args: &[&str], stdin: &str) -> Output {
    bare_cmd()
        .arg("--config-file")
        .arg(cfg)
        .args(["--insecure", "-H", host, "account"])
        .args(args)
        .write_stdin(stdin.to_string())
        .output()
        .unwrap()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok_body(values: Value) -> Value {
    json!({"success": true, "values": values})
}

fn not_found_body() -> Value {
    json!({"success": false, "error": "account not found", "values": {}})
}

fn decode(s: &str) -> String {
    let spaced = s.replace('+', " ");
    urlencoding::decode(&spaced)
        .map(|c| c.into_owned())
        .unwrap_or(spaced)
}

/// The form fields of a request body, decoded, sorted by name.
fn form_of(body: &[u8]) -> Vec<(String, String)> {
    let body = String::from_utf8_lossy(body);
    let mut fields: Vec<(String, String)> = body
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    fields.sort();
    fields
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = list
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    v.sort();
    v
}

/// The requests the server received with `op=<op>`.
async fn requests_with_op(server: &MockServer, op: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.query_pairs().any(|(k, v)| k == "op" && v == op))
        .collect()
}

/// The `{"error": {...}}` object on stderr.
fn stderr_json_error(output: &Output) -> Value {
    let stderr = stderr_of(output);
    stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("error").is_some())
        .unwrap_or_else(|| panic!("no JSON error object on stderr: {stderr}"))
}

/// A catch-all with `expect(0)`: mounted on a server that must receive no
/// request at all.
async fn mount_nothing(server: &MockServer) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(599))
        .expect(0)
        .mount(server)
        .await;
}

// ─── Helpers for lists ───────────────────────────────────────────────────────

/// Matches a request whose form body is exactly these fields.
struct FormIs(Vec<(String, String)>);

impl Match for FormIs {
    fn matches(&self, request: &Request) -> bool {
        form_of(&request.body) == self.0
    }
}

/// The account `example-user-<n>`: screen name `example-user-<n>`, item
/// name `@example-user-<n>`, email `user-<n>@example.com`, not locked.
fn account(n: u32) -> Value {
    json!({
        "canonical_email": format!("user-{n}@example.com"),
        "email": format!("user-{n}@example.com"),
        "has_disability_access": false,
        "itemname": format!("@example-user-{n}"),
        "locked": false,
        "notifications": [],
        "screenname": format!("example-user-{n}"),
        "verified": true,
        "lastlogin": "2026-01-01 00:00:00",
        "createdate": "2020-01-01 00:00:00"
    })
}

/// [`account`] `n` with `email` as its address.
fn account_with_email(n: u32, email: &str) -> Value {
    let mut v = account(n);
    v["email"] = json!(email);
    v["canonical_email"] = json!(email);
    v
}

/// The form field the test inputs here are looked up by.
fn lookup_field(input: &str) -> &'static str {
    if input.starts_with('@') {
        "itemname"
    } else if input.contains('@') {
        "email"
    } else {
        "screenname"
    }
}

/// A lookup mock for exactly one input line.
async fn mount_lookup(server: &MockServer, input: &str, response: ResponseTemplate, expect: u64) {
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .and(FormIs(pairs(&[(lookup_field(input), input)])))
        .respond_with(response)
        .expect(expect)
        .mount(server)
        .await;
}

/// `input` finds `values`, looked up once.
async fn mount_found(server: &MockServer, input: &str, values: Value) {
    mount_lookup(
        server,
        input,
        ResponseTemplate::new(200).set_body_json(ok_body(values)),
        1,
    )
    .await;
}

/// `input` finds no account, looked up once.
async fn mount_not_found(server: &MockServer, input: &str) {
    mount_lookup(
        server,
        input,
        ResponseTemplate::new(200).set_body_json(not_found_body()),
        1,
    )
    .await;
}

/// A lock or unlock mock for exactly one item name and comment.
async fn mount_lock_for(
    server: &MockServer,
    itemname: &str,
    lock: bool,
    comment: Option<&str>,
    response: ResponseTemplate,
    expect: u64,
) {
    let mut fields = vec![
        ("itemname", itemname),
        ("is_lock", if lock { "1" } else { "0" }),
    ];
    if let Some(c) = comment {
        fields.push(("comments", c));
    }
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "lock_unlock"))
        .and(FormIs(pairs(&fields)))
        .respond_with(response)
        .expect(expect)
        .mount(server)
        .await;
}

fn lock_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"success": true}))
}

/// A catch-all with `expect(0)` behind every other mock: a request no
/// other mock matches fails the test.
async fn mount_unexpected(server: &MockServer) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(599))
        .with_priority(u8::MAX)
        .expect(0)
        .mount(server)
        .await;
}

/// A list with a comment, blank lines, padding, an indented comment and a
/// repeated line; it names three accounts.
const LIST3: &str = "# moderation list\n\
                     example-user-1\n\
                     \n\
                     \x20  user-2@example.com   \n\
                     \x20 # indented comment\n\
                     @example-user-3\n\
                     example-user-1\n\
                     \n";

/// The three accounts [`LIST3`] names, with their lookup types.
const INPUTS3: [(&str, &str); 3] = [
    ("example-user-1", "screenname"),
    ("user-2@example.com", "email"),
    ("@example-user-3", "itemname"),
];

/// Lookups for the three lines of [`LIST3`], each expected once.
async fn mount_three_found(server: &MockServer) {
    mount_found(server, "example-user-1", account(1)).await;
    mount_found(server, "user-2@example.com", account(2)).await;
    mount_found(server, "@example-user-3", account(3)).await;
}

fn write_list(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, contents).unwrap();
    p
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// The non-empty lines of `text`, sorted (results print in completion
/// order).
fn sorted_lines(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

fn sorted(list: &[String]) -> Vec<String> {
    let mut v = list.to_vec();
    v.sort();
    v
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l.trim_end() == line)
}

/// Every stdout line parsed as JSON, sorted by `account`.
fn json_records(output: &Output) -> Vec<Value> {
    let stdout = stdout_of(output);
    let mut records: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}\n{stdout}"))
        })
        .collect();
    records.sort_by(|a, b| {
        a["account"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["account"].as_str().unwrap_or_default())
    });
    records
}

/// The joblog's entries, or none when the file does not exist.
fn joblog_entries(path: &Path) -> Vec<Value> {
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// One joblog line, in the format `ia_core::joblog::JoblogEntry` writes.
fn joblog_line(second: u32, op: &str, item: &str, status: &str) -> String {
    let ts = format!("2026-01-01T00:00:{second:02}Z");
    let entry = if status == "ok" {
        json!({"ts": ts, "op": op, "item": item, "file": "", "status": "ok",
               "bytes": 0, "elapsed_ms": 5})
    } else {
        json!({"ts": ts, "op": op, "item": item, "file": "", "status": "error",
               "error": "example failure", "retries": 0})
    };
    format!("{entry}\n")
}

/// The stderr line a resumed run prints.
fn resuming_line(n: usize) -> String {
    format!("resuming: skipping {n} account(s) already done in joblog")
}

/// Asserts the human `--get-screenname` output for [`LIST3`].
fn assert_three_screennames(output: &Output) {
    let stderr = stderr_of(output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(output)),
        sorted(&[
            "example-user-1: example-user-1".to_string(),
            "user-2@example.com: example-user-2".to_string(),
            "@example-user-3: example-user-3".to_string(),
        ]),
    );
    assert!(
        has_line(&stderr, "3 succeeded, 0 failed"),
        "stderr: {stderr}"
    );
}

// ─── Input sources ───────────────────────────────────────────────────────────

/// Blank lines, `#` lines (after trimming) and a repeated line are skipped;
/// each line is trimmed; every remaining line is looked up once (each
/// lookup mock expects one request).
#[tokio::test]
async fn list_from_file_skips_blank_comment_and_repeated_lines() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", LIST3);

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&list), "--get-screenname"],
    );
    assert_three_screennames(&output);
}

#[tokio::test]
async fn itemlist_dash_reads_standard_input() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--itemlist", "-", "--get-screenname"],
        LIST3,
    );
    assert_three_screennames(&output);
}

/// With no `USER` and no `--itemlist`, piped standard input is the list.
#[tokio::test]
async fn piped_stdin_without_user_or_itemlist_is_the_list() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--get-screenname"], LIST3);
    assert_three_screennames(&output);
}

/// `--itemlist /dev/stdin`, the form other commands document, also works.
#[cfg(unix)]
#[tokio::test]
async fn itemlist_dev_stdin_reads_standard_input() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--itemlist", "/dev/stdin", "--get-screenname"],
        LIST3,
    );
    assert_three_screennames(&output);
}

/// `--json`: one object per line, each naming its lookup type; no summary.
#[tokio::test]
async fn list_json_names_each_lines_lookup_type() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", LIST3);

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&list), "--get-itemname", "--json"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let records = json_records(&output);
    assert_eq!(records.len(), 3, "{records:?}");
    for (input, lookup) in INPUTS3 {
        let record = records
            .iter()
            .find(|r| r["account"] == input)
            .unwrap_or_else(|| panic!("no record for {input}: {records:?}"));
        assert_eq!(record["lookup"], lookup, "{record}");
        assert_eq!(record["action"], "get-itemname", "{record}");
        assert_eq!(record["status"], "ok", "{record}");
        assert!(record["elapsed_ms"].is_u64(), "{record}");
    }
    let itemnames: Vec<&str> = records
        .iter()
        .map(|r| r["itemname"].as_str().unwrap_or_default())
        .collect();
    let mut itemnames = itemnames;
    itemnames.sort_unstable();
    assert_eq!(
        itemnames,
        ["@example-user-1", "@example-user-2", "@example-user-3"]
    );
    assert!(
        !stderr.contains("succeeded"),
        "no summary with --json: {stderr}"
    );
}

/// With no action flag, each line prints `<line>: <values as one-line JSON>`.
#[tokio::test]
async fn list_default_lookup_prints_one_line_json_per_account() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", LIST3);

    let output = run_account(cfg.path(), &host, &["--itemlist", path_str(&list)]);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let stdout = stdout_of(&output);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "one line per account: {stdout}");
    for (n, (input, _)) in (1..).zip(INPUTS3) {
        let prefix = format!("{input}: ");
        let line = lines
            .iter()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no line for {input}: {stdout}"));
        let printed: Value = serde_json::from_str(&line[prefix.len()..])
            .unwrap_or_else(|e| panic!("{input}: not one-line JSON ({e}): {line}"));
        assert_eq!(printed, account(n), "{input}");
    }
    assert!(
        has_line(&stderr, "3 succeeded, 0 failed"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn list_is_locked_prints_true_or_false_per_line() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    let mut locked = account(2);
    locked["locked"] = json!(true);
    mount_found(&server, "user-2@example.com", locked).await;
    mount_found(&server, "@example-user-3", account(3)).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--is-locked"], LIST3);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: false".to_string(),
            "user-2@example.com: true".to_string(),
            "@example-user-3: false".to_string(),
        ]),
    );
}

/// Each account in a lock or unlock list is looked up and then gets one
/// request with the comment; each prints `<line>: locked` / `unlocked`.
#[tokio::test]
async fn list_lock_and_unlock_send_one_request_per_account() {
    for (flag, lock, word) in [("--lock", true, "locked"), ("--unlock", false, "unlocked")] {
        let server = MockServer::start().await;
        mount_three_found(&server).await;
        for n in 1..=3 {
            mount_lock_for(
                &server,
                &format!("@example-user-{n}"),
                lock,
                Some("example note"),
                lock_ok(),
                1,
            )
            .await;
        }
        mount_unexpected(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account_stdin(
            cfg.path(),
            &host,
            &[flag, "--comment", "example note"],
            LIST3,
        );
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{flag}: stderr: {stderr}");
        let expected: Vec<String> = INPUTS3
            .iter()
            .map(|(input, _)| format!("{input}: {word}"))
            .collect();
        assert_eq!(
            sorted_lines(&stdout_of(&output)),
            sorted(&expected),
            "{flag}"
        );
        assert!(
            has_line(&stderr, "3 succeeded, 0 failed"),
            "{flag}: stderr: {stderr}"
        );
        assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 3);
    }
}

/// A list of one account prints in list form and has no summary line (the
/// summary follows a list of more than one).
#[tokio::test]
async fn single_line_list_prints_no_summary() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--get-screenname"], "example-user-1\n");
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "example-user-1: example-user-1\n");
    assert!(!stderr.contains("succeeded"), "stderr: {stderr}");
}

// ─── Failures in a list ──────────────────────────────────────────────────────

/// A list where the second account is unknown.
const LIST_WITH_UNKNOWN: &str = "example-user-1\nnobody@example.org\n@example-user-3\n";

/// Lookups for [`LIST_WITH_UNKNOWN`] and lock requests for the two accounts
/// that exist, each expected once.
async fn mount_list_with_unknown(server: &MockServer, comment: Option<&str>) {
    mount_found(server, "example-user-1", account(1)).await;
    mount_not_found(server, "nobody@example.org").await;
    mount_found(server, "@example-user-3", account(3)).await;
    mount_lock_for(server, "@example-user-1", true, comment, lock_ok(), 1).await;
    mount_lock_for(server, "@example-user-3", true, comment, lock_ok(), 1).await;
    mount_unexpected(server).await;
}

/// One unknown account: the others complete, exit 1, the error and the
/// summary on stderr.
#[tokio::test]
async fn one_failing_account_exits_1_and_the_others_complete() {
    let server = MockServer::start().await;
    mount_list_with_unknown(&server, None).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--lock"], LIST_WITH_UNKNOWN);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: locked".to_string(),
            "@example-user-3: locked".to_string(),
        ]),
    );
    assert!(
        stderr.contains("error: nobody@example.org: "),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("account not found"), "stderr: {stderr}");
    assert!(
        has_line(&stderr, "2 succeeded, 1 failed"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.lines().any(|l| l.starts_with("Error:")),
        "exit 1 with no extra Error: line: {stderr}"
    );
}

#[tokio::test]
async fn one_failing_account_json_has_an_error_record_and_no_summary() {
    let server = MockServer::start().await;
    mount_list_with_unknown(&server, None).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--lock", "--json"], LIST_WITH_UNKNOWN);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    let records = json_records(&output);
    assert_eq!(records.len(), 3, "{records:?}");
    for record in &records {
        assert_eq!(record["action"], "lock", "{record}");
        if record["account"] == "nobody@example.org" {
            assert_eq!(record["lookup"], "email", "{record}");
            assert_eq!(record["status"], "error", "{record}");
            assert_eq!(record["error"]["code"], "account_api_error", "{record}");
            assert_eq!(record["error"]["account"], "nobody@example.org", "{record}");
        } else {
            assert_eq!(record["status"], "ok", "{record}");
        }
    }
    assert!(
        !stderr.contains("succeeded"),
        "no summary with --json: {stderr}"
    );
}

// ─── Usage and input errors ──────────────────────────────────────────────────

#[tokio::test]
async fn user_with_itemlist_is_a_usage_error() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", "example-user-2\n");

    let output = run_account(
        cfg.path(),
        &host,
        &["example-user-1", "--itemlist", path_str(&list)],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        !stderr.contains("unexpected argument"),
        "--itemlist must be a known flag: {stderr}"
    );
    assert!(stderr.contains("cannot be used with"), "stderr: {stderr}");
    assert!(stderr.contains("--itemlist"), "stderr: {stderr}");
}

/// An empty or whitespace-only `USER` is its own error, not the
/// no-source error.
#[tokio::test]
async fn empty_user_is_an_error() {
    for user in ["", "   "] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(cfg.path(), &host, &[user, "--lock"]);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "{user:?}: stderr: {stderr}");
        assert!(
            stderr.contains("USER is empty"),
            "{user:?}: stderr: {stderr}"
        );
        assert!(!stderr.contains("no account given"), "{user:?}: {stderr}");
        assert_eq!(stdout_of(&output), "", "{user:?}");

        let output = run_account(cfg.path(), &host, &[user, "--lock", "--json"]);
        assert_eq!(output.status.code(), Some(1), "{user:?}");
        let err = stderr_json_error(&output);
        assert_eq!(err["error"]["code"], "invalid_argument", "{err}");
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("USER is empty"),
            "{err}"
        );
    }
}

/// A byte-order mark at the start of a list is not part of the first line:
/// before a `#` line the line is still a comment, and before an `@` line
/// the line is still an item name.
#[tokio::test]
async fn leading_byte_order_mark_is_stripped() {
    for (list, expected) in [
        (
            "\u{feff}# moderation list\n@example-user-3\n",
            vec!["@example-user-3: example-user-3".to_string()],
        ),
        (
            "\u{feff}@example-user-3\nexample-user-1\n",
            vec![
                "@example-user-3: example-user-3".to_string(),
                "example-user-1: example-user-1".to_string(),
            ],
        ),
    ] {
        for from_file in [true, false] {
            let server = MockServer::start().await;
            mount_found(&server, "@example-user-3", account(3)).await;
            if expected.len() == 2 {
                mount_found(&server, "example-user-1", account(1)).await;
            }
            mount_unexpected(&server).await;
            let host = host_of(&server);
            let cfg = config_with_cookies(&host);
            let dir = TempDir::new().unwrap();
            let path = write_list(&dir, "accounts.txt", list);

            let output = if from_file {
                run_account(
                    cfg.path(),
                    &host,
                    &["--itemlist", path_str(&path), "--get-screenname"],
                )
            } else {
                run_account_stdin(cfg.path(), &host, &["--get-screenname"], list)
            };
            let stderr = stderr_of(&output);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{list:?} file={from_file}: stderr: {stderr}"
            );
            assert_eq!(
                sorted_lines(&stdout_of(&output)),
                sorted(&expected),
                "{list:?} file={from_file}"
            );
        }
    }
}

#[tokio::test]
async fn missing_itemlist_file_exits_1_with_no_request() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.txt");

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&missing), "--lock"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        !stderr.contains("unexpected argument"),
        "--itemlist must be a known flag: {stderr}"
    );
    assert!(stderr.contains(path_str(&missing)), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "");

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&missing), "--lock", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_of(&output)
    );
    let err = stderr_json_error(&output);
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(path_str(&missing)),
        "{err}"
    );
    assert_eq!(stdout_of(&output), "");
}

/// A list with only blank and comment lines names no account.
const EMPTY_LIST: &str = "# nothing to do\n\n   \n  # still nothing\n";

#[tokio::test]
async fn empty_list_file_exits_1_with_no_request() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "empty.txt", EMPTY_LIST);
    let message = format!("no accounts found in {}", path_str(&list));

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&list), "--lock"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains(&message), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "");

    let output = run_account(
        cfg.path(),
        &host,
        &["--itemlist", path_str(&list), "--lock", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_of(&output)
    );
    let err = stderr_json_error(&output);
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&message),
        "{err}"
    );
}

/// Piped standard input with no account in it, through `--itemlist -`, with
/// no flag, and as an empty pipe: `no accounts found in …`, not the
/// no-source error.
#[tokio::test]
async fn empty_standard_input_exits_1_with_no_request() {
    for (args, stdin) in [
        (vec!["--itemlist", "-", "--lock"], EMPTY_LIST),
        (vec!["--lock"], EMPTY_LIST),
        (vec!["--lock"], ""),
    ] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account_stdin(cfg.path(), &host, &args, stdin);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains("no accounts found in "),
            "{args:?} {stdin:?}: stderr: {stderr}"
        );
        assert_eq!(stdout_of(&output), "", "{args:?}");
    }
}

/// With a terminal on standard input, the no-source error and the
/// `--itemlist -` error. A pseudo-terminal from `openpty` stands in for
/// the terminal.
#[cfg(unix)]
mod terminal_stdin {
    use super::*;
    use std::os::fd::FromRawFd;
    use std::process::{Command, Stdio};

    const NO_ACCOUNT_GIVEN: &str =
        "no account given: pass USER, --itemlist <FILE>, or pipe accounts on standard input";

    /// A pseudo-terminal: (controlling side, terminal side).
    fn pty() -> (fs::File, fs::File) {
        let mut controller: libc::c_int = -1;
        let mut terminal: libc::c_int = -1;
        // SAFETY: openpty writes two newly opened descriptors into the two
        // out-parameters, which point at live stack variables; null name,
        // termios and winsize pointers are permitted and select defaults.
        let rc = unsafe {
            libc::openpty(
                &mut controller,
                &mut terminal,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: both descriptors were just opened by openpty and nothing
        // else owns them, so each File is their only owner.
        unsafe {
            (
                fs::File::from_raw_fd(controller),
                fs::File::from_raw_fd(terminal),
            )
        }
    }

    /// `ia … account <args>` with the terminal side of a pseudo-terminal
    /// as standard input.
    fn run_with_terminal_stdin(cfg: &Path, host: &str, args: &[&str]) -> Output {
        let (controller, terminal) = pty();
        let output = Command::new(env!("CARGO_BIN_EXE_ia-cli"))
            .env_remove("IA_ACCESS_KEY_ID")
            .env_remove("IA_SECRET_ACCESS_KEY")
            .env_remove("IA_CONFIG_FILE")
            .arg("--config-file")
            .arg(cfg)
            .args(["--insecure", "-H", host, "account"])
            .args(args)
            .stdin(Stdio::from(terminal))
            .output()
            .unwrap();
        drop(controller);
        output
    }

    #[tokio::test]
    async fn no_user_and_terminal_stdin_names_every_source() {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_with_terminal_stdin(cfg.path(), &host, &["--lock"]);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
        assert!(stderr.contains(NO_ACCOUNT_GIVEN), "stderr: {stderr}");

        let output = run_with_terminal_stdin(cfg.path(), &host, &["--lock", "--json"]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "stderr: {}",
            stderr_of(&output)
        );
        // The JSON message is the error's display, which for an
        // InvalidArgument starts with "invalid argument: ".
        let err = stderr_json_error(&output);
        assert_eq!(err["error"]["code"], "invalid_argument", "{err}");
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(NO_ACCOUNT_GIVEN),
            "{err}"
        );
    }

    #[tokio::test]
    async fn itemlist_dash_with_terminal_stdin_is_an_error() {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_with_terminal_stdin(cfg.path(), &host, &["--itemlist", "-", "--lock"]);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
        assert!(
            stderr.contains("--itemlist - needs standard input from a pipe or file"),
            "stderr: {stderr}"
        );
    }
}

// ─── --jobs ──────────────────────────────────────────────────────────────────

/// Every account is processed with one job, the default, and four.
#[tokio::test]
async fn every_account_is_processed_at_each_jobs_setting() {
    let list: String = (1..=5).map(|n| format!("example-user-{n}\n")).collect();
    for jobs in [vec![], vec!["-j", "1"], vec!["--jobs", "4"]] {
        let server = MockServer::start().await;
        for n in 1..=5 {
            mount_found(&server, &format!("example-user-{n}"), account(n)).await;
            mount_lock_for(
                &server,
                &format!("@example-user-{n}"),
                true,
                None,
                lock_ok(),
                1,
            )
            .await;
        }
        mount_unexpected(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let mut args = jobs.clone();
        args.push("--lock");
        let output = run_account_stdin(cfg.path(), &host, &args, &list);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{jobs:?}: stderr: {stderr}");
        let expected: Vec<String> = (1..=5)
            .map(|n| format!("example-user-{n}: locked"))
            .collect();
        assert_eq!(
            sorted_lines(&stdout_of(&output)),
            sorted(&expected),
            "{jobs:?}"
        );
        assert!(
            has_line(&stderr, "5 succeeded, 0 failed"),
            "{jobs:?}: stderr: {stderr}"
        );
    }
}

// ─── Several lines for one account ───────────────────────────────────────────

/// Four spellings of account 1: screen name, item name, email address in
/// other letter case, item name in capitals. The server answers each with
/// account 1, whose item name is `@example-user-1`.
const SPELLINGS: [&str; 4] = [
    "example-user-1",
    "@example-user-1",
    "User-1@Example.com",
    "@EXAMPLE-USER-1",
];

async fn mount_spellings(server: &MockServer) {
    for spelling in SPELLINGS {
        mount_found(server, spelling, account(1)).await;
    }
}

fn spellings_list() -> String {
    SPELLINGS.iter().map(|s| format!("{s}\n")).collect()
}

/// With one job the first line sends the lock; the others report it as
/// theirs with `same account as example-user-1`. One lock request, one
/// comment; each line gets the first line's status in the joblog.
#[tokio::test]
async fn one_account_named_four_ways_is_locked_once() {
    let server = MockServer::start().await;
    mount_spellings(&server).await;
    mount_lock_for(
        &server,
        "@example-user-1",
        true,
        Some("example note"),
        lock_ok(),
        1,
    )
    .await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("account.jsonl");

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &[
            "-j",
            "1",
            "--lock",
            "--comment",
            "example note",
            "--joblog",
            path_str(&log),
        ],
        &spellings_list(),
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: locked".to_string(),
            "@example-user-1: locked (same account as example-user-1)".to_string(),
            "User-1@Example.com: locked (same account as example-user-1)".to_string(),
            "@EXAMPLE-USER-1: locked (same account as example-user-1)".to_string(),
        ]),
    );
    assert!(
        has_line(&stderr, "4 succeeded, 0 failed"),
        "stderr: {stderr}"
    );
    assert_eq!(requests_with_op(&server, "info").await.len(), 4);
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 1);

    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 4, "{entries:?}");
    let mut items: Vec<String> = entries
        .iter()
        .map(|e| {
            assert_eq!(e["op"], "account-lock", "{e}");
            assert_eq!(e["status"], "ok", "{e}");
            e["item"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    items.sort();
    let mut expected: Vec<String> = SPELLINGS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(items, expected);
}

/// With the default jobs the first line to reach the lock is not fixed:
/// exactly one record has no `same_as`, and the others name it.
#[tokio::test]
async fn one_account_named_four_ways_json_names_the_first_line() {
    let server = MockServer::start().await;
    mount_spellings(&server).await;
    mount_lock_for(&server, "@example-user-1", true, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--lock", "--json"], &spellings_list());
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let records = json_records(&output);
    assert_eq!(records.len(), 4, "{records:?}");
    for record in &records {
        assert_eq!(record["status"], "ok", "{record}");
        assert_eq!(record["action"], "lock", "{record}");
        assert_eq!(record["itemname"], "@example-user-1", "{record}");
    }
    let firsts: Vec<&Value> = records
        .iter()
        .filter(|r| r.get("same_as").is_none())
        .collect();
    assert_eq!(
        firsts.len(),
        1,
        "exactly one line sends the lock: {records:?}"
    );
    let first = firsts[0]["account"].as_str().unwrap_or_default();
    assert!(SPELLINGS.contains(&first), "{first}");
    for record in records.iter().filter(|r| r.get("same_as").is_some()) {
        assert_eq!(record["same_as"], first, "{record}");
    }
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 1);
}

/// When the one lock request fails, every line for that account fails, and
/// the joblog records each as an error.
#[tokio::test]
async fn one_account_named_twice_shares_a_failed_lock() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_found(&server, "@example-user-1", account(1)).await;
    mount_lock_for(
        &server,
        "@example-user-1",
        true,
        None,
        ResponseTemplate::new(500).set_body_string("server error"),
        1,
    )
    .await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("account.jsonl");

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["-j", "1", "--lock", "--joblog", path_str(&log)],
        "example-user-1\n@example-user-1\n",
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("error: example-user-1: "),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("error: @example-user-1: "),
        "stderr: {stderr}"
    );
    assert!(
        has_line(&stderr, "0 succeeded, 2 failed"),
        "stderr: {stderr}"
    );
    assert_eq!(stdout_of(&output), "");

    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 2, "{entries:?}");
    for e in &entries {
        assert_eq!(e["op"], "account-lock", "{e}");
        assert_eq!(e["status"], "error", "{e}");
    }
}

/// Read actions are not de-duplicated: two spellings, two lookups, two
/// plain result lines.
#[tokio::test]
async fn read_actions_look_up_every_spelling() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_found(&server, "@example-user-1", account(1)).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--get-email"],
        "example-user-1\n@example-user-1\n",
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: user-1@example.com".to_string(),
            "@example-user-1: user-1@example.com".to_string(),
        ]),
    );
    assert_eq!(requests_with_op(&server, "info").await.len(), 2);

    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_found(&server, "@example-user-1", account(1)).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--get-email", "--json"],
        "example-user-1\n@example-user-1\n",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let records = json_records(&output);
    assert_eq!(records.len(), 2, "{records:?}");
    for record in &records {
        assert!(record.get("same_as").is_none(), "{record}");
        assert_eq!(record["canonical_email"], "user-1@example.com", "{record}");
    }
}

/// With four jobs and a slow lock answer, all four spellings reach the lock
/// step while the first request is in flight: still one request, every
/// line ok, three naming the line that sent it.
#[tokio::test]
async fn one_account_named_four_ways_concurrently_is_locked_once() {
    let server = MockServer::start().await;
    mount_spellings(&server).await;
    mount_lock_for(
        &server,
        "@example-user-1",
        true,
        None,
        lock_ok().set_delay(Duration::from_millis(500)),
        1,
    )
    .await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["-j", "4", "--lock", "--json"],
        &spellings_list(),
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let records = json_records(&output);
    assert_eq!(records.len(), 4, "{records:?}");
    for record in &records {
        assert_eq!(record["status"], "ok", "{record}");
    }
    let firsts: Vec<&Value> = records
        .iter()
        .filter(|r| r.get("same_as").is_none())
        .collect();
    assert_eq!(firsts.len(), 1, "{records:?}");
    let first = firsts[0]["account"].as_str().unwrap_or_default();
    let followers: Vec<&Value> = records
        .iter()
        .filter(|r| r.get("same_as").is_some())
        .collect();
    assert_eq!(followers.len(), 3, "{records:?}");
    for record in followers {
        assert_eq!(record["same_as"], first, "{record}");
    }
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 1);
}

/// Item names that differ only in letter case are two accounts: each gets
/// its own lock request, and neither line is reported as the other.
#[tokio::test]
async fn item_names_differing_in_case_are_locked_separately() {
    let server = MockServer::start().await;
    let mut first = account(1);
    first["itemname"] = json!("@Example-X");
    let mut second = account(2);
    second["itemname"] = json!("@example-x");
    mount_found(&server, "example-user-1", first).await;
    mount_found(&server, "example-user-2", second).await;
    mount_lock_for(&server, "@Example-X", true, None, lock_ok(), 1).await;
    mount_lock_for(&server, "@example-x", true, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["-j", "1", "--lock"],
        "example-user-1\nexample-user-2\n",
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: locked".to_string(),
            "example-user-2: locked".to_string(),
        ]),
    );
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 2);
}

// ─── --dry-run ───────────────────────────────────────────────────────────────

/// With valid cookies and a loopback host, where a real run would send
/// requests, a dry run still sends none.
#[tokio::test]
async fn dry_run_with_valid_cookies_sends_nothing() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    for extra in [vec![], vec!["--json"], vec!["-j", "4"]] {
        let mut args = vec!["--dry-run", "--lock", "--comment", "example note"];
        args.extend(extra.iter().copied());
        let output = run_account_stdin(cfg.path(), &host, &args, LIST3);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        assert_eq!(
            sorted_lines(&stdout_of(&output)).len(),
            3,
            "{args:?}: one line per account"
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a dry run sent a request"
    );
}

/// Sends nothing, needs no cookies, and prints one line per account and a
/// footer on stderr; a non-empty comment is shown.
#[tokio::test]
async fn dry_run_sends_nothing_and_prints_each_line() {
    for (extra, suffix) in [
        (
            vec!["--comment", "example note"],
            ", comment \"example note\"",
        ),
        (vec![], ""),
        (vec!["--comment", ""], ""),
        // Quotes and a newline are escaped, so the line stays one line.
        (
            vec!["--comment", "say \"hi\"\nnext"],
            ", comment \"say \\\"hi\\\"\\nnext\"",
        ),
    ] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_without_cookies(&host);

        let mut args = vec!["--dry-run", "--lock"];
        args.extend(extra.iter().copied());
        let output = run_account_stdin(cfg.path(), &host, &args, LIST3);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        let expected: Vec<String> = INPUTS3
            .iter()
            .map(|(input, lookup)| format!("dry-run {input}: lock (lookup by {lookup}){suffix}"))
            .collect();
        assert_eq!(
            sorted_lines(&stdout_of(&output)),
            sorted(&expected),
            "{args:?}"
        );
        assert!(
            has_line(&stderr, "dry-run: would lock 3 account(s), nothing sent"),
            "{args:?}: stderr: {stderr}"
        );
    }
}

/// Read actions can be dry-run too, to see how each line will be looked up.
#[tokio::test]
async fn dry_run_read_actions_name_the_action() {
    for (flags, action) in [(vec!["--get-email"], "get-email"), (vec![], "lookup")] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_without_cookies(&host);

        let mut args = vec!["--dry-run"];
        args.extend(flags.iter().copied());
        let output = run_account_stdin(cfg.path(), &host, &args, LIST3);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        let expected: Vec<String> = INPUTS3
            .iter()
            .map(|(input, lookup)| format!("dry-run {input}: {action} (lookup by {lookup})"))
            .collect();
        assert_eq!(
            sorted_lines(&stdout_of(&output)),
            sorted(&expected),
            "{args:?}"
        );
        assert!(
            has_line(
                &stderr,
                &format!("dry-run: would {action} 3 account(s), nothing sent")
            ),
            "{args:?}: stderr: {stderr}"
        );
    }
}

/// A positional `USER` dry run; the footer is dropped at `-qq`.
#[tokio::test]
async fn dry_run_positional_user_and_qq_footer() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_without_cookies(&host);

    let output = run_account(
        cfg.path(),
        &host,
        &["example-user-1", "--unlock", "--dry-run"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stdout_of(&output),
        "dry-run example-user-1: unlock (lookup by screenname)\n"
    );
    assert!(
        has_line(&stderr, "dry-run: would unlock 1 account(s), nothing sent"),
        "stderr: {stderr}"
    );

    let output = run_account_stdin(cfg.path(), &host, &["-qq", "--dry-run", "--lock"], LIST3);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        !stderr.contains("would lock"),
        "-qq drops the footer: {stderr}"
    );
}

/// The cookie checks are skipped: no cookies and a host the cookies may not
/// go to are both fine for a dry run. `0.0.0.0` reaches only this machine,
/// so a request sent by mistake lands on the mock.
#[tokio::test]
async fn dry_run_skips_the_cookie_host_check() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let other_host = format!("0.0.0.0:{}", server.address().port());

    let output = run_account(
        cfg.path(),
        &other_host,
        &["example-user-1", "--lock", "--dry-run"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stdout_of(&output),
        "dry-run example-user-1: lock (lookup by screenname)\n"
    );
}

#[tokio::test]
async fn dry_run_json_records() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_without_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--dry-run", "--lock", "--comment", "example note", "--json"],
        LIST3,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let records = json_records(&output);
    let mut expected: Vec<Value> = INPUTS3
        .iter()
        .map(|(input, lookup)| {
            json!({"account": input, "lookup": lookup, "action": "lock",
                   "status": "dry_run", "comment": "example note", "dry_run": true})
        })
        .collect();
    expected.sort_by(|a, b| {
        a["account"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["account"].as_str().unwrap_or_default())
    });
    assert_eq!(records, expected);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--dry-run", "--is-locked", "--json"],
        LIST3,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    for record in json_records(&output) {
        assert_eq!(record["action"], "is-locked", "{record}");
        assert_eq!(record["status"], "dry_run", "{record}");
        assert_eq!(record["dry_run"], true, "{record}");
        assert!(record.get("comment").is_none(), "{record}");
    }
}

/// A dry run applies the resume skip set first and writes no joblog entry.
#[tokio::test]
async fn dry_run_applies_resume_and_writes_no_joblog() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_without_cookies(&host);
    let dir = TempDir::new().unwrap();

    // A fresh joblog stays empty.
    let fresh = dir.path().join("fresh.jsonl");
    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--dry-run", "--lock", "--joblog", path_str(&fresh)],
        LIST3,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        joblog_entries(&fresh).is_empty(),
        "{:?}",
        joblog_entries(&fresh)
    );

    // One line already locked: the dry run shows the other two.
    let seeded = dir.path().join("seeded.jsonl");
    let before = joblog_line(0, "account-lock", "example-user-1", "ok");
    fs::write(&seeded, &before).unwrap();
    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--dry-run", "--lock", "--joblog", path_str(&seeded)],
        LIST3,
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "dry-run user-2@example.com: lock (lookup by email)".to_string(),
            "dry-run @example-user-3: lock (lookup by itemname)".to_string(),
        ]),
    );
    assert!(has_line(&stderr, &resuming_line(1)), "stderr: {stderr}");
    assert!(
        has_line(&stderr, "dry-run: would lock 2 account(s), nothing sent"),
        "stderr: {stderr}"
    );
    assert_eq!(fs::read_to_string(&seeded).unwrap(), before);
}

// ─── --joblog ────────────────────────────────────────────────────────────────

/// A lock list writes one entry per line; a rerun skips the `ok` lines and
/// retries the failed one; a third run finds everything done, sends
/// nothing and exits 0.
#[tokio::test]
async fn joblog_records_each_line_and_a_rerun_retries_the_failed_one() {
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", LIST_WITH_UNKNOWN);
    let log = dir.path().join("account.jsonl");
    let args = [
        "--itemlist",
        path_str(&list),
        "--lock",
        "--comment",
        "example note",
        "--joblog",
        path_str(&log),
    ];

    // First run: two ok, one error.
    let server = MockServer::start().await;
    mount_list_with_unknown(&server, Some("example note")).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &args);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_of(&output)
    );
    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 3, "{entries:?}");
    for e in &entries {
        assert_eq!(e["op"], "account-lock", "{e}");
        assert_eq!(e["file"], "", "{e}");
        let item = e["item"].as_str().unwrap_or_default();
        if item == "nobody@example.org" {
            assert_eq!(e["status"], "error", "{e}");
            assert!(
                e["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("account not found"),
                "{e}"
            );
        } else {
            assert!(["example-user-1", "@example-user-3"].contains(&item), "{e}");
            assert_eq!(e["status"], "ok", "{e}");
            assert_eq!(e["bytes"], 0, "{e}");
            assert!(e["elapsed_ms"].is_u64(), "{e}");
        }
    }

    // Second run: only the failed line, which now exists.
    let server = MockServer::start().await;
    mount_found(
        &server,
        "nobody@example.org",
        account_with_email(9, "nobody@example.org"),
    )
    .await;
    mount_lock_for(
        &server,
        "@example-user-9",
        true,
        Some("example note"),
        lock_ok(),
        1,
    )
    .await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &args);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(has_line(&stderr, &resuming_line(2)), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "nobody@example.org: locked\n");
    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 4, "{entries:?}");
    assert_eq!(entries[3]["item"], "nobody@example.org");
    assert_eq!(entries[3]["op"], "account-lock");
    assert_eq!(entries[3]["status"], "ok");
    drop(server);

    // Third run: everything done.
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &args);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(has_line(&stderr, &resuming_line(3)), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "");
    assert_eq!(joblog_entries(&log).len(), 4);

    // At -q the resume line is dropped; the exit status is the same.
    let mut quiet_args = vec!["-q"];
    quiet_args.extend(args);
    let output = run_account(cfg.path(), &host, &quiet_args);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("resuming:"), "stderr: {stderr}");
}

/// A positional `USER` lock writes its entry too.
#[tokio::test]
async fn joblog_records_a_positional_lock() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_lock_for(&server, "@example-user-1", true, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("account.jsonl");

    let output = run_account(
        cfg.path(),
        &host,
        &["example-user-1", "--lock", "--joblog", path_str(&log)],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["op"], "account-lock");
    assert_eq!(entries[0]["item"], "example-user-1");
    assert_eq!(entries[0]["file"], "");
    assert_eq!(entries[0]["status"], "ok");
}

/// An earlier lock entry does not make an unlock run skip the line.
#[tokio::test]
async fn joblog_unlock_run_is_not_skipped_by_a_lock_entry() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_lock_for(&server, "@example-user-1", false, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(&dir, "accounts.txt", "example-user-1\n");
    let log = dir.path().join("account.jsonl");
    fs::write(&log, joblog_line(0, "account-lock", "example-user-1", "ok")).unwrap();

    let output = run_account(
        cfg.path(),
        &host,
        &[
            "--itemlist",
            path_str(&list),
            "--unlock",
            "--joblog",
            path_str(&log),
        ],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("resuming:"), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "example-user-1: unlocked\n");
    let entries = joblog_entries(&log);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1]["op"], "account-unlock");
    assert_eq!(entries[1]["item"], "example-user-1");
    assert_eq!(entries[1]["status"], "ok");
}

/// A line is skipped only when its latest entry among `account-lock` and
/// `account-unlock` is `ok` with this run's op: locked then unlocked runs
/// again; unlocked then locked is skipped; locked then failed runs again.
#[tokio::test]
async fn joblog_lock_rerun_follows_the_latest_entry() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_lock_for(&server, "@example-user-1", true, None, lock_ok(), 1).await;
    mount_found(&server, "@example-user-3", account(3)).await;
    mount_lock_for(&server, "@example-user-3", true, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let list = write_list(
        &dir,
        "accounts.txt",
        "example-user-1\nuser-2@example.com\n@example-user-3\n",
    );
    let log = dir.path().join("account.jsonl");
    let seeded = [
        joblog_line(0, "account-lock", "example-user-1", "ok"),
        joblog_line(1, "account-unlock", "example-user-1", "ok"),
        joblog_line(2, "account-unlock", "user-2@example.com", "ok"),
        joblog_line(3, "account-lock", "user-2@example.com", "ok"),
        joblog_line(4, "account-lock", "@example-user-3", "ok"),
        joblog_line(5, "account-lock", "@example-user-3", "error"),
    ]
    .concat();
    fs::write(&log, &seeded).unwrap();

    let output = run_account(
        cfg.path(),
        &host,
        &[
            "--itemlist",
            path_str(&list),
            "--lock",
            "--joblog",
            path_str(&log),
        ],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(has_line(&stderr, &resuming_line(1)), "stderr: {stderr}");
    assert_eq!(
        sorted_lines(&stdout_of(&output)),
        sorted(&[
            "example-user-1: locked".to_string(),
            "@example-user-3: locked".to_string(),
        ]),
    );
}

#[tokio::test]
async fn joblog_no_resume_runs_every_line() {
    let server = MockServer::start().await;
    mount_three_found(&server).await;
    for n in 1..=3 {
        mount_lock_for(
            &server,
            &format!("@example-user-{n}"),
            true,
            None,
            lock_ok(),
            1,
        )
        .await;
    }
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("account.jsonl");
    let seeded: String = INPUTS3
        .iter()
        .zip(0..)
        .map(|((input, _), s)| joblog_line(s, "account-lock", input, "ok"))
        .collect();
    fs::write(&log, &seeded).unwrap();

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--lock", "--joblog", path_str(&log), "--no-resume"],
        LIST3,
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("resuming:"), "stderr: {stderr}");
    assert_eq!(sorted_lines(&stdout_of(&output)).len(), 3);
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 3);
}

/// A read action writes no entries and is not skipped by lock entries.
#[tokio::test]
async fn joblog_read_action_writes_nothing_and_skips_nothing() {
    let dir = TempDir::new().unwrap();

    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let fresh = dir.path().join("fresh.jsonl");
    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--get-email", "--joblog", path_str(&fresh)],
        LIST3,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        joblog_entries(&fresh).is_empty(),
        "{:?}",
        joblog_entries(&fresh)
    );

    let server = MockServer::start().await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let seeded_path = dir.path().join("seeded.jsonl");
    let seeded: String = INPUTS3
        .iter()
        .zip(0..)
        .map(|((input, _), s)| joblog_line(s, "account-lock", input, "ok"))
        .collect();
    fs::write(&seeded_path, &seeded).unwrap();
    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["--get-email", "--joblog", path_str(&seeded_path)],
        LIST3,
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("resuming:"), "stderr: {stderr}");
    assert_eq!(sorted_lines(&stdout_of(&output)).len(), 3);
    assert_eq!(fs::read_to_string(&seeded_path).unwrap(), seeded);
}

// ─── Quiet levels ────────────────────────────────────────────────────────────

/// `-q` drops the lock lines and keeps errors and the summary; `-qq` drops
/// errors and the summary; the exit status is 1 at both.
#[tokio::test]
async fn quiet_levels_in_a_lock_list() {
    for (quiet, loud) in [("-q", true), ("-qq", false)] {
        let server = MockServer::start().await;
        mount_list_with_unknown(&server, None).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account_stdin(cfg.path(), &host, &[quiet, "--lock"], LIST_WITH_UNKNOWN);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "{quiet}: stderr: {stderr}");
        assert_eq!(stdout_of(&output), "", "{quiet}: lock lines dropped");
        assert_eq!(
            stderr.contains("error: nobody@example.org: "),
            loud,
            "{quiet}: stderr: {stderr}"
        );
        assert_eq!(
            has_line(&stderr, "2 succeeded, 1 failed"),
            loud,
            "{quiet}: stderr: {stderr}"
        );
        if !loud {
            assert!(!stderr.contains("succeeded"), "{quiet}: stderr: {stderr}");
        }
    }
}

/// Read values print at every quiet level; the summary is kept at `-q`.
#[tokio::test]
async fn quiet_levels_keep_read_values_in_a_list() {
    for (quiet, summary) in [("-q", true), ("-qq", false)] {
        let server = MockServer::start().await;
        mount_three_found(&server).await;
        mount_unexpected(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account_stdin(cfg.path(), &host, &[quiet, "--get-screenname"], LIST3);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(0), "{quiet}: stderr: {stderr}");
        assert_eq!(
            sorted_lines(&stdout_of(&output)),
            sorted(&[
                "example-user-1: example-user-1".to_string(),
                "user-2@example.com: example-user-2".to_string(),
                "@example-user-3: example-user-3".to_string(),
            ]),
            "{quiet}"
        );
        assert_eq!(
            has_line(&stderr, "3 succeeded, 0 failed"),
            summary,
            "{quiet}: stderr: {stderr}"
        );
    }
}

/// Duplicate lines' result lines are lock lines too: dropped at `-q`.
#[tokio::test]
async fn quiet_drops_same_account_lines() {
    let server = MockServer::start().await;
    mount_found(&server, "example-user-1", account(1)).await;
    mount_found(&server, "@example-user-1", account(1)).await;
    mount_lock_for(&server, "@example-user-1", true, None, lock_ok(), 1).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(
        cfg.path(),
        &host,
        &["-q", "--lock"],
        "example-user-1\n@example-user-1\n",
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "");
    assert!(
        has_line(&stderr, "2 succeeded, 0 failed"),
        "stderr: {stderr}"
    );
}

// ─── Rate limiting ───────────────────────────────────────────────────────────

/// A 429 mock at the highest priority that answers the first request with
/// `op=<op>`, whichever account it is for; the retry falls through to that
/// account's own mock.
async fn mount_one_429(server: &MockServer, op: &str) {
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", op))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn list_lookup_429_pauses_once_then_every_account_succeeds() {
    let server = MockServer::start().await;
    mount_one_429(&server, "info").await;
    mount_three_found(&server).await;
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--get-screenname"], LIST3);
    assert_three_screennames(&output);
    let stderr = stderr_of(&output);
    assert_eq!(stderr.matches(PAUSE_MESSAGE).count(), 1, "stderr: {stderr}");
    assert_eq!(requests_with_op(&server, "info").await.len(), 4);
}

#[tokio::test]
async fn list_lock_429_pauses_once_then_every_account_succeeds() {
    let server = MockServer::start().await;
    mount_one_429(&server, "lock_unlock").await;
    mount_three_found(&server).await;
    for n in 1..=3 {
        mount_lock_for(
            &server,
            &format!("@example-user-{n}"),
            true,
            None,
            lock_ok(),
            1,
        )
        .await;
    }
    mount_unexpected(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account_stdin(cfg.path(), &host, &["--lock"], LIST3);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr.matches(PAUSE_MESSAGE).count(), 1, "stderr: {stderr}");
    assert_eq!(sorted_lines(&stdout_of(&output)).len(), 3);
    assert!(
        has_line(&stderr, "3 succeeded, 0 failed"),
        "stderr: {stderr}"
    );
    assert_eq!(requests_with_op(&server, "info").await.len(), 3);
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 4);
}
