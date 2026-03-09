use assert_cmd::Command;
use predicates::prelude::*;
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
