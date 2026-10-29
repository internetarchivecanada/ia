use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn config_login_missing_password_flag_without_tty() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");

    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args([
        "--config-file",
        ini_path.to_str().unwrap(),
        "config",
        "login",
        "-u",
        "user@example.com",
    ]);
    cmd.assert().failure();
}

fn user_agent_of(request: &wiremock::Request) -> String {
    request
        .headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn config_login_user_agent_has_no_access_key_even_if_config_has_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "values": {
                "s3": {"access": "newaccess", "secret": "newsecret"},
                "cookies": {"logged-in-user": "u@example.com", "logged-in-sig": "sig"},
                "screenname": "tester"
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    // The `config` subcommand ignores -H/--insecure, so the mock server is
    // selected through the config file.
    let host = server.uri().replace("http://", "");
    std::fs::write(
        &ini_path,
        format!("[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = oldaccess\nsecret = oldsecret\n"),
    )
    .unwrap();
    let output = assert_cmd::cargo_bin_cmd!("ia-cli")
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .args([
            "--config-file",
            ini_path.to_str().unwrap(),
            "config",
            "login",
            "-u",
            "u@example.com",
            "-p",
            "pw",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let ua = user_agent_of(&requests[0]);
    assert!(ua.starts_with("ia/"), "unexpected User-Agent: {ua}");
    assert!(
        !ua.contains("oldaccess"),
        "login request must not carry the access key, got: {ua}"
    );
}

#[tokio::test]
async fn config_check_user_agent_carries_configured_access_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "values": {"screenname": "tester", "itemname": "@tester"}
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    // The `config` subcommand ignores -H/--insecure, so the mock server is
    // selected through the config file.
    let host = server.uri().replace("http://", "");
    std::fs::write(
        &ini_path,
        format!("[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = myaccess\nsecret = mysecret\n"),
    )
    .unwrap();
    let output = assert_cmd::cargo_bin_cmd!("ia-cli")
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .args([
            "--config-file",
            ini_path.to_str().unwrap(),
            "config",
            "check",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let ua = user_agent_of(&requests[0]);
    assert!(
        ua.contains("; myaccess) Rust/"),
        "User-Agent must carry the access key, got: {ua}"
    );
}
