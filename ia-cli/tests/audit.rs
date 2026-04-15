use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_schema_json() -> &'static str {
    r#"{
        "metadata_schema": [
            {
                "field": "identifier",
                "label": "Identifier",
                "required": "Yes",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "not editable",
                "definition": "Unique identifier",
                "accepted values": "String"
            },
            {
                "field": "title",
                "label": "Title",
                "required": "Recommended",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Title of media",
                "accepted values": "String, plain text"
            },
            {
                "field": "collection",
                "label": "Collection",
                "required": "Yes",
                "repeatable": "Yes",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Parent collection"
            },
            {
                "field": "mediatype",
                "label": "Media Type",
                "required": "Yes",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Type of media"
            },
            {
                "field": "date",
                "label": "Date",
                "required": "No",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Date of media",
                "accepted values": "YYYY-MM-DD"
            }
        ],
        "files_schema": []
    }"#
}

fn clean_item_json() -> &'static str {
    r#"{
        "metadata": {
            "identifier": "clean-item",
            "title": "A Clean Item",
            "collection": "test-collection",
            "mediatype": "texts",
            "date": "2024-01-15"
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    }"#
}

fn item_with_date_array_json() -> &'static str {
    r#"{
        "metadata": {
            "identifier": "bad-date",
            "title": "Item With Bad Date",
            "collection": "test-collection",
            "mediatype": "texts",
            "date": ["2004", "December 6, 2004", "December 6, 2004"]
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    }"#
}

fn item_missing_required_json() -> &'static str {
    r#"{
        "metadata": {
            "identifier": "missing-fields"
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    }"#
}

async fn setup_mock(server: &MockServer, identifier: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/metadata/{identifier}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

async fn setup_schema_mock(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn audit_clean_item_shows_no_findings() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "clean-item", clean_item_json()).await;

    let host = server.uri().replace("http://", "");
    // Clean item: no findings reported to stderr, exit 0
    assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "clean-item",
        ])
        .assert()
        .success();
}

#[tokio::test]
async fn audit_detects_repeatability_violation() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "bad-date", item_with_date_array_json()).await;

    let host = server.uri().replace("http://", "");
    assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "bad-date",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("date"))
        .stderr(predicate::str::contains("not repeatable"));
}

#[tokio::test]
async fn audit_detects_missing_required() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "missing-fields", item_missing_required_json()).await;

    let host = server.uri().replace("http://", "");
    assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "missing-fields",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("collection"))
        .stderr(predicate::str::contains("missing"));
}

#[tokio::test]
async fn audit_json_output() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "bad-date", item_with_date_array_json()).await;

    let host = server.uri().replace("http://", "");
    let output = assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "bad-date",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["identifier"], "bad-date");
    assert!(!parsed["findings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn audit_csv_output() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "bad-date", item_with_date_array_json()).await;

    let host = server.uri().replace("http://", "");
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("audit.csv");

    assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "bad-date",
            "-o",
            csv_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let csv_content = std::fs::read_to_string(&csv_path).unwrap();
    assert!(csv_content.contains("identifier"));
    assert!(csv_content.contains("severity"));
    assert!(csv_content.contains("bad-date"));
    assert!(csv_content.contains("date"));
}

#[tokio::test]
async fn audit_field_filter() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;
    setup_mock(&server, "bad-date", item_with_date_array_json()).await;

    let host = server.uri().replace("http://", "");
    let output = assert_cmd::cargo_bin_cmd!("ia")
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "audit",
            "bad-date",
            "--json",
            "--field",
            "date",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    // All findings should be for the "date" field only
    for finding in parsed["findings"].as_array().unwrap() {
        assert_eq!(finding["field"], "date");
    }
}

#[tokio::test]
async fn audit_no_input_shows_help() {
    let server = MockServer::start().await;
    setup_schema_mock(&server).await;

    let host = server.uri().replace("http://", "");
    assert_cmd::cargo_bin_cmd!("ia")
        .args(["--host", &host, "--insecure", "metadata", "audit"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No input provided"));
}
