//! `ia tasks submit`: joblog records, the resume key, and `--wait` with
//! `--spreadsheet` (issues #186, #187, #190). Every request goes to wiremock;
//! submitting a task is a write, so nothing here may reach archive.org.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ia_cmd() -> assert_cmd::Command {
    assert_cmd::cargo_bin_cmd!("ia-cli")
}

fn submit_ok(task_id: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "success": true,
        "value": { "task_id": task_id, "log": format!("https://catalogd.archive.org/log/{task_id}") }
    }))
}

/// Number of task submissions the mock received with this `cmd` (and this
/// identifier, when given).
async fn posts(server: &MockServer, cmd: &str, identifier: Option<&str>) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .filter(|b| b["cmd"] == cmd)
        .filter(|b| identifier.is_none_or(|id| b["identifier"] == id))
        .count()
}

async fn polls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.query().unwrap_or("").contains("task_id="))
        .count()
}

fn run(host: &str, joblog: &Path, args: &[&str]) -> std::process::Output {
    let mut all = vec![
        "--insecure",
        "-H",
        host,
        "--joblog",
        joblog.to_str().unwrap(),
    ];
    all.extend_from_slice(args);
    ia_cmd()
        .args(&all)
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap()
}

fn joblog_text(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// #186 case 1: one identifier, run twice with the same joblog. The first run
/// records the submission, so the second sends nothing and exits 0.
#[tokio::test]
async fn tasks_single_item_writes_joblog_and_rerun_skips_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(7))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("items.txt");
    std::fs::write(&items, "item-a\n").unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");
    let args = [
        "tasks",
        "submit",
        "--cmd",
        "derive",
        "--itemlist",
        items.to_str().unwrap(),
    ];

    let first = run(&host, &log, &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run(&host, &log, &args);
    let stderr = String::from_utf8_lossy(&second.stderr);

    assert_eq!(
        posts(&server, "derive.php", Some("item-a")).await,
        1,
        "derive submitted more than once across two runs; joblog: {:?}",
        joblog_text(&log)
    );
    assert!(
        second.status.success(),
        "rerun with nothing left must exit 0: {stderr}"
    );
    assert!(
        stderr.contains("resuming:"),
        "rerun should say it resumed: {stderr}"
    );

    // The positional single-item form records too.
    let log2 = dir.path().join("log2.jsonl");
    let out = run(
        &host,
        &log2,
        &["tasks", "submit", "item-b", "--cmd", "derive"],
    );
    assert!(out.status.success());
    let text = joblog_text(&log2);
    assert!(
        text.contains("\"item\":\"item-b\"") && text.contains("\"status\":\"ok\""),
        "single positional submission not recorded: {text:?}"
    );
}

/// #186 case 2: a batch where one item is answered at once and the other
/// hangs. The answered item is in the joblog before the batch ends.
#[tokio::test]
async fn tasks_batch_joblog_written_as_items_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_partial_json(json!({"identifier": "item-fast"})))
        .respond_with(submit_ok(1))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_partial_json(json!({"identifier": "item-slow"})))
        .respond_with(submit_ok(2).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("items.txt");
    std::fs::write(&items, "item-fast\nitem-slow\n").unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ia-cli"))
        .args([
            "--insecure",
            "-H",
            &host,
            "--joblog",
            log.to_str().unwrap(),
            "-j",
            "2",
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--itemlist",
            items.to_str().unwrap(),
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Wait until both submissions have been sent, then give the fast one's
    // answer time to be handled.
    let start = Instant::now();
    while posts(&server, "derive.php", None).await < 2 && start.elapsed() < Duration::from_secs(20)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !joblog_text(&log).contains("item-fast") && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let text = joblog_text(&log);
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        text.contains("\"item\":\"item-fast\"") && text.contains("\"status\":\"ok\""),
        "item-fast was submitted (task 1) but the joblog has: {text:?}"
    );
    assert!(
        !text.contains("item-slow"),
        "item-slow has no answer yet: {text:?}"
    );
}

/// #187: a failed `make_dark` row is retried even though the `derive` row for
/// the same item succeeded; the `derive` row is not sent again.
#[tokio::test]
async fn tasks_spreadsheet_resume_retries_failed_row_same_item() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_partial_json(json!({"cmd": "make_dark.php"})))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(9))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\nitem-a,make_dark\n").unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");
    let args = ["tasks", "submit", "--spreadsheet", sheet.to_str().unwrap()];

    let first = run(&host, &log, &args);
    assert!(!first.status.success(), "the 500 must fail the first run");
    let second = run(&host, &log, &args);
    let stderr = String::from_utf8_lossy(&second.stderr);

    assert_eq!(
        posts(&server, "make_dark.php", None).await,
        2,
        "make_dark not retried; rerun stderr: {stderr}"
    );
    assert_eq!(
        posts(&server, "derive.php", None).await,
        1,
        "derive sent again: {stderr}"
    );
    assert!(second.status.success(), "rerun stderr: {stderr}");
}

