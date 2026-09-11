use assert_cmd::Command;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[allow(unused_imports)]
use serde_json::json;
use std::io::Write;
use tempfile;

fn ia_cmd() -> Command {
    assert_cmd::cargo_bin_cmd!("ia-cli")
}

#[tokio::test]
async fn test_tasks_list_json() {
    let mock_server = MockServer::start().await;

    // Provide a config file with logged-in-user cookie so the bare `ia tasks`
    // command can resolve the submitter email without a real config on disk.
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("ia.ini");
    std::fs::write(
        &config_path,
        "[s3]\naccess = dummy\nsecret = dummy\n\n[cookies]\nlogged-in-user = test%40example.com\n",
    )
    .unwrap();

    // Mock whoami (POST /services/xauthn/?op=info)
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "values": {
                "screenname": "testuser",
                "email": "test@example.com",
                "itemname": "@testuser"
            }
        })))
        .mount(&mock_server)
        .await;

    let body = [
        r#"{"category":"summary","queued":1,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":111,"identifier":"test-item","cmd":"derive.php","submitter":"test@example.com","submittime":"2026-03-10 12:00:00","server":"ia600100.us.archive.org","color":"green","priority":0}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "--json"])
        .env("IA_CONFIG_FILE", config_path.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":111"));
    assert!(stdout.contains("derive.php"));
}

#[tokio::test]
async fn test_tasks_alias() {
    let output = ia_cmd().args(["ta", "--help"]).output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("catalog tasks"));
}

#[tokio::test]
async fn test_tasks_active_completed_mutual_exclusion() {
    let output = ia_cmd()
        .args(["tasks", "--active-only", "--completed-only"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mutually exclusive"));
}

#[tokio::test]
async fn test_tasks_completed_only_requires_identifier() {
    let mock_server = MockServer::start().await;

    // Mock whoami so it doesn't fail before our validation
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "values": {
                "screenname": "testuser",
                "email": "test@example.com",
                "itemname": "@testuser"
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "--completed-only"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires an identifier"));
}

#[tokio::test]
async fn test_tasks_rate_limit_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("rate_limits", "1"))
        .and(query_param("cmd", "derive.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "cmd": "derive.php",
                "task_limits": 500,
                "tasks_inflight": 120,
                "tasks_blocked_by_offline": 0
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rate-limit", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_limits\":500"));
}

#[tokio::test]
async fn test_tasks_log_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_log", "1234567"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("Task started at: 2026-03-10\nDone.")
                .insert_header("content-type", "text/plain;charset=UTF-8"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "log",
            "1234567",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":1234567"));
    assert!(stdout.contains("Task started at"));
}

#[tokio::test]
async fn test_tasks_submit_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "task_id": 9876543,
                "log": "https://catalogd.archive.org/log/9876543"
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9876543"));
}

// ─── New comprehensive tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_tasks_list_table_output() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":1,"running":1,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":111,"identifier":"test-item","cmd":"derive.php","submitter":"user@example.com","submittime":"2026-03-10 12:00:00","server":"ia600100","color":"green","priority":0}"#,
        r#"{"category":"catalog","task_id":222,"identifier":"test-item","cmd":"fixer.php","submitter":"user@example.com","submittime":"2026-03-10 12:01:00","server":"ia600101","color":"blue","priority":0}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("identifier", "test-item"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "test-item"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Table should contain task IDs and commands
    assert!(stdout.contains("111"), "table should contain task_id 111");
    assert!(stdout.contains("222"), "table should contain task_id 222");
    assert!(
        stdout.contains("derive.php"),
        "table should contain derive.php"
    );
    assert!(
        stdout.contains("fixer.php"),
        "table should contain fixer.php"
    );
}

#[tokio::test]
async fn test_tasks_list_no_summary() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":1,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":111,"identifier":"test-item","cmd":"derive.php","color":"green"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "test-item",
            "--no-summary",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // With --no-summary --json, summary line should NOT be printed
    assert!(
        !stdout.contains("\"summary\""),
        "summary should be suppressed"
    );
    // But task entries should still appear
    assert!(
        stdout.contains("\"task_id\":111"),
        "task entry should still appear"
    );
}

