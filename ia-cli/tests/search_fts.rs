//! `ia search fts` against a mocked full-text search API (#124).
//!
//! Hermetic: every request goes to a wiremock server named as `fts_host` in
//! a temporary config file.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FTS_PATH: &str = "/ia-pub-fts-api";

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

fn one_hit_page() -> serde_json::Value {
    json!({
        "hits": {"total": 1, "hits": [{
            "_id": "comingofageinlan0000your|1319f8442aca0dbe3c25322fea16539a99ce3f94",
            "fields": {"identifier": ["comingofageinlan0000your"]}
        }]},
        "_scroll_id": ""
    })
}

/// The API's 200 error envelope is printed on stderr, the exit code is
/// non-zero, and no result line or zero count is printed first.
#[test]
fn fts_error_envelope_prints_the_server_message_and_fails() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    rt.block_on(
        Mock::given(method("POST"))
            .and(path(FTS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_message": {"msg_type": "ERROR", "text": "the parameter q is required"},
                "ia-pub-fts-api-wrapper": true
            })))
            .mount(&server),
    );
    let cfg = config_for(&server);

    ia_with_config(&cfg)
        .args(["search", "fts", "computer"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("the parameter q is required"))
        .stderr(predicate::str::contains("0 results").not())
        .stderr(predicate::str::contains("no results found").not());
}

/// `--itemlist` prints item identifiers that `ia download` accepts, not
/// the hit's `_id` (`identifier|sha1`).
#[test]
fn fts_itemlist_prints_the_item_identifier() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    rt.block_on(
        Mock::given(method("POST"))
            .and(path(FTS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(one_hit_page()))
            .mount(&server),
    );
    let cfg = config_for(&server);

    ia_with_config(&cfg)
        .args(["search", "fts", "computer", "--itemlist"])
        .assert()
        .success()
        .stdout("comingofageinlan0000your\n");
}

/// `--size` and `--from` go out as JSON numbers, and `--from` turns scroll
/// off (the API rejects `from` with scroll).
#[test]
fn fts_size_and_from_are_numbers_and_from_turns_scroll_off() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    rt.block_on(
        Mock::given(method("POST"))
            .and(path(FTS_PATH))
            .and(body_partial_json(json!({
                "q": "!L computer", "size": 5, "from": 10, "scroll": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(one_hit_page()))
            .expect(1)
            .mount(&server),
    );
    let cfg = config_for(&server);

    ia_with_config(&cfg)
        .args([
            "search",
            "fts",
            "computer",
            "--size",
            "5",
            "--from",
            "10",
            "--itemlist",
        ])
        .assert()
        .success()
        .stdout("comingofageinlan0000your\n");
    rt.block_on(server.verify());
}

/// `--num-found` sends `--scope` as well as the query.
#[test]
fn fts_num_found_sends_scope() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    rt.block_on(
        Mock::given(method("GET"))
            .and(path(FTS_PATH))
            .and(query_param("q", "!L computer"))
            .and(query_param("scope", "all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {"total": 6524253, "hits": []}
            })))
            .expect(1)
            .mount(&server),
    );
    let cfg = config_for(&server);

    ia_with_config(&cfg)
        .args(["search", "fts", "computer", "--scope", "all", "--num-found"])
        .assert()
        .success()
        .stdout("6524253\n");
    rt.block_on(server.verify());
}
