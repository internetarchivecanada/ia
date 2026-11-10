//! `ia account` for one account given as the positional `USER`: lookup,
//! the read actions, lock and unlock.
//!
//! All tests are hermetic. Each writes a config whose host is the wiremock
//! server, with test cookies, passes it with `--config-file`, passes
//! `--insecure -H <mock>` as well, and removes the `IA_*` environment
//! variables, so any request can only reach the mock, where `expect`
//! counts catch one the test did not intend.

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::{json, Value};
use tempfile::NamedTempFile;
use wiremock::matchers::{any, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const COOKIES: &str = "[cookies]\nlogged-in-user = test%40example.com\nlogged-in-sig = test-sig\n";

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

/// `ia --config-file <cfg> --insecure -H <host> account <args>`.
fn run_account(cfg: &Path, host: &str, args: &[&str]) -> Output {
    bare_cmd()
        .arg("--config-file")
        .arg(cfg)
        .args(["--insecure", "-H", host, "account"])
        .args(args)
        .output()
        .unwrap()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn values() -> Value {
    json!({
        "canonical_email": "canonical@example.com",
        "email": "person@example.com",
        "has_disability_access": false,
        "itemname": "@example-user-1",
        "locked": false,
        "notifications": [],
        "screenname": "example-user-1",
        "verified": true,
        "lastlogin": "2026-01-01 00:00:00",
        "createdate": "2020-01-01 00:00:00"
    })
}

fn ok_body(values: Value) -> Value {
    json!({"success": true, "values": values})
}

async fn mount_info(server: &MockServer, response: ResponseTemplate, expect: u64) {
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(response)
        .expect(expect)
        .mount(server)
        .await;
}

async fn mount_info_ok(server: &MockServer) {
    mount_info(
        server,
        ResponseTemplate::new(200).set_body_json(ok_body(values())),
        1,
    )
    .await;
}

async fn mount_lock(server: &MockServer, response: ResponseTemplate, expect: u64) {
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "lock_unlock"))
        .respond_with(response)
        .expect(expect)
        .mount(server)
        .await;
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

fn decode(s: &str) -> String {
    urlencoding::decode(&s.replace('+', " "))
        .unwrap()
        .into_owned()
}

/// The form fields of a request body, decoded, sorted by name.
fn form_fields(req: &Request) -> Vec<(String, String)> {
    let body = std::str::from_utf8(&req.body).unwrap();
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

/// The only line of stdout, parsed as JSON.
fn single_json_line(output: &Output) -> Value {
    let stdout = stdout_of(output);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one JSON line; stdout: {stdout}");
    serde_json::from_str(lines[0]).unwrap()
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

fn assert_ok_record(record: &Value, account: &str, lookup: &str, action: &str) {
    assert_eq!(record["account"], account, "{record}");
    assert_eq!(record["lookup"], lookup, "{record}");
    assert_eq!(record["action"], action, "{record}");
    assert_eq!(record["status"], "ok", "{record}");
    assert!(record["elapsed_ms"].is_u64(), "elapsed_ms: {record}");
}

// -- read actions --

/// With no action flag, the whole `values` object prints as indented JSON.
#[tokio::test]
async fn lookup_prints_values_as_indented_json() {
    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(cfg.path(), &host, &["example-user-1"]);
    let stdout = stdout_of(&output);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let printed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(printed, values());
    assert!(
        stdout.contains("\n  \""),
        "values should be indented: {stdout}"
    );

    let info = requests_with_op(&server, "info").await;
    assert_eq!(
        form_fields(&info[0]),
        pairs(&[("screenname", "example-user-1")])
    );
}

#[tokio::test]
async fn lookup_json_names_the_lookup_type() {
    for (user, lookup) in [
        ("person@example.com", "email"),
        ("example-user-1", "screenname"),
        ("@example-user-1", "itemname"),
    ] {
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(cfg.path(), &host, &[user, "--json"]);
        assert!(output.status.success(), "stderr: {}", stderr_of(&output));
        let record = single_json_line(&output);
        assert_ok_record(&record, user, lookup, "lookup");
        assert_eq!(record["values"], values(), "{user}");

        let info = requests_with_op(&server, "info").await;
        assert_eq!(form_fields(&info[0]), pairs(&[(lookup, user)]), "{user}");
    }
}

/// Each read flag prints its value; `--get-email` prints `canonical_email`.
#[tokio::test]
async fn read_actions_print_their_value() {
    for (flag, action, human, key, json_value) in [
        (
            "--get-email",
            "get-email",
            "canonical@example.com",
            "canonical_email",
            json!("canonical@example.com"),
        ),
        (
            "--get-screenname",
            "get-screenname",
            "example-user-1",
            "screenname",
            json!("example-user-1"),
        ),
        (
            "-s",
            "get-screenname",
            "example-user-1",
            "screenname",
            json!("example-user-1"),
        ),
        (
            "-g",
            "get-email",
            "canonical@example.com",
            "canonical_email",
            json!("canonical@example.com"),
        ),
        (
            "--get-itemname",
            "get-itemname",
            "@example-user-1",
            "itemname",
            json!("@example-user-1"),
        ),
        ("--is-locked", "is-locked", "false", "locked", json!(false)),
    ] {
        // Human output.
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);
        let output = run_account(cfg.path(), &host, &["example-user-1", flag]);
        assert!(
            output.status.success(),
            "{flag}: stderr: {}",
            stderr_of(&output)
        );
        assert_eq!(stdout_of(&output), format!("{human}\n"), "{flag}");

        // JSON output.
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);
        let output = run_account(cfg.path(), &host, &["example-user-1", flag, "--json"]);
        assert!(
            output.status.success(),
            "{flag} --json: stderr: {}",
            stderr_of(&output)
        );
        let record = single_json_line(&output);
        assert_ok_record(&record, "example-user-1", "screenname", action);
        assert_eq!(record[key], json_value, "{flag}: {record}");
    }
}

#[tokio::test]
async fn is_locked_prints_true_for_a_locked_account() {
    let server = MockServer::start().await;
    let mut v = values();
    v["locked"] = json!(true);
    mount_info(
        &server,
        ResponseTemplate::new(200).set_body_json(ok_body(v)),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(cfg.path(), &host, &["example-user-1", "--is-locked"]);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "true\n");
}

// -- lock and unlock --

/// A lock sends the lookup, then one lock request with the looked-up item
/// name and the comment.
#[tokio::test]
async fn lock_and_unlock_send_lookup_then_one_request_with_comment() {
    for (flag, is_lock, word) in [
        ("--lock", "1", "locked"),
        ("-L", "1", "locked"),
        ("--unlock", "0", "unlocked"),
        ("-u", "0", "unlocked"),
    ] {
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({"success": true})),
            1,
        )
        .await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(
            cfg.path(),
            &host,
            &["example-user-1", flag, "--comment", "example note"],
        );
        assert!(
            output.status.success(),
            "{flag}: stderr: {}",
            stderr_of(&output)
        );
        assert_eq!(
            stdout_of(&output),
            format!("example-user-1: {word}\n"),
            "{flag}"
        );

        let info = requests_with_op(&server, "info").await;
        assert_eq!(info.len(), 1, "{flag}");
        assert_eq!(
            form_fields(&info[0]),
            pairs(&[("screenname", "example-user-1")])
        );
        let lock = requests_with_op(&server, "lock_unlock").await;
        assert_eq!(lock.len(), 1, "{flag}");
        assert_eq!(
            form_fields(&lock[0]),
            pairs(&[
                ("itemname", "@example-user-1"),
                ("is_lock", is_lock),
                ("comments", "example note"),
            ]),
            "{flag}"
        );
        // The server checks the order: the lookup comes first.
        let all = server.received_requests().await.unwrap();
        assert!(
            all[0]
                .url
                .query_pairs()
                .any(|(k, v)| k == "op" && v == "info"),
            "{flag}: lookup must be sent first"
        );
    }
}

