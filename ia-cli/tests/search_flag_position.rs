//! Where `ia search` options may go relative to the subcommand name.
//!
//! The bare command (`ia search [OPTIONS] QUERY`) and each subcommand
//! (`ia search scrape|advanced|fts QUERY [OPTIONS]`) take their options.
//! An option given before a subcommand name (`ia search --timeout 5 scrape
//! QUERY`) is used as if given after it, as `ia update --json list` is,
//! when that subcommand takes it. A query, or an option the subcommand does
//! not take, given before the name is refused before any request instead of
//! being dropped.
//!
//! Hermetic: every request goes to a wiremock server named as `host` and
//! `fts_host` in a temporary config file.

use std::fs;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SUBCOMMANDS: [&str; 3] = ["scrape", "advanced", "fts"];

/// A server that answers every backend with one result and a total of 7,
/// after `delay`. Each result has a field besides the identifier, so output
/// without `--itemlist` differs from output with it.
fn search_server_with_delay(rt: &tokio::runtime::Runtime, delay: Duration) -> MockServer {
    let server = rt.block_on(MockServer::start());
    rt.block_on(async {
        Mock::given(path("/services/search/v1/scrape"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "items": [{"identifier": "item-1", "title": "Item One"}],
                        "count": 1,
                        "total": 7
                    }))
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        Mock::given(path("/advancedsearch.php"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "response": {"numFound": 7, "start": 0, "docs": [{"identifier": "item-1", "title": "Item One"}]}
                    }))
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        Mock::given(path("/ia-pub-fts-api"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "hits": {"total": 7, "hits": [{
                            "_id": "item-1|0000000000000000000000000000000000000000",
                            "fields": {"identifier": ["item-1"]}
                        }]},
                        "_scroll_id": ""
                    }))
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
    });
    server
}

fn search_server(rt: &tokio::runtime::Runtime) -> MockServer {
    search_server_with_delay(rt, Duration::ZERO)
}

fn config_for(server: &MockServer) -> NamedTempFile {
    let host = server.uri().trim_start_matches("http://").to_string();
    let f = NamedTempFile::new().unwrap();
    fs::write(
        f.path(),
        format!("[general]\nhost = {host}\nfts_host = {host}\nsecure = false\n"),
    )
    .unwrap();
    f
}

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY");
    cmd
}

/// Runs `ia search <args>` against the mock server, asserts success, and
/// returns stdout and the requests sent.
fn run_ok(args: &[&str]) -> (String, Vec<Request>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server(&rt);
    let cfg = config_for(&server);
    let mut full = vec!["search"];
    full.extend_from_slice(args);
    let out = ia_with_config(&cfg).args(&full).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let requests = rt.block_on(server.received_requests()).unwrap();
    (stdout, requests)
}

fn stdout_of(args: &[&str]) -> String {
    run_ok(args).0
}

/// Runs `ia search <args>`, asserts failure with nothing on stdout and no
/// request sent, and returns stderr.
fn run_refused(args: &[&str]) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server(&rt);
    let cfg = config_for(&server);
    let mut full = vec!["search"];
    full.extend_from_slice(args);
    let out = ia_with_config(&cfg)
        .args(&full)
        .assert()
        .failure()
        .code(1)
        .stdout("");
    let requests = rt.block_on(server.received_requests()).unwrap();
    assert!(
        requests.is_empty(),
        "{full:?} sent {} request(s); expected none",
        requests.len()
    );
    String::from_utf8(out.get_output().stderr.clone()).unwrap()
}

