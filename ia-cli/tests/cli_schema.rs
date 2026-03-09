use assert_cmd::Command;
use predicates::prelude::*;
use serde_json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_schema_json() -> &'static str {
    r#"{
        "metadata_schema": [
            {
                "field": "title",
                "label": "Title",
                "required": "Recommended",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Title of media",
                "accepted values": "String, plain text",
                "usage notes": "All alphabets supported",
                "example": ["San Francisco (1955)"]
            },
            {
                "field": "scanner",
                "label": "Scanner",
                "required": "No",
                "repeatable": "No",
                "internal use only": "Yes",
                "defined by": "IA software",
                "edit access": "IA admin",
                "definition": "Scanner used to digitize",
                "accepted values": "String"
            }
        ],
        "files_schema": [
            {
                "field": "name",
                "label": "File Name",
                "required": "Yes",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "not editable",
                "definition": "Name of the file"
            }
        ]
    }"#
}

#[tokio::test]
async fn schema_table_hides_internal_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema"])
        .assert()
        .success()
        .stdout(predicate::str::contains("title"))
        .stdout(predicate::str::contains("Title"))
        .stdout(predicate::str::contains("scanner").not());
}

#[tokio::test]
async fn schema_table_shows_internal_with_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "--internal",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("title"))
        .stdout(predicate::str::contains("scanner"));
}

#[tokio::test]
async fn schema_detail_shows_all_properties() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "title"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("title"));
    assert!(stdout.contains("Label:"));
    assert!(stdout.contains("Title"));
    assert!(stdout.contains("Required:"));
    assert!(stdout.contains("Recommended"));
    assert!(stdout.contains("Definition:"));
    assert!(stdout.contains("Example:"));
}

#[tokio::test]
async fn schema_detail_unknown_field_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "nonexistent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[tokio::test]
async fn schema_detail_json_outputs_single_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "title",
            "--json",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["field"], "title");
    assert!(v.is_object(), "single field should be an object, not array");
}

#[tokio::test]
async fn schema_required_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "--required",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("title")); // Recommended counts as required
    // scanner is "No" required AND internal — should not appear
}

#[tokio::test]
async fn schema_files_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "--files",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("name"));
    assert!(stdout.contains("File Name"));
    // Should not contain metadata-only fields
    assert!(!stdout.contains("title"));
}

#[tokio::test]
async fn schema_json_outputs_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "--json",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(v.is_array(), "listing mode should output JSON array");
}

#[tokio::test]
async fn schema_defined_by_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host",
            &host,
            "--insecure",
            "metadata",
            "schema",
            "--internal",
            "--defined-by",
            "ia-software",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("scanner")); // defined by IA software
    assert!(!stdout.contains("title")); // defined by uploader
}