#[tokio::test]
async fn test_tasks_list_limit() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":3,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":111,"identifier":"item-a","cmd":"derive.php","color":"green"}"#,
        r#"{"category":"catalog","task_id":222,"identifier":"item-b","cmd":"derive.php","color":"green"}"#,
        r#"{"category":"catalog","task_id":333,"identifier":"item-c","cmd":"derive.php","color":"green"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "test-item",
            "--limit",
            "2",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should have summary + only 2 entries (limited from 3)
    let lines: Vec<&str> = stdout.lines().collect();
    // summary line + 2 task lines = 3 lines
    assert_eq!(
        lines.len(),
        3,
        "expected 3 lines (summary + 2 tasks), got {}: {:?}",
        lines.len(),
        lines
    );
}

#[tokio::test]
async fn test_tasks_list_active_only() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("catalog", "1"))
        .and(query_param("history", "0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
                )
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "test-item",
            "--active-only",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
}

#[tokio::test]
async fn test_tasks_submit_batch_itemlist() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 999, "log": "https://catalogd.archive.org/log/999" }
        })))
        .expect(2) // Should be called twice (2 items)
        .mount(&mock_server)
        .await;

    // Create temp itemlist file
    let dir = tempfile::tempdir().unwrap();
    let itemlist_path = dir.path().join("items.txt");
    std::fs::write(&itemlist_path, "item-one\nitem-two\n").unwrap();

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--itemlist",
            itemlist_path.to_str().unwrap(),
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should have 2 success lines
    let success_count = stdout
        .lines()
        .filter(|l| l.contains("\"success\":true"))
        .count();
    assert_eq!(
        success_count, 2,
        "expected 2 success lines, got stdout: {stdout}"
    );
}

#[tokio::test]
async fn test_tasks_rerun_multiple_ids() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "111": "item-a" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rerun",
            "111",
            "222",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"task_id\":111"),
        "should contain task_id 111"
    );
    assert!(
        stdout.contains("\"task_id\":222"),
        "should contain task_id 222"
    );
}

#[tokio::test]
async fn test_tasks_rerun_query_filters() {
    let mock_server = MockServer::start().await;

    // First: list_tasks returns matching tasks (rerun queries with color=red default)
    let body = [
        r#"{"category":"summary","queued":0,"running":0,"error":2,"paused":0}"#,
        r#"{"category":"catalog","task_id":555,"identifier":"item-x","cmd":"derive.php","color":"red"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("cmd", "derive.php"))
        .and(query_param("color", "red"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    // Then: rerun_task PUT
    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "555": "item-x" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rerun",
            "--cmd",
            "derive.php",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"task_id\":555"),
        "should contain task_id 555"
    );
    assert!(
        stdout.contains("\"success\":true"),
        "should indicate success"
    );
}

#[tokio::test]
async fn test_tasks_log_not_found() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_log", "9999999"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "log", "9999999"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("task"),
        "stderr should mention task not found, got: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_rate_limit_human_output() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("rate_limits", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "cmd": "derive.php",
                "task_limits": 500,
                "tasks_inflight": 42,
                "tasks_blocked_by_offline": 0
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rate-limit"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Command:"),
        "should have Command: label, got: {stdout}"
    );
    assert!(stdout.contains("derive.php"), "should show command name");
    assert!(stdout.contains("500"), "should show task limit");
    assert!(stdout.contains("42"), "should show inflight count");
}

#[tokio::test]
async fn test_tasks_submit_wait() {
    let mock_server = MockServer::start().await;

    // Submit returns task_id
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 1111, "log": "https://catalogd.archive.org/log/1111" }
        })))
        .mount(&mock_server)
        .await;

    // Wait poll: task is no longer in catalog (completed)
    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "1111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "summary": {"queued": 0, "running": 0, "error": 0, "paused": 0},
                "catalog": []
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "--wait",
            "--wait-interval",
            "1",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line: submit response, second line: completed status
    assert!(stdout.contains("1111"), "should contain task_id 1111");
    assert!(
        stdout.contains("\"status\":\"completed\""),
        "should show completed status"
    );
}

