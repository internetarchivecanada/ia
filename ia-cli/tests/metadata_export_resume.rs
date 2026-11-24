//! `ia metadata export` into a CSV file keeps every row the joblog marks
//! done across an interruption (#188), and exits 1 when an item cannot be
//! fetched (#189). Every request goes to a local wiremock server; the
//! config file points the default host at a closed local port.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn closed_host_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "[general]\nhost = 127.0.0.1:1\n").unwrap();
    f
}

fn item_body(id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({"metadata": {"identifier": id, "title": title}, "files": []})
}

/// Mount `/metadata/<id>` answering `body` after `delay`.
async fn mount(server: &MockServer, id: &str, body: serde_json::Value, delay: Duration) {
    Mock::given(method("GET"))
        .and(path(format!("/metadata/{id}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(body)
                .set_delay(delay),
        )
        .mount(server)
        .await;
}

struct Export {
    dir: TempDir,
    cfg: NamedTempFile,
}

impl Export {
    fn new(ids: &str) -> Self {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("ids.txt"), ids).unwrap();
        Self {
            dir,
            cfg: closed_host_config(),
        }
    }
    fn out(&self) -> PathBuf {
        self.dir.path().join("out.csv")
    }
    fn joblog(&self) -> PathBuf {
        self.dir.path().join("export.jsonl")
    }
    fn command(&self, server: &MockServer) -> Command {
        let mut cmd = self.command_to_stdout(server);
        cmd.arg("-o").arg(self.out());
        cmd
    }
    /// The export with no `-o`: JSONL on stdout.
    fn command_to_stdout(&self, server: &MockServer) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ia-cli"));
        cmd.env(
            "IA_CACHE_DIR",
            concat!(env!("CARGO_TARGET_TMPDIR"), "/ia-cache"),
        )
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .arg("--config-file")
        .arg(self.cfg.path())
        .args(["--insecure", "-H", &server.uri().replace("http://", "")])
        .arg("--joblog")
        .arg(self.joblog())
        .args(["metadata", "export", "--itemlist"])
        .arg(self.dir.path().join("ids.txt"));
        cmd
    }
    fn spawn(&self, server: &MockServer) -> Child {
        self.command(server)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
    fn run(&self, server: &MockServer) -> std::process::Output {
        self.command(server).output().unwrap()
    }
}

fn joblog_marks_ok(joblog: &Path, id: &str) -> bool {
    fs::read_to_string(joblog)
        .map(|s| {
            s.lines().any(|l| {
                l.contains("\"status\":\"ok\"") && l.contains(&format!("\"item\":\"{id}\""))
            })
        })
        .unwrap_or(false)
}

async fn requested(server: &MockServer, id: &str) -> bool {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .any(|r| r.url.path() == format!("/metadata/{id}"))
}

fn signal(child: &Child, sig: libc::c_int) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: kill(2) on our own child process.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
}

/// The data lines of the CSV, sorted: `identifier,title`.
fn rows(out: &Path) -> Vec<String> {
    let content = fs::read_to_string(out).unwrap();
    let mut lines = content.lines();
    assert_eq!(lines.next(), Some("identifier,title"), "{content}");
    let mut rows: Vec<String> = lines.map(str::to_string).collect();
    rows.sort();
    rows
}

