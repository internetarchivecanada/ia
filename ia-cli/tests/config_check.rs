//! `ia config check` and `ia config whoami` against a mocked
//! `GET /services/user.php?op=whoami`.

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_whoami(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/services/user.php"))
        .and(query_param("op", "whoami"))
        .respond_with(response)
        .mount(server)
        .await;
}

fn valid_answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "success": true,
        "value": {
            "username": "user@example.com",
            "itemname": "@testuser",
            "screenname": "testuser",
            "privs": []
        }
    }))
}

fn rejected_answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "success": false,
        "error": "Invalid S3 key"
    }))
}

/// Run `ia config <args>` with a config file that points at the mock server.
/// The `config` subcommand ignores -H/--insecure, so the host is set in the file.
fn run(server: &MockServer, args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let host = server.uri().replace("http://", "");
    std::fs::write(
        &ini_path,
        format!(
            "[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = myaccess\nsecret = mysecret\n\n\
             [cookies]\nlogged-in-user = cookie%40example.com\nlogged-in-sig = sig\n"
        ),
    )
    .unwrap();
    let mut full = vec!["--config-file", ini_path.to_str().unwrap(), "config"];
    full.extend_from_slice(args);
    assert_cmd::cargo_bin_cmd!("ia-cli")
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .args(&full)
        .output()
        .unwrap()
}

#[tokio::test]
async fn check_valid_key_without_privileges() {
    let server = MockServer::start().await;
    mount_whoami(&server, valid_answer()).await;

    let output = run(&server, &["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Credentials valid (testuser)"), "{stderr}");
}

#[tokio::test]
async fn check_valid_key_json() {
    let server = MockServer::start().await;
    mount_whoami(&server, valid_answer()).await;

    let output = run(&server, &["check", "--json"]);
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "valid": true,
            "screenname": "testuser",
            "email": "user@example.com",
            "itemname": "@testuser"
        })
    );
}

#[tokio::test]
async fn check_bad_key_exits_1_with_message() {
    let server = MockServer::start().await;
    mount_whoami(&server, rejected_answer()).await;

    let output = run(&server, &["check"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected"), "{stderr}");
    assert!(stderr.contains("Invalid S3 key"), "{stderr}");
}

#[tokio::test]
async fn check_bad_key_json_exits_1() {
    let server = MockServer::start().await;
    mount_whoami(&server, rejected_answer()).await;

    let output = run(&server, &["check", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["valid"], false);
    assert!(
        json["error"].as_str().unwrap().contains("Invalid S3 key"),
        "{json}"
    );
}

#[tokio::test]
async fn check_non_json_answer_exits_1() {
    let server = MockServer::start().await;
    mount_whoami(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>Service unavailable</html>"),
    )
    .await;

    let output = run(&server, &["check"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected key check answer"), "{stderr}");
}

#[tokio::test]
async fn check_non_json_answer_json_exits_1() {
    let server = MockServer::start().await;
    mount_whoami(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>Service unavailable</html>"),
    )
    .await;

    let output = run(&server, &["check", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["valid"], false);
}

#[tokio::test]
async fn whoami_shows_server_account() {
    let server = MockServer::start().await;
    mount_whoami(&server, valid_answer()).await;

    let output = run(&server, &["whoami"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("testuser"), "{stdout}");
    assert!(stdout.contains("user@example.com"), "{stdout}");
    assert!(stdout.contains("@testuser"), "{stdout}");
    assert!(!stdout.contains("cookie"), "{stdout}");
}

#[tokio::test]
async fn whoami_json() {
    let server = MockServer::start().await;
    mount_whoami(&server, valid_answer()).await;

    let output = run(&server, &["whoami", "--json"]);
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "screenname": "testuser",
            "email": "user@example.com",
            "itemname": "@testuser"
        })
    );
}

#[tokio::test]
async fn whoami_bad_key_exits_1() {
    let server = MockServer::start().await;
    mount_whoami(&server, rejected_answer()).await;

    let output = run(&server, &["whoami"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Invalid S3 key"), "{stderr}");
}