/// #187, identifier mode: a joblog from a `--cmd derive` run does not make a
/// later `--cmd make_dark` run skip the item. Different task arguments are a
/// different task too.
#[tokio::test]
async fn tasks_resume_matches_command_and_args() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(3))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("items.txt");
    std::fs::write(&items, "item-a\nitem-b\n").unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");
    let il = items.to_str().unwrap();

    assert!(run(
        &host,
        &log,
        &["tasks", "submit", "--cmd", "derive", "--itemlist", il]
    )
    .status
    .success());
    let out = run(
        &host,
        &log,
        &["tasks", "submit", "--cmd", "make_dark", "--itemlist", il],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(posts(&server, "make_dark.php", None).await, 2);

    // Same command with arguments: a new task.
    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--args",
            "remove_derived=*.jpg",
            "--itemlist",
            il,
        ],
    );
    assert!(out.status.success());
    assert_eq!(posts(&server, "derive.php", None).await, 4);

    // The same command and arguments again, with `derive` spelled with
    // `.php` and a new comment: already done, nothing sent.
    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "--cmd",
            "derive.php",
            "--args",
            "remove_derived=*.jpg",
            "--comment",
            "again",
            "--itemlist",
            il,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(posts(&server, "derive.php", None).await, 4);
}

/// A joblog written before the task was recorded (empty `file`) still skips
/// the item for every command, so an older log never causes a resubmission.
#[tokio::test]
async fn tasks_resume_honours_old_entries_without_task() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(5))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("items.txt");
    std::fs::write(&items, "item-old\nitem-new\n").unwrap();
    let log = dir.path().join("log.jsonl");
    std::fs::write(
        &log,
        "{\"ts\":\"2026-11-01T00:00:00Z\",\"op\":\"task-submit\",\"item\":\"item-old\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":0}\n",
    )
    .unwrap();
    let host = server.uri().replace("http://", "");

    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "--cmd",
            "make_dark",
            "--itemlist",
            items.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(posts(&server, "make_dark.php", Some("item-old")).await, 0);
    assert_eq!(posts(&server, "make_dark.php", Some("item-new")).await, 1);
}

fn mount_poll(task_id: u64, color: Option<&str>) -> Mock {
    let catalog = match color {
        Some(c) => {
            json!([{"task_id": task_id, "identifier": "item-a", "cmd": "derive.php", "color": c}])
        }
        None => json!([]),
    };
    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", task_id.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "summary": {"queued": 0, "running": 0, "error": 0, "paused": 0},
                "catalog": catalog
            }
        })))
}