/// Ctrl+C after `item-a` is fetched, while `item-b` is still in flight, then
/// the same command again: the file holds both rows, once each.
#[tokio::test]
async fn export_csv_resume_after_interrupt_keeps_rows() {
    let export = Export::new("item-a\nitem-b\n");

    let first = MockServer::start().await;
    mount(&first, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    mount(
        &first,
        "item-b",
        item_body("item-b", "B"),
        Duration::from_secs(120),
    )
    .await;
    let mut child = export.spawn(&first);

    // Wait until item-a's answer has reached the export: the joblog marks it,
    // or a second has passed since it was requested.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut requested_at = None;
    loop {
        assert!(Instant::now() < deadline, "item-a was never fetched");
        if joblog_marks_ok(&export.joblog(), "item-a") {
            break;
        }
        if requested_at.is_none() && requested(&first, "item-a").await {
            requested_at = Some(Instant::now());
        }
        if requested_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    signal(&child, libc::SIGINT);
    let status = child.wait().unwrap();

    let second = MockServer::start().await;
    mount(&second, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    mount(&second, "item-b", item_body("item-b", "B"), Duration::ZERO).await;
    let output = export.run(&second);
    assert!(output.status.success(), "{output:?}");

    let content = fs::read_to_string(export.out()).unwrap_or_default();
    assert_eq!(
        status.code(),
        Some(130),
        "Ctrl+C should write the pending rows and exit 130; file:\n{content}"
    );
    assert_eq!(
        rows(&export.out()),
        vec!["item-a,A", "item-b,B"],
        "resumed CSV must hold every item once:\n{content}"
    );
}

/// A hard kill (no chance to write on the way out) after the joblog marks
/// `item-a` done: the file already holds `item-a`'s row, so the rerun does
/// not fetch it again and ends with both rows, once each.
#[tokio::test]
async fn export_csv_row_is_written_before_joblog_marks_it_done() {
    let export = Export::new("item-a\nitem-b\n");

    let first = MockServer::start().await;
    mount(&first, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    mount(
        &first,
        "item-b",
        item_body("item-b", "B"),
        Duration::from_secs(120),
    )
    .await;
    let mut child = export.spawn(&first);

    // The file is rewritten at least every few seconds while rows wait.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !joblog_marks_ok(&export.joblog(), "item-a") {
        assert!(Instant::now() < deadline, "joblog never marked item-a done");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let on_disk = fs::read_to_string(export.out()).unwrap_or_default();
    signal(&child, libc::SIGKILL);
    child.wait().unwrap();
    assert!(
        on_disk.lines().any(|l| l == "item-a,A"),
        "joblog marked item-a done before its row was in the file:\n{on_disk}"
    );

    let second = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/item-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item_body("item-a", "A")))
        .expect(0)
        .mount(&second)
        .await;
    mount(&second, "item-b", item_body("item-b", "B"), Duration::ZERO).await;
    let output = export.run(&second);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows(&export.out()), vec!["item-a,A", "item-b,B"]);
}

/// The state a crash leaves between rewriting the file and marking its rows
/// done: the file holds `item-b`, the joblog does not mark it. The rerun
/// fetches `item-b` again and its new row replaces the old one.
#[tokio::test]
async fn export_csv_refetched_row_replaces_unmarked_row() {
    let export = Export::new("item-a\nitem-b\n");
    fs::write(export.out(), "identifier,title\nitem-a,A\nitem-b,old\n").unwrap();
    fs::write(
        export.joblog(),
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item-a\",\"file\":\"\",\"status\":\"ok\"}\n",
    )
    .unwrap();

    let server = MockServer::start().await;
    mount(
        &server,
        "item-b",
        item_body("item-b", "new"),
        Duration::ZERO,
    )
    .await;
    let output = export.run(&server);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows(&export.out()), vec!["item-a,A", "item-b,new"]);
}

/// An item that cannot be fetched makes the export exit 1 and is named; the
/// other item's row is still written (#189).
#[tokio::test]
async fn export_exits_1_when_an_item_fails() {
    let export = Export::new("item-a\nmissing\n");
    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    // The Metadata API answers an identifier with no item with `{}`.
    mount(&server, "missing", serde_json::json!({}), Duration::ZERO).await;

    let output = export.run(&server);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "export with a failed item exited {:?}; stderr: {stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains("1 of 2 item(s) failed: missing"),
        "stderr should name the item: {stderr}"
    );
    assert_eq!(rows(&export.out()), vec!["item-a,A"]);
}

/// Stdout mode exits 1 too.
#[tokio::test]
async fn export_to_stdout_exits_1_when_an_item_fails() {
    let export = Export::new("missing\n");
    let server = MockServer::start().await;
    mount(&server, "missing", serde_json::json!({}), Duration::ZERO).await;

    let output = export.command_to_stdout(&server).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("1 of 1 item(s) failed: missing"),
        "{stderr}"
    );
}