#[tokio::test]
async fn lock_json_reports_itemname_and_response() {
    for (flag, action) in [("--lock", "lock"), ("--unlock", "unlock")] {
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        let answer = json!({"success": true, "values": {"note": "example"}});
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_json(&answer),
            1,
        )
        .await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(cfg.path(), &host, &["example-user-1", flag, "--json"]);
        assert!(
            output.status.success(),
            "{flag}: stderr: {}",
            stderr_of(&output)
        );
        let record = single_json_line(&output);
        assert_ok_record(&record, "example-user-1", "screenname", action);
        assert_eq!(record["itemname"], "@example-user-1", "{record}");
        assert_eq!(record["response"], answer, "{record}");
    }
}

/// An empty comment is not sent, nor is a missing one.
#[tokio::test]
async fn empty_or_missing_comment_sends_no_comments_field() {
    for args in [
        vec!["example-user-1", "--lock", "--comment", ""],
        vec!["example-user-1", "--lock"],
    ] {
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        mount_lock(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({"success": true})),
            1,
        )
        .await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(cfg.path(), &host, &args);
        assert!(
            output.status.success(),
            "{args:?}: stderr: {}",
            stderr_of(&output)
        );
        let lock = requests_with_op(&server, "lock_unlock").await;
        assert_eq!(
            form_fields(&lock[0]),
            pairs(&[("itemname", "@example-user-1"), ("is_lock", "1")]),
            "{args:?}"
        );
    }
}