#[tokio::test]
async fn test_tasks_rerun_stdin_raw_ids() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "111": "item-a" }
        })))
        .mount(&mock_server)
        .await;

    // Create a temp file with task IDs to pipe via stdin
    let dir = tempfile::tempdir().unwrap();
    let ids_path = dir.path().join("ids.txt");
    {
        let mut f = std::fs::File::create(&ids_path).unwrap();
        writeln!(f, "111").unwrap();
        writeln!(f, "# comment line").unwrap();
        writeln!(f, "222").unwrap();
        writeln!(f, "").unwrap(); // blank line
    }

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rerun", "-", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .pipe_stdin(&ids_path)
        .unwrap()
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"task_id\":111"),
        "should contain task_id 111, got: {stdout}"
    );
    assert!(
        stdout.contains("\"task_id\":222"),
        "should contain task_id 222, got: {stdout}"
    );
}

#[tokio::test]
async fn test_tasks_rerun_stdin_jsonl() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "333": "item-c" }
        })))
        .mount(&mock_server)
        .await;

    // Create a temp file with JSONL task entries (like `ia tasks --json` output)
    let dir = tempfile::tempdir().unwrap();
    let jsonl_path = dir.path().join("tasks.jsonl");
    {
        let mut f = std::fs::File::create(&jsonl_path).unwrap();
        writeln!(
            f,
            r#"{{"task_id":333,"identifier":"item-c","cmd":"derive.php","color":"red"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"task_id":444,"identifier":"item-d","cmd":"derive.php","color":"red"}}"#
        )
        .unwrap();
    }

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rerun", "-", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .pipe_stdin(&jsonl_path)
        .unwrap()
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"task_id\":333"),
        "should contain task_id 333, got: {stdout}"
    );
    assert!(
        stdout.contains("\"task_id\":444"),
        "should contain task_id 444, got: {stdout}"
    );
}

#[tokio::test]
async fn test_tasks_submit_rate_limited_retry() {
    let mock_server = MockServer::start().await;

    // First call: 429 with Retry-After header (reqwest-middleware handles this)
    // Second call: success
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_string("rate limited")
                .insert_header("Retry-After", "1"),
        )
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 5555, "log": "https://catalogd.archive.org/log/5555" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "should succeed after middleware retry, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("5555"),
        "should contain task_id after retry"
    );
}

#[tokio::test]
async fn test_tasks_list_with_task_id_flag() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":1,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":12345,"identifier":"test-item","cmd":"derive.php","color":"blue"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "12345"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "--task-id",
            "12345",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":12345"));
}

#[tokio::test]
async fn test_tasks_list_with_date_range_flags() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("submittime>=", "2026-03-01"))
        .and(query_param("submittime<=", "2026-03-10"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
                )
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "test-item",
            "--since",
            "2026-03-01",
            "--before",
            "2026-03-10",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
}

#[tokio::test]
async fn test_tasks_submit_malformed_args() {
    let output = ia_cmd()
        .args([
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "--args",
            "remove_derived",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("KEY=VALUE"),
        "should explain expected format, got: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_submit_wait_with_batch_errors() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 1, "log": "https://catalogd.archive.org/log/1" }
        })))
        .mount(&mock_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let itemlist_path = dir.path().join("items.txt");
    std::fs::write(&itemlist_path, "item-one\nitem-two\n").unwrap();

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--itemlist",
            itemlist_path.to_str().unwrap(),
            "--wait",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not supported in batch mode"),
        "should reject --wait in batch mode, got: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_submit_with_args_and_comment() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 4444, "log": "https://catalogd.archive.org/log/4444" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "my-item",
            "--cmd",
            "derive",
            "--args",
            "remove_derived=*.jpg",
            "--comment",
            "re-derive without JPEGs",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4444"));
}