/// The request line's query string (scrape and advanced send everything
/// there; fts sends a JSON body, checked separately).
fn query_of(requests: &[Request]) -> String {
    requests
        .iter()
        .map(|r| r.url.query().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("&")
}

// ─── Before the subcommand name: used, as after it ───────────────────────────

#[test]
fn num_found_before_subcommand_is_used() {
    for sub in SUBCOMMANDS {
        assert_eq!(stdout_of(&["-n", sub, "collection:test"]), "7\n", "{sub}");
    }
}

#[test]
fn json_before_subcommand_is_used() {
    for sub in SUBCOMMANDS {
        assert_eq!(
            stdout_of(&["--json", sub, "collection:test", "-n"]),
            "{\"num_found\":7}\n",
            "{sub}"
        );
    }
}

/// `ia search --itemlist <sub> QUERY` prints identifiers only. Control:
/// without `--itemlist`, every backend prints more than the identifier
/// (the mock results carry a title), so the assertion observes the flag.
fn itemlist_before_subcommand_is_used(sub: &str) {
    let without = stdout_of(&[sub, "collection:test", "-p", "rows=1"]);
    assert!(
        without.starts_with("item-1") && without != "item-1\n",
        "{sub}: {without:?}"
    );
    assert_eq!(
        stdout_of(&["--itemlist", sub, "collection:test", "-p", "rows=1"]),
        "item-1\n",
        "{sub}"
    );
}

#[test]
fn itemlist_before_scrape_is_used() {
    itemlist_before_subcommand_is_used("scrape");
}

#[test]
fn itemlist_before_advanced_is_used() {
    itemlist_before_subcommand_is_used("advanced");
}

#[test]
fn itemlist_before_fts_is_used() {
    itemlist_before_subcommand_is_used("fts");
}

#[test]
fn parameters_before_subcommand_are_used() {
    for sub in SUBCOMMANDS {
        let stderr = run_refused(&["-p", "bad", sub, "collection:test"]);
        assert!(stderr.contains("KEY=VALUE or KEY:VALUE"), "{sub}: {stderr}");
    }
    for sub in ["scrape", "advanced"] {
        let (_, requests) = run_ok(&["-p", "extra=before", sub, "collection:test", "-p", "rows=1"]);
        assert!(
            query_of(&requests).contains("extra=before"),
            "{sub}: {}",
            query_of(&requests)
        );
    }
}

#[test]
fn sort_and_field_before_scrape_and_advanced_are_used() {
    for sub in ["scrape", "advanced"] {
        let (stdout, requests) = run_ok(&[
            "-s",
            "downloads desc",
            "-f",
            "title",
            sub,
            "collection:test",
            "--itemlist",
            "-p",
            "rows=1",
        ]);
        assert_eq!(stdout, "item-1\n", "{sub}");
        let q = query_of(&requests);
        assert!(q.contains("downloads"), "{sub}: sort not sent: {q}");
        assert!(q.contains("title"), "{sub}: field not sent: {q}");
    }
}

fn timeout_before_subcommand_is_used(sub: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server_with_delay(&rt, Duration::from_secs(30));
    let cfg = config_for(&server);
    let started = std::time::Instant::now();
    let out = ia_with_config(&cfg)
        .args(["search", "--timeout", "1", sub, "collection:test", "-n"])
        .timeout(Duration::from_secs(25))
        .assert()
        .failure()
        .code(1);
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "{sub}: --timeout before the subcommand was not applied"
    );
    // The request was sent and timed out; the command was not refused.
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(!stderr.contains("before the subcommand"), "{sub}: {stderr}");
    assert!(
        !rt.block_on(server.received_requests()).unwrap().is_empty(),
        "{sub}: no request sent"
    );
}

#[test]
fn timeout_before_scrape_is_used() {
    timeout_before_subcommand_is_used("scrape");
}

#[test]
fn timeout_before_advanced_is_used() {
    timeout_before_subcommand_is_used("advanced");
}

#[test]
fn timeout_before_fts_is_used() {
    timeout_before_subcommand_is_used("fts");
}

/// A flag given on both sides counts once, as with `ia update --json list
/// --json`; list options add up, in command-line order.
#[test]
fn options_on_both_sides_combine() {
    for sub in SUBCOMMANDS {
        assert_eq!(
            stdout_of(&["--json", sub, "--json", "-n", "collection:test"]),
            "{\"num_found\":7}\n",
            "{sub}"
        );
    }
    let (_, requests) = run_ok(&[
        "-p",
        "first=1",
        "scrape",
        "collection:test",
        "-p",
        "second=2",
        "--itemlist",
    ]);
    let q = query_of(&requests);
    assert!(q.contains("first=1") && q.contains("second=2"), "{q}");
}

// ─── Before the subcommand name: refused ─────────────────────────────────────

#[test]
fn query_before_subcommand_is_refused() {
    for sub in SUBCOMMANDS {
        let stderr = run_refused(&["other-query", sub, "collection:test"]);
        assert!(stderr.contains("'other-query'"), "{sub}: {stderr}");
        assert!(stderr.contains(&format!("'{sub}'")), "{sub}: {stderr}");
    }
}

/// fts has no -s or -f; given before `fts` they were dropped.
#[test]
fn sort_and_field_before_fts_are_refused() {
    let stderr = run_refused(&["-s", "downloads desc", "fts", "q"]);
    assert!(stderr.contains("-s/--sort"), "{stderr}");
    assert!(stderr.contains("'fts'"), "{stderr}");
    let stderr = run_refused(&["--field", "title", "fts", "q"]);
    assert!(stderr.contains("-f/--field"), "{stderr}");
}