/// A 2xx lock answer that is not JSON is success, as in the Python client.
#[tokio::test]
async fn non_json_lock_answer_is_success() {
    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    mount_lock(
        &server,
        ResponseTemplate::new(200).set_body_string("account locked"),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &["example-user-1", "--lock"]);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "example-user-1: locked\n");

    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    mount_lock(
        &server,
        ResponseTemplate::new(200).set_body_string("account locked"),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &["example-user-1", "--lock", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let record = single_json_line(&output);
    assert_ok_record(&record, "example-user-1", "screenname", "lock");
    assert_eq!(record["response"], json!("account locked"), "{record}");
}

// -- errors --

fn not_found_body() -> Value {
    json!({"success": false, "error": "account not found", "values": {}})
}

/// An unknown account fails the lookup; no lock request is sent.
#[tokio::test]
async fn unknown_account_exits_1_and_sends_no_lock() {
    let server = MockServer::start().await;
    mount_info(
        &server,
        ResponseTemplate::new(200).set_body_json(not_found_body()),
        1,
    )
    .await;
    mount_lock(&server, ResponseTemplate::new(200), 0).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(
        cfg.path(),
        &host,
        &["nobody@example.org", "--lock", "--comment", "example note"],
    );
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("error: nobody@example.org: "),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("account not found"), "stderr: {stderr}");
    assert_eq!(stdout_of(&output), "", "nothing on stdout");
}

#[tokio::test]
async fn unknown_account_json_is_an_error_record() {
    let server = MockServer::start().await;
    mount_info(
        &server,
        ResponseTemplate::new(200).set_body_json(not_found_body()),
        1,
    )
    .await;
    mount_lock(&server, ResponseTemplate::new(200), 0).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(
        cfg.path(),
        &host,
        &["nobody@example.org", "--lock", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_of(&output)
    );
    let record = single_json_line(&output);
    assert_eq!(record["account"], "nobody@example.org");
    assert_eq!(record["lookup"], "email");
    assert_eq!(record["action"], "lock");
    assert_eq!(record["status"], "error");
    assert_eq!(record["error"]["code"], "account_api_error", "{record}");
    assert_eq!(record["error"]["account"], "nobody@example.org", "{record}");
    assert_eq!(
        record["error"]["message"], "account API error for nobody@example.org: account not found",
        "{record}"
    );
    assert!(record["elapsed_ms"].is_u64(), "{record}");
}

/// Usage errors exit 2 and send nothing. Each names its cause: two action
/// flags conflict; `--comment` needs `--lock` or `--unlock`.
#[tokio::test]
async fn clap_usage_errors() {
    for (args, cause) in [
        (
            vec!["example-user-1", "--lock", "--unlock"],
            "cannot be used with",
        ),
        (
            vec!["example-user-1", "--get-email", "--is-locked"],
            "cannot be used with",
        ),
        (
            vec!["example-user-1", "--get-screenname", "--lock"],
            "cannot be used with",
        ),
        (
            vec!["example-user-1", "--comment", "example note"],
            "--lock",
        ),
        (
            vec!["example-user-1", "--get-email", "--comment", "example note"],
            "--lock",
        ),
    ] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);
        let output = run_account(cfg.path(), &host, &args);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            !stderr.contains("unrecognized subcommand"),
            "{args:?}: stderr: {stderr}"
        );
        assert!(stderr.contains(cause), "{args:?}: stderr: {stderr}");
    }
}

