//! Search-parameter passthrough (`--search-parameter`) and
//! the clearer "search returned 0 results" message on `--search` commands.
//!
//! All tests are hermetic: the bad-format cases error while parsing parameters,
//! before any network request; the empty-result case uses wiremock.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

// ─── Bad-format rejection (parsed before any network call) ───────────────────

#[test]
fn metadata_export_rejects_bad_search_parameter() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "metadata",
            "export",
            "--search",
            "x",
            "--search-parameter",
            "badparam",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("KEY=VALUE or KEY:VALUE"));
}

#[test]
fn metadata_audit_rejects_bad_search_parameter() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "metadata",
            "audit",
            "--search",
            "x",
            "--search-parameter",
            "badparam",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("KEY=VALUE or KEY:VALUE"));
}

#[test]
fn metadata_modify_rejects_bad_search_parameter() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "metadata",
            "modify",
            "--search",
            "x",
            "--search-parameter",
            "badparam",
            "-m",
            "k:v",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("KEY=VALUE or KEY:VALUE"));
}

#[test]
fn tasks_submit_rejects_bad_search_parameter() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "tasks",
            "submit",
            "--search",
            "x",
            "--search-parameter",
            "badparam",
            "--cmd",
            "derive",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("KEY=VALUE or KEY:VALUE"));
}

/// `tasks submit -p` is a *task* parameter, not a search parameter, so a bare
/// `derive` value (no `=`/`:`) is accepted there — it must NOT be rejected by the
/// search-parameter parser.
#[test]
fn tasks_submit_dash_p_is_task_parameter_not_search() {
    let cfg = empty_config();
    // `-p foo=bar` is a valid task parameter; this should fail later (no network
    // / no creds), but never with the search-parameter format error.
    let output = ia_with_config(&cfg)
        .args([
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "-p",
            "foo=bar",
            "--dry-run",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("KEY=VALUE or KEY:VALUE"),
        "-p on tasks submit must be a task parameter, not parsed as a search parameter: {stderr}"
    );
}

// ─── Empty search results → clear message (not "No input provided") ──────────

#[tokio::test]
async fn metadata_export_empty_search_reports_zero_results() {
    let mock_server = MockServer::start().await;

    // Scrape matches the sorts param we pass and returns zero items.
    Mock::given(method("POST"))
        .and(path("/services/search/v1/scrape"))
        .and(query_param("sorts", "identifier asc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "count": 0,
            "total": 0,
            "cursor": ""
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let cfg = empty_config();
    let output = ia_with_config(&cfg)
        .args([
            "--insecure",
            "-H",
            &host,
            "metadata",
            "export",
            "--search",
            "collection:empty",
            "--search-parameter",
            "sorts=identifier asc",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("0 results for 'collection:empty'"),
        "expected a clear empty-result message, got: {stderr}"
    );
    assert!(
        !stderr.contains("No input provided"),
        "must not claim 'No input provided' when --search ran but matched nothing: {stderr}"
    );
}