/// #190: `--wait` with a one-row spreadsheet polls the task; the exit code
/// follows the task's outcome.
#[tokio::test]
async fn tasks_spreadsheet_wait_polls() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\n").unwrap();
    let sheet = sheet.to_str().unwrap();
    let args = [
        "tasks",
        "submit",
        "--spreadsheet",
        sheet,
        "--wait",
        "--wait-interval",
        "1",
    ];

    // Completed: polled, exit 0.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(4242))
        .mount(&server)
        .await;
    mount_poll(4242, None).mount(&server).await;
    let host = server.uri().replace("http://", "");
    let out = run(&host, &dir.path().join("a.jsonl"), &args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        polls(&server).await >= 1,
        "--wait sent no poll; exit {:?}; stderr: {stderr}",
        out.status.code()
    );
    assert!(out.status.success(), "{stderr}");

    // Errored: exit 1 naming the task.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(4243))
        .mount(&server)
        .await;
    mount_poll(4243, Some("red")).mount(&server).await;
    let host = server.uri().replace("http://", "");
    let out = run(&host, &dir.path().join("b.jsonl"), &args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "an errored task must exit 1: {stderr}"
    );
    assert!(stderr.contains("task 4243 failed"), "{stderr}");
}

/// #190: with more than one row to submit, `--wait` is refused before any
/// request, as in identifier batch mode.
#[tokio::test]
async fn tasks_spreadsheet_wait_refused_for_several_rows() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(1))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\nitem-b,derive\n").unwrap();
    let host = server.uri().replace("http://", "");
    let out = run(
        &host,
        &dir.path().join("log.jsonl"),
        &[
            "tasks",
            "submit",
            "--spreadsheet",
            sheet.to_str().unwrap(),
            "--wait",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("not supported in batch mode"), "{stderr}");
    assert_eq!(posts(&server, "derive.php", None).await, 0);
}

/// #190: `-p` values reach the spreadsheet submissions.
#[tokio::test]
async fn tasks_spreadsheet_sends_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_partial_json(
            json!({"cmd": "derive.php", "foo": "bar"}),
        ))
        .respond_with(submit_ok(1))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\n").unwrap();
    let host = server.uri().replace("http://", "");
    let out = run(
        &host,
        &dir.path().join("log.jsonl"),
        &[
            "tasks",
            "submit",
            "--spreadsheet",
            sheet.to_str().unwrap(),
            "-p",
            "foo=bar",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The resume key sorts task arguments: the same arguments given in another
/// order are the same task.
#[tokio::test]
async fn tasks_resume_key_ignores_argument_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(6))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");

    let first = [
        "tasks", "submit", "item-a", "--cmd", "derive", "--args", "a=1", "--args", "b=2", "--args",
        "c=3", "--args", "d=4",
    ];
    let second = [
        "tasks", "submit", "item-a", "--cmd", "derive", "--args", "d=4", "--args", "c=3", "--args",
        "b=2", "--args", "a=1",
    ];
    assert!(run(&host, &log, &first).status.success());
    let out = run(&host, &log, &second);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        posts(&server, "derive.php", None).await,
        1,
        "joblog: {}",
        joblog_text(&log)
    );
    assert!(
        joblog_text(&log)
            .contains(r#""file":"derive.php {\"a\":\"1\",\"b\":\"2\",\"c\":\"3\",\"d\":\"4\"}""#),
        "joblog: {}",
        joblog_text(&log)
    );
}

/// `--dry-run --json` keeps `"args": {}` for a task without arguments.
#[test]
fn tasks_dry_run_json_args_is_an_object() {
    let out = ia_cmd()
        .args([
            "tasks",
            "submit",
            "item-a",
            "--cmd",
            "derive",
            "--dry-run",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();
    assert!(out.status.success());
    let line: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(line["args"], json!({}));
}

/// A two-row spreadsheet whose resume leaves one row accepts `--wait` and
/// polls that task.
#[tokio::test]
async fn tasks_spreadsheet_wait_after_resume_leaves_one_row() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(77))
        .mount(&server)
        .await;
    mount_poll(77, None).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\nitem-b,derive\n").unwrap();
    let log = dir.path().join("log.jsonl");
    std::fs::write(
        &log,
        "{\"ts\":\"2026-11-24T00:00:00Z\",\"op\":\"task-submit\",\"item\":\"item-a\",\"file\":\"derive.php\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":1}\n",
    )
    .unwrap();
    let host = server.uri().replace("http://", "");
    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "--spreadsheet",
            sheet.to_str().unwrap(),
            "--wait",
            "--wait-interval",
            "1",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(posts(&server, "derive.php", Some("item-a")).await, 0);
    assert_eq!(posts(&server, "derive.php", Some("item-b")).await, 1);
    assert!(polls(&server).await >= 1);
}