/// `-c`, `-i` and `-l` stay global (config file, insecure, log) before and
/// after the subcommand.
#[tokio::test]
async fn short_global_flags_keep_their_meaning() {
    let server = MockServer::start().await;
    mount_info(
        &server,
        ResponseTemplate::new(200).set_body_json(ok_body(values())),
        2,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let cfg_path = cfg.path().to_str().unwrap();

    let before = bare_cmd()
        .args(["-c", cfg_path, "-i", "-l", "-H", &host])
        .args(["account", "example-user-1", "--get-screenname"])
        .output()
        .unwrap();
    assert!(before.status.success(), "stderr: {}", stderr_of(&before));
    assert_eq!(stdout_of(&before), "example-user-1\n");

    let after = bare_cmd()
        .args(["account", "example-user-1", "--get-screenname"])
        .args(["-c", cfg_path, "-i", "-l", "-H", &host])
        .output()
        .unwrap();
    assert!(after.status.success(), "stderr: {}", stderr_of(&after));
    assert_eq!(stdout_of(&after), "example-user-1\n");
}

#[tokio::test]
async fn ac_alias_runs_the_command() {
    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = bare_cmd()
        .arg("--config-file")
        .arg(cfg.path())
        .args([
            "--insecure",
            "-H",
            &host,
            "ac",
            "example-user-1",
            "--get-itemname",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "@example-user-1\n");
}

/// Without the login cookies the command fails before any request.
#[tokio::test]
async fn missing_cookies_exit_1_with_no_request() {
    for args in [
        vec!["example-user-1"],
        vec!["example-user-1", "--lock", "--comment", "example note"],
    ] {
        let server = MockServer::start().await;
        mount_nothing(&server).await;
        let host = host_of(&server);
        let cfg = config_without_cookies(&host);

        let output = run_account(cfg.path(), &host, &args);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains("logged-in-user and logged-in-sig"),
            "{args:?}: stderr: {stderr}"
        );

        let mut json_args = args.clone();
        json_args.push("--json");
        let output = run_account(cfg.path(), &host, &json_args);
        assert_eq!(output.status.code(), Some(1), "{json_args:?}");
        let err = stderr_json_error(&output);
        assert_eq!(err["error"]["code"], "auth_error", "{err}");
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("logged-in-user and logged-in-sig"),
            "{err}"
        );
        assert_eq!(stdout_of(&output), "", "{json_args:?}: nothing on stdout");
    }
}

/// The cookies go only to archive.org, its subdomains and loopback hosts.
/// `0.0.0.0` is outside that set and can only reach this machine, so a
/// request sent by mistake lands on the mock and fails its `expect(0)`.
#[tokio::test]
async fn non_archive_host_exits_1_with_no_request() {
    let server = MockServer::start().await;
    mount_nothing(&server).await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let other_host = format!("0.0.0.0:{}", server.address().port());

    let output = run_account(cfg.path(), &other_host, &["example-user-1", "--lock"]);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("sends login cookies only to archive.org"),
        "stderr: {stderr}"
    );

    let output = run_account(
        cfg.path(),
        &other_host,
        &["example-user-1", "--lock", "--json"],
    );
    assert_eq!(output.status.code(), Some(1));
    let err = stderr_json_error(&output);
    assert_eq!(err["error"]["code"], "auth_error", "{err}");
}