/// A subcommand's own options (not the bare form's) go after its name;
/// before it, clap refuses them as unexpected arguments, so nothing is
/// dropped.
#[test]
fn subcommand_own_options_before_its_name_are_refused() {
    for (option, sub) in [
        (["--rows", "5"], "advanced"),
        (["-r", "5"], "advanced"),
        (["--size", "5"], "fts"),
        (["--scope", "all"], "fts"),
        (["--from", "5"], "fts"),
    ] {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = search_server(&rt);
        let cfg = config_for(&server);
        let out = ia_with_config(&cfg)
            .args(["search", option[0], option[1], sub, "q"])
            .assert()
            .failure()
            .stdout("");
        let stderr = String::from_utf8_lossy(&out.get_output().stderr);
        assert!(
            stderr.contains(&format!("unexpected argument '{}'", option[0])),
            "{option:?} {sub}: {stderr}"
        );
        assert!(rt.block_on(server.received_requests()).unwrap().is_empty());
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server(&rt);
    let cfg = config_for(&server);
    let out = ia_with_config(&cfg)
        .args(["search", "--dsl", "fts", "q"])
        .assert()
        .failure()
        .stdout("");
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("unexpected argument '--dsl'"), "{stderr}");
    assert!(rt.block_on(server.received_requests()).unwrap().is_empty());
}

/// A single-value option given on both sides is refused, as clap refuses
/// `--timeout` given twice after the subcommand name.
#[test]
fn timeout_on_both_sides_is_refused() {
    for sub in SUBCOMMANDS {
        let stderr = run_refused(&["--timeout", "5", sub, "--timeout", "6", "q"]);
        assert!(
            stderr.contains("--timeout") && stderr.contains("more than once"),
            "{sub}: {stderr}"
        );
    }
}

/// With --json on either side, the refusal is the JSON error on stderr.
#[test]
fn refusal_with_json_is_a_json_error() {
    for args in [
        ["--json", "other-query", "scrape", "q"],
        ["other-query", "scrape", "q", "--json"],
    ] {
        let stderr = run_refused(&args);
        let v: serde_json::Value = serde_json::from_str(stderr.trim())
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e}): {stderr}"));
        assert_eq!(v["error"]["code"], "invalid_argument", "{args:?}");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("'other-query'"),
            "{args:?}: {stderr}"
        );
    }
}

// ─── Bare form, after the name, `--`, and help: unchanged or stated ──────────

#[test]
fn bare_form_takes_its_options() {
    assert_eq!(stdout_of(&["collection:test", "-n"]), "7\n");
    assert_eq!(stdout_of(&["-n", "collection:test"]), "7\n");
    assert_eq!(
        stdout_of(&["collection:test", "--json", "-n", "--timeout", "5"]),
        "{\"num_found\":7}\n"
    );
    // Without --itemlist the title is printed too, so this observes the flag.
    assert_ne!(stdout_of(&["collection:test", "-p", "rows=1"]), "item-1\n");
    assert_eq!(
        stdout_of(&["--itemlist", "collection:test", "-p", "rows=1"]),
        "item-1\n"
    );
    assert_eq!(
        stdout_of(&[
            "collection:test",
            "-s",
            "downloads desc",
            "-f",
            "identifier",
            "--itemlist"
        ]),
        "item-1\n"
    );
}

#[test]
fn options_after_subcommand_name_are_used() {
    for sub in SUBCOMMANDS {
        assert_eq!(stdout_of(&[sub, "collection:test", "-n"]), "7\n", "{sub}");
        assert_eq!(
            stdout_of(&[sub, "--json", "-n", "--timeout", "5", "collection:test"]),
            "{\"num_found\":7}\n",
            "{sub}"
        );
        assert_eq!(
            stdout_of(&[sub, "collection:test", "--itemlist", "-p", "rows=1"]),
            "item-1\n",
            "{sub}"
        );
    }
    for sub in ["scrape", "advanced"] {
        assert_eq!(
            stdout_of(&[
                sub,
                "collection:test",
                "-s",
                "downloads desc",
                "-f",
                "identifier",
                "--itemlist",
                "-p",
                "rows=1",
            ]),
            "item-1\n",
            "{sub}"
        );
    }
}

/// `--` makes a subcommand name the query.
#[test]
fn double_dash_searches_for_a_subcommand_name() {
    let (stdout, requests) = run_ok(&["--itemlist", "--", "scrape"]);
    assert_eq!(stdout, "item-1\n");
    assert!(
        requests[0].url.path() == "/services/search/v1/scrape"
            && query_of(&requests).contains("q=scrape"),
        "{}",
        query_of(&requests)
    );
}

/// The usage line shows the two accepted forms, not `[QUERY] [COMMAND]`,
/// in both `-h` and `--help`; the help says how to search for a subcommand
/// name.
#[test]
fn help_usage_shows_the_accepted_forms() {
    for flag in ["-h", "--help"] {
        let out = assert_cmd::cargo_bin_cmd!("ia-cli")
            .args(["search", flag])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(!stdout.contains("[QUERY] [COMMAND]"), "{flag}: {stdout}");
        assert!(
            stdout.contains("search [OPTIONS] <QUERY>"),
            "{flag}: {stdout}"
        );
        assert!(
            stdout.contains("search [OPTIONS] <COMMAND> <QUERY> [OPTIONS]"),
            "{flag}: {stdout}"
        );
        assert!(
            stdout.contains("ia search --itemlist -- scrape"),
            "{flag}: {stdout}"
        );
    }
}
