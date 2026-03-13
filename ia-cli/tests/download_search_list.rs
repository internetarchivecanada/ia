use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use std::fs;
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ia_cmd() -> Command {
    assert_cmd::cargo_bin_cmd!("ia")
}

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

/// Standard metadata response for a test item with two files.
fn metadata_response() -> serde_json::Value {
    json!({
        "metadata": {
            "identifier": "test-item",
            "mediatype": "texts",
            "title": "Test Item",
            "collection": ["opensource"]
        },
        "files": [
            {
                "name": "test.pdf",
                "source": "original",
                "format": "Text PDF",
                "size": "12345",
                "md5": "d41d8cd98f00b204e9800998ecf8427e",
                "mtime": "1700000000"
            },
            {
                "name": "test_meta.xml",
                "source": "metadata",
                "format": "Metadata",
                "size": "500",
                "md5": "abc123",
                "mtime": "1700000001"
            }
        ]
    })
}

// ─── Download tests ──────────────────────────────────────────────────────────

#[test]
fn download_help_shows_key_flags() {
    ia_cmd()
        .args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--glob"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--dashboard"))
        .stdout(predicate::str::contains("--search"))
        .stdout(predicate::str::contains("--itemlist"));
}

#[test]
fn download_no_identifier_and_no_search_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg).arg("download").assert().failure();
}

#[tokio::test]
async fn download_dry_run_shows_files() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata_response()))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let dir = TempDir::new().unwrap();
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "download",
            "test-item",
            "--dry-run",
            "--destdir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("test.pdf") || stderr.contains("test-item"),
        "dry-run output should mention files or item: {stderr}"
    );
}

#[tokio::test]
async fn download_json_output_produces_jsonl() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata_response()))
        .mount(&mock_server)
        .await;

    // Mock the actual file download
    Mock::given(method("GET"))
        .and(path("/download/test-item/test.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"fake pdf content")
                .insert_header("content-length", "16"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/test-item/test_meta.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"fake metadata")
                .insert_header("content-length", "13"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let dir = TempDir::new().unwrap();
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "download",
            "test-item",
            "--json",
            "--destdir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Each line should be valid JSON
    for line in stdout.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("expected valid JSON line, got: {line}"));
        // Should have an identifier or file field
        assert!(
            parsed.get("identifier").is_some() || parsed.get("file").is_some(),
            "JSON output missing identifier or file field: {parsed}"
        );
    }
}

#[tokio::test]
async fn download_not_found_item_errors() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/nonexistent-item"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let dir = TempDir::new().unwrap();
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "download",
            "nonexistent-item",
            "--destdir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "expected 'not found' in error: {stderr}"
    );
}

// ─── List tests ──────────────────────────────────────────────────────────────

#[test]
fn list_help_shows_key_flags() {
    ia_cmd()
        .args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--glob"))
        .stdout(predicate::str::contains("--columns"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--location"))
        .stdout(predicate::str::contains("--source"));
}

#[test]
fn list_alias_ls_works() {
    ia_cmd()
        .args(["ls", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List files"));
}

#[tokio::test]
async fn list_json_outputs_file_objects() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata_response()))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "list", "test-item", "--json"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should output at least one JSON line per file
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected at least 2 lines of JSON output, got {}",
        lines.len()
    );
    for line in &lines {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("expected valid JSON, got: {line}"));
        assert!(
            parsed.get("name").is_some(),
            "JSON file object missing 'name' field: {parsed}"
        );
    }
}

#[tokio::test]
async fn list_source_filter_works() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata_response()))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "list",
            "test-item",
            "--source",
            "original",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    // Only the original file should appear (test.pdf), not the metadata file
    assert_eq!(
        lines.len(),
        1,
        "expected 1 original file, got {}",
        lines.len()
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["name"], "test.pdf");
}

#[tokio::test]
async fn list_not_found_item_errors() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/nonexistent-item"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "list", "nonexistent-item"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "expected 'not found' in error: {stderr}"
    );
}

// ─── Search tests ────────────────────────────────────────────────────────────

#[test]
fn search_help_shows_subcommands() {
    ia_cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scrape"))
        .stdout(predicate::str::contains("advanced"))
        .stdout(predicate::str::contains("fts"));
}

#[tokio::test]
async fn search_scrape_json_outputs_results() {
    let mock_server = MockServer::start().await;

    let scrape_response = json!({
        "items": [
            {"identifier": "item-1"},
            {"identifier": "item-2"}
        ],
        "count": 2,
        "total": 2
    });

    Mock::given(method("POST"))
        .and(path("/services/search/v1/scrape"))
        .respond_with(ResponseTemplate::new(200).set_body_json(scrape_response))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "search",
            "collection:test",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("item-1"));
    assert!(stdout.contains("item-2"));
}

#[tokio::test]
async fn search_advanced_num_found() {
    let mock_server = MockServer::start().await;

    let advanced_response = json!({
        "response": {
            "numFound": 42,
            "start": 0,
            "docs": []
        }
    });

    Mock::given(method("GET"))
        .and(path("/advancedsearch.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(advanced_response))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "search",
            "advanced",
            "collection:test",
            "--num-found",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "expected num_found=42 in output: {stdout}"
    );
}

#[tokio::test]
async fn search_itemlist_outputs_identifiers_only() {
    let mock_server = MockServer::start().await;

    let scrape_response = json!({
        "items": [
            {"identifier": "alpha-item"},
            {"identifier": "beta-item"}
        ],
        "count": 2,
        "total": 2
    });

    Mock::given(method("POST"))
        .and(path("/services/search/v1/scrape"))
        .respond_with(ResponseTemplate::new(200).set_body_json(scrape_response))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "search",
            "collection:test",
            "--itemlist",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "alpha-item");
    assert_eq!(lines[1], "beta-item");
}