// -- quiet levels --

#[tokio::test]
async fn quiet_drops_the_lock_line() {
    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    mount_lock(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);
    let output = run_account(cfg.path(), &host, &["-q", "example-user-1", "--lock"]);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "", "-q drops the lock result line");
}

/// Read values are the output, so they print at every quiet level.
#[tokio::test]
async fn quiet_keeps_read_values() {
    for args in [
        vec!["-q", "example-user-1", "--get-screenname"],
        vec!["-qq", "example-user-1", "--get-screenname"],
        vec!["-q", "example-user-1"],
    ] {
        let server = MockServer::start().await;
        mount_info_ok(&server).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);
        let output = run_account(cfg.path(), &host, &args);
        assert!(
            output.status.success(),
            "{args:?}: stderr: {}",
            stderr_of(&output)
        );
        let stdout = stdout_of(&output);
        if args.len() == 3 {
            assert_eq!(stdout, "example-user-1\n", "{args:?}");
        } else {
            let printed: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(printed, values(), "{args:?}");
        }
    }
}

#[tokio::test]
async fn quiet_keeps_errors_and_qq_drops_them() {
    for (quiet, error_shown) in [("-q", true), ("-qq", false)] {
        let server = MockServer::start().await;
        mount_info(
            &server,
            ResponseTemplate::new(200).set_body_json(not_found_body()),
            1,
        )
        .await;
        mount_lock(&server, ResponseTemplate::new(200), 0).await;
        let host = host_of(&server);
        let cfg = config_with_cookies(&host);

        let output = run_account(cfg.path(), &host, &[quiet, "nobody@example.org", "--lock"]);
        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(1), "{quiet}: stderr: {stderr}");
        assert_eq!(
            stderr.contains("error: nobody@example.org: "),
            error_shown,
            "{quiet}: stderr: {stderr}"
        );
        if !error_shown {
            assert!(
                !stderr.contains("account not found"),
                "{quiet}: stderr: {stderr}"
            );
        }
    }
}

// -- rate limiting --

#[tokio::test]
async fn lookup_429_pauses_and_retries() {
    let server = MockServer::start().await;
    // First lookup: 429. Mounted first, so it answers the first request;
    // the retry falls through to the 200 below.
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_info_ok(&server).await;
    mount_lock(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(cfg.path(), &host, &["example-user-1", "--lock"]);
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("Rate limited. Pausing all workers for 1s"),
        "stderr: {stderr}"
    );
    assert_eq!(stdout_of(&output), "example-user-1: locked\n");
    assert_eq!(requests_with_op(&server, "info").await.len(), 2);
    assert_eq!(requests_with_op(&server, "lock_unlock").await.len(), 1);
}

#[tokio::test]
async fn lock_429_pauses_and_retries_only_the_lock() {
    let server = MockServer::start().await;
    mount_info_ok(&server).await;
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "lock_unlock"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_lock(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
        1,
    )
    .await;
    let host = host_of(&server);
    let cfg = config_with_cookies(&host);

    let output = run_account(
        cfg.path(),
        &host,
        &["example-user-1", "--lock", "--comment", "example note"],
    );
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("Rate limited. Pausing all workers for 1s"),
        "stderr: {stderr}"
    );
    assert_eq!(stdout_of(&output), "example-user-1: locked\n");
    assert_eq!(requests_with_op(&server, "info").await.len(), 1);
    let lock = requests_with_op(&server, "lock_unlock").await;
    assert_eq!(lock.len(), 2);
    for req in &lock {
        assert_eq!(
            form_fields(req),
            pairs(&[
                ("itemname", "@example-user-1"),
                ("is_lock", "1"),
                ("comments", "example note"),
            ])
        );
    }
}