#[tokio::test]
async fn test_tasks_rerun_with_identifier_filter() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":0,"error":1,"paused":0}"#,
        r#"{"category":"catalog","task_id":777,"identifier":"target-item","cmd":"derive.php","color":"red"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("identifier", "target-item"))
        .and(query_param("color", "red"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "777": "target-item" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rerun",
            "--identifier",
            "target-item",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":777"));
    assert!(stdout.contains("\"success\":true"));
}

#[tokio::test]
async fn test_tasks_rate_limit_custom_cmd() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("rate_limits", "1"))
        .and(query_param("cmd", "make_dark.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "cmd": "make_dark.php",
                "task_limits": 100,
                "tasks_inflight": 5,
                "tasks_blocked_by_offline": 0
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rate-limit",
            "make_dark",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"cmd\":\"make_dark.php\""));
    assert!(stdout.contains("\"task_limits\":100"));
}

#[tokio::test]
async fn test_tasks_completed_only_with_task_id() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"history","task_id":99999,"identifier":"done-item","cmd":"derive.php","color":"done","finished":85575266}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "99999"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "--completed-only",
            "--task-id",
            "99999",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "--completed-only with --task-id should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn test_tasks_submit_spreadsheet_nonexistent() {
    let output = ia_cmd()
        .args(["tasks", "submit", "--spreadsheet", "/nonexistent/jobs.csv"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read spreadsheet"),
        "should report file error, got: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_rerun_auto_stdin() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "888": "item-z" }
        })))
        .mount(&mock_server)
        .await;

    // Create a temp file with task IDs to pipe via stdin
    let dir = tempfile::tempdir().unwrap();
    let ids_path = dir.path().join("ids.txt");
    {
        let mut f = std::fs::File::create(&ids_path).unwrap();
        writeln!(f, "888").unwrap();
        writeln!(f, "999").unwrap();
    }

    let host = mock_server.uri().replace("http://", "");
    // No explicit `-` — stdin is auto-detected
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rerun", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .pipe_stdin(&ids_path)
        .unwrap()
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("\"task_id\":888"),
        "should contain task_id 888, stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"task_id\":999"),
        "should contain task_id 999, stdout: {stdout}"
    );
    // Should NOT contain deprecation warning (no `-` was used)
    assert!(
        !stderr.contains("deprecated"),
        "should not show deprecation warning without explicit -, stderr: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_rerun_deprecated_dash_warns() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "111": "item-a" }
        })))
        .mount(&mock_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let ids_path = dir.path().join("ids.txt");
    {
        let mut f = std::fs::File::create(&ids_path).unwrap();
        writeln!(f, "111").unwrap();
    }

    let host = mock_server.uri().replace("http://", "");
    // Explicit `-` should show deprecation warning
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rerun", "-", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .pipe_stdin(&ids_path)
        .unwrap()
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated"),
        "should show deprecation warning for explicit '-', stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"task_id\":111"),
        "should still work, stdout: {stdout}"
    );
}

#[tokio::test]
async fn test_tasks_submit_cmd_required_without_spreadsheet() {
    // When no positional args and no --spreadsheet, should error about missing cmd
    let output = ia_cmd()
        .args(["tasks", "submit", "--itemlist", "/dev/null"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--cmd") || stderr.contains("required"),
        "should explain --cmd is required, got: {stderr}"
    );
}

#[tokio::test]
async fn test_tasks_submit_custom_cmd_with_itemlist() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 1234, "log": "https://catalogd.archive.org/log/1234" }
        })))
        .expect(2)
        .mount(&mock_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let itemlist_path = dir.path().join("items.txt");
    std::fs::write(&itemlist_path, "item-one\nitem-two\n").unwrap();

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "--cmd",
            "reduce_item",
            "--itemlist",
            itemlist_path.to_str().unwrap(),
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "custom command with --itemlist should work, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let success_count = stdout
        .lines()
        .filter(|l| l.contains("\"success\":true"))
        .count();
    assert_eq!(
        success_count, 2,
        "expected 2 successes, got stdout: {stdout}"
    );
}