/// `--wait` when the joblog records every task: nothing to poll, so exit 1
/// rather than report a completion it has not seen. Without `--wait`, exit 0.
#[tokio::test]
async fn tasks_wait_with_everything_recorded_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(8))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");
    let args = ["tasks", "submit", "item-a", "--cmd", "derive"];
    assert!(run(&host, &log, &args).status.success());

    let out = run(
        &host,
        &log,
        &["tasks", "submit", "item-a", "--cmd", "derive", "--wait"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("nothing to poll"), "{stderr}");
    assert_eq!(posts(&server, "derive.php", None).await, 1);
    assert_eq!(polls(&server).await, 0);
}

/// Two identical spreadsheet rows, one refused: the rerun submits the one
/// that failed. Each `ok` entry in the joblog covers one matching row, not
/// every row with that task.
#[tokio::test]
async fn tasks_spreadsheet_identical_rows_retry_the_failed_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(11))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("jobs.csv");
    std::fs::write(&sheet, "identifier,cmd\nitem-a,derive\nitem-a,derive\n").unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");
    let args = [
        "tasks",
        "submit",
        "-j",
        "1",
        "--spreadsheet",
        sheet.to_str().unwrap(),
    ];

    let first = run(&host, &log, &args);
    assert!(!first.status.success(), "one row was refused");
    assert_eq!(posts(&server, "derive.php", None).await, 2);

    let second = run(&host, &log, &args);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert_eq!(
        posts(&server, "derive.php", None).await,
        3,
        "the failed row was not retried; rerun stderr: {stderr}"
    );
    assert!(second.status.success(), "{stderr}");
    assert!(stderr.contains("skipping 1 already-submitted"), "{stderr}");

    // A third run: both rows are recorded now, nothing is sent.
    let third = run(&host, &log, &args);
    assert!(third.status.success());
    assert_eq!(posts(&server, "derive.php", None).await, 3);
}

/// `--args comment=x` and `--comment x` send the same field (`args.comment`),
/// so neither is part of the task key: rewording the comment either way does
/// not submit the task again.
#[tokio::test]
async fn tasks_resume_key_leaves_out_comment_arg() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(12))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");

    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "item-a",
            "--cmd",
            "derive",
            "--args",
            "comment=first",
        ],
    );
    assert!(out.status.success());
    let out = run(
        &host,
        &log,
        &[
            "tasks",
            "submit",
            "item-a",
            "--cmd",
            "derive",
            "--comment",
            "second",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        posts(&server, "derive.php", None).await,
        1,
        "joblog: {}",
        joblog_text(&log)
    );
    assert!(
        joblog_text(&log).contains("\"file\":\"derive.php\""),
        "joblog: {}",
        joblog_text(&log)
    );
}

/// A spreadsheet `args.comment` column is left out of the key, and the other
/// arguments stay in it.
#[tokio::test]
async fn tasks_resume_key_drops_comment_column_keeps_other_args() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(submit_ok(13))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log.jsonl");
    let host = server.uri().replace("http://", "");

    let sheet = dir.path().join("jobs.csv");
    std::fs::write(
        &sheet,
        "identifier,cmd,args.comment,args.remove_derived\nitem-a,derive,first,*.jpg\n",
    )
    .unwrap();
    let out = run(
        &host,
        &log,
        &["tasks", "submit", "--spreadsheet", sheet.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        joblog_text(&log).contains(r#""file":"derive.php {\"remove_derived\":\"*.jpg\"}""#),
        "joblog: {}",
        joblog_text(&log)
    );

    std::fs::write(
        &sheet,
        "identifier,cmd,args.comment,args.remove_derived\nitem-a,derive,second,*.jpg\n",
    )
    .unwrap();
    let out = run(
        &host,
        &log,
        &["tasks", "submit", "--spreadsheet", sheet.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(posts(&server, "derive.php", None).await, 1);
}
