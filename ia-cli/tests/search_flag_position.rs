//! Where `ia search` options may go relative to the subcommand name.
//!
//! The bare command (`ia search QUERY [OPTIONS]`) and each subcommand
//! (`ia search scrape|advanced|fts QUERY [OPTIONS]`) take their options.
//! A bare-mode option or query given before a subcommand name
//! (`ia search --timeout 5 scrape QUERY`) is refused before any request,
//! rather than parsed and then not used.
//!
//! Hermetic: every request goes to a wiremock server named as `host` and
//! `fts_host` in a temporary config file.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUBCOMMANDS: [&str; 3] = ["scrape", "advanced", "fts"];

/// A server that answers every backend with one result and a total of 7.
fn search_server(rt: &tokio::runtime::Runtime) -> MockServer {
    let server = rt.block_on(MockServer::start());
    rt.block_on(async {
        Mock::given(path("/services/search/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"identifier": "item-1"}],
                "count": 1,
                "total": 7
            })))
            .mount(&server)
            .await;
        Mock::given(path("/advancedsearch.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {"numFound": 7, "start": 0, "docs": [{"identifier": "item-1"}]}
            })))
            .mount(&server)
            .await;
        Mock::given(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {"total": 7, "hits": [{
                    "_id": "item-1|0000000000000000000000000000000000000000",
                    "fields": {"identifier": ["item-1"]}
                }]},
                "_scroll_id": ""
            })))
            .mount(&server)
            .await;
    });
    server
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

/// `ia search <before...> <subcommand> QUERY` fails for every subcommand,
/// names `named` on stderr, and sends no request.
fn assert_refused_before_each_subcommand(before: &[&str], named: &str) {
    for sub in SUBCOMMANDS {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = search_server(&rt);
        let cfg = config_for(&server);

        let mut args = vec!["search"];
        args.extend_from_slice(before);
        args.extend_from_slice(&[sub, "collection:test"]);

        ia_with_config(&cfg)
            .args(&args)
            .assert()
            .failure()
            .stdout("")
            .stderr(predicate::str::contains(named))
            .stderr(predicate::str::contains(format!("'{sub}'")));

        let requests = rt.block_on(server.received_requests()).unwrap();
        assert!(
            requests.is_empty(),
            "{args:?} sent {} request(s); expected none",
            requests.len()
        );
    }
}

// ─── Before the subcommand name: refused ─────────────────────────────────────

#[test]
fn timeout_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["--timeout", "5"], "--timeout");
}

#[test]
fn json_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["--json"], "--json");
}

#[test]
fn itemlist_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["--itemlist"], "--itemlist");
}

#[test]
fn num_found_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["-n"], "--num-found");
}

#[test]
fn parameters_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["-p", "rows=1"], "--parameters");
}

#[test]
fn sort_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["-s", "downloads desc"], "--sort");
}

#[test]
fn field_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["-f", "title"], "--field");
}

/// A query before the subcommand name (`ia search q scrape other`) was
/// dropped too, and the subcommand searched for `other`.
#[test]
fn query_before_subcommand_is_refused() {
    assert_refused_before_each_subcommand(&["other-query"], "other-query");
}

/// All the given options are named, not only the first.
#[test]
fn every_option_before_subcommand_is_named() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server(&rt);
    let cfg = config_for(&server);

    ia_with_config(&cfg)
        .args(["search", "--json", "--timeout", "5", "scrape", "q"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("--json"))
        .stderr(predicate::str::contains("--timeout"));
    assert!(rt.block_on(server.received_requests()).unwrap().is_empty());
}

// ─── Bare form and after the subcommand name: unchanged ──────────────────────

/// Runs `ia search <args>` against the mock server and returns stdout.
fn run_ok(args: &[&str]) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = search_server(&rt);
    let cfg = config_for(&server);
    let mut full = vec!["search"];
    full.extend_from_slice(args);
    let out = ia_with_config(&cfg).args(&full).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

#[test]
fn bare_form_takes_its_options() {
    assert_eq!(run_ok(&["collection:test", "-n"]), "7\n");
    assert_eq!(run_ok(&["-n", "collection:test"]), "7\n");
    assert_eq!(
        run_ok(&["collection:test", "--json", "-n", "--timeout", "5"]),
        "{\"num_found\":7}\n"
    );
    assert_eq!(
        run_ok(&["--itemlist", "collection:test", "-p", "rows=1"]),
        "item-1\n"
    );
}

#[test]
fn options_after_subcommand_name_are_used() {
    for sub in SUBCOMMANDS {
        assert_eq!(run_ok(&[sub, "collection:test", "-n"]), "7\n", "{sub}");
        assert_eq!(
            run_ok(&[sub, "--json", "-n", "--timeout", "5", "collection:test"]),
            "{\"num_found\":7}\n",
            "{sub}"
        );
        assert_eq!(
            run_ok(&[sub, "collection:test", "--itemlist", "-p", "rows=1"]),
            "item-1\n",
            "{sub}"
        );
    }
}

/// `-s` and `-f` exist on the bare form, `scrape` and `advanced`. (`-p rows=1`
/// keeps `advanced` to one page: the mock returns one result per page.)
#[test]
fn sort_and_field_after_subcommand_name_are_accepted() {
    for sub in ["scrape", "advanced"] {
        assert_eq!(
            run_ok(&[
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
    assert_eq!(
        run_ok(&[
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
