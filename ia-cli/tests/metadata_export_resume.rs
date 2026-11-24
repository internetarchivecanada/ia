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

// ─── Review of PR #194: --no-resume, nothing to write, failed writes, targets ─

impl Export {
    /// The export into `out` (or no `-o`), with or without the joblog, and
    /// `extra` arguments at the end.
    fn command_into(
        &self,
        server: &MockServer,
        out: Option<&Path>,
        joblog: bool,
        extra: &[&str],
    ) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ia-cli"));
        cmd.env(
            "IA_CACHE_DIR",
            concat!(env!("CARGO_TARGET_TMPDIR"), "/ia-cache"),
        )
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .arg("--config-file")
        .arg(self.cfg.path())
        .args(["--insecure", "-H", &server.uri().replace("http://", "")]);
        if joblog {
            cmd.arg("--joblog").arg(self.joblog());
        }
        cmd.args(["metadata", "export", "--itemlist"])
            .arg(self.dir.path().join("ids.txt"));
        if let Some(out) = out {
            cmd.arg("-o").arg(out);
        }
        cmd.args(extra);
        cmd
    }
}

/// Every item whose latest `export` entry in the joblog is `ok` has a row
/// in `out`.
fn assert_every_marked_item_has_a_row(joblog: &Path, out: &Path) {
    let log = fs::read_to_string(joblog).unwrap_or_default();
    let content = fs::read_to_string(out).unwrap_or_default();
    let mut latest = std::collections::BTreeMap::new();
    for line in log.lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        if entry["op"] == "export" {
            latest.insert(
                entry["item"].as_str().unwrap().to_string(),
                entry["status"] == "ok",
            );
        }
    }
    for (id, ok) in latest {
        if ok {
            assert!(
                content.lines().any(|l| l.starts_with(&format!("{id},"))),
                "joblog marks {id} done but the file has no row for it:\n{content}\njoblog:\n{log}"
            );
        }
    }
}

/// Root ignores directory and file modes, so a test that relies on them
/// cannot run as root.
fn running_as_root() -> bool {
    // SAFETY: geteuid(2) has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

fn ok_line(id: &str) -> String {
    format!(
        "{{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"{id}\",\"file\":\"\",\"status\":\"ok\"}}\n"
    )
}

/// Wait until `id` has been requested, then `settle` more.
async fn wait_requested(server: &MockServer, id: &str, settle: Duration) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !requested(server, id).await {
        assert!(Instant::now() < deadline, "{id} was never requested");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(settle).await;
}

/// An interrupted `--no-resume` run (past a mid-run rewrite's due time, then
/// Ctrl+C) leaves the complete earlier file: the joblog still marks the
/// earlier run's items done, and nothing was read back to keep their rows.
#[tokio::test]
async fn no_resume_interrupt_keeps_rows_the_joblog_marks_done() {
    let export = Export::new("item-a\nitem-b\nitem-c\n");
    let earlier = "identifier,title\nitem-a,A\nitem-b,B\nitem-c,C\n";
    fs::write(export.out(), earlier).unwrap();
    fs::write(
        export.joblog(),
        ok_line("item-a") + &ok_line("item-b") + &ok_line("item-c"),
    )
    .unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A2"), Duration::ZERO).await;
    mount(
        &server,
        "item-b",
        item_body("item-b", "B2"),
        Duration::from_secs(120),
    )
    .await;
    mount(
        &server,
        "item-c",
        item_body("item-c", "C2"),
        Duration::from_secs(120),
    )
    .await;
    let out = export.out();
    let mut child = export
        .command_into(&server, Some(&out), true, &["--no-resume"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Past the 5 s a mid-run rewrite waits for.
    wait_requested(&server, "item-a", Duration::from_secs(6)).await;
    signal(&child, libc::SIGINT);
    // --no-resume installs no Ctrl+C handler, so SIGINT has its inherited
    // disposition, which is "ignore" under a backgrounded shell: then kill.
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            signal(&child, libc::SIGKILL);
            child.wait().unwrap();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_every_marked_item_has_a_row(&export.joblog(), &out);
    assert_eq!(fs::read_to_string(&out).unwrap(), earlier);
}

/// A resumed run whose list leaves out an item an earlier run marked done
/// keeps that item's row (the file is read back whenever the joblog marks
/// any item done).
#[tokio::test]
async fn resume_keeps_rows_of_marked_items_outside_this_list() {
    let export = Export::new("item-a\n");
    fs::write(export.out(), "identifier,title\nitem-x,X\n").unwrap();
    fs::write(export.joblog(), ok_line("item-x")).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let output = export.run(&server);
    assert!(output.status.success(), "{output:?}");
    assert_every_marked_item_has_a_row(&export.joblog(), &export.out());
    assert_eq!(rows(&export.out()), vec!["item-a,A", "item-x,X"]);
}

/// Ctrl+C before any row is fetched leaves an existing file as it is.
#[tokio::test]
async fn interrupt_before_any_row_leaves_the_file() {
    let export = Export::new("item-a\nitem-b\n");
    let earlier = "identifier,title\nitem-a,A\nitem-b,B\n";
    fs::write(export.out(), earlier).unwrap();

    let server = MockServer::start().await;
    mount(
        &server,
        "item-a",
        item_body("item-a", "A"),
        Duration::from_secs(120),
    )
    .await;
    mount(
        &server,
        "item-b",
        item_body("item-b", "B"),
        Duration::from_secs(120),
    )
    .await;
    let mut child = export.spawn(&server);
    wait_requested(&server, "item-b", Duration::from_millis(500)).await;
    signal(&child, libc::SIGINT);
    child.wait().unwrap();

    assert_eq!(fs::read_to_string(export.out()).unwrap(), earlier);
}

/// A run in which every item fails leaves an existing file as it is.
#[tokio::test]
async fn run_with_no_rows_leaves_the_file() {
    let export = Export::new("missing\n");
    let earlier = "identifier,title\nitem-a,A\n";
    fs::write(export.out(), earlier).unwrap();

    let server = MockServer::start().await;
    mount(&server, "missing", serde_json::json!({}), Duration::ZERO).await;
    let output = export.run(&server);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(export.out()).unwrap(), earlier);
}

/// Ctrl+C whose final write fails still exits 130, reports the write
/// error, and marks nothing done.
#[tokio::test]
async fn interrupt_with_failing_write_exits_130() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let export = Export::new("item-a\nitem-b\n");
    let sub = export.dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let out = sub.join("out.csv");

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    mount(
        &server,
        "item-b",
        item_body("item-b", "B"),
        Duration::from_secs(120),
    )
    .await;
    let child = export
        .command_into(&server, Some(&out), true, &[])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Well inside the 5 s before a mid-run rewrite.
    wait_requested(&server, "item-a", Duration::from_secs(1)).await;
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
    signal(&child, libc::SIGINT);
    let output = child.wait_with_output().unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "stderr: {stderr}");
    assert!(
        stderr.contains("failed to write export file"),
        "the write error should be reported: {stderr}"
    );
    assert!(!joblog_marks_ok(&export.joblog(), "item-a"));
}

/// Replacing the output keeps its mode.
#[tokio::test]
async fn export_keeps_the_mode_of_the_file_it_replaces() {
    use std::os::unix::fs::PermissionsExt;
    let export = Export::new("item-a\n");
    fs::write(export.out(), "identifier,title\n").unwrap();
    fs::set_permissions(export.out(), fs::Permissions::from_mode(0o600)).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let out = export.out();
    let output = export
        .command_into(&server, Some(&out), false, &[])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows(&out), vec!["item-a,A"]);
    let mode = fs::metadata(&out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "mode was {mode:o}");
}

/// `-o` naming a symlink writes the file it points to and keeps the link.
#[tokio::test]
async fn export_through_a_symlink_keeps_the_link() {
    let export = Export::new("item-a\n");
    let real = export.dir.path().join("real.csv");
    fs::write(&real, "identifier,title\n").unwrap();
    std::os::unix::fs::symlink(&real, export.out()).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let out = export.out();
    let output = export
        .command_into(&server, Some(&out), false, &[])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        fs::symlink_metadata(&out).unwrap().file_type().is_symlink(),
        "the symlink was replaced"
    );
    assert_eq!(rows(&real), vec!["item-a,A"]);
}

/// A writable file in a directory that cannot be written to is written in
/// place.
#[tokio::test]
async fn export_into_a_read_only_directory_writes_in_place() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let export = Export::new("item-a\n");
    let sub = export.dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let out = sub.join("out.csv");
    fs::write(&out, "identifier,title\n").unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let output = export
        .command_into(&server, Some(&out), true, &[])
        .output()
        .unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows(&out), vec!["item-a,A"]);
    assert_every_marked_item_has_a_row(&export.joblog(), &out);
}

/// `-o` naming a FIFO writes the file into it once, and the FIFO stays.
#[tokio::test]
async fn export_into_a_fifo_writes_it_once() {
    let export = Export::new("item-a\n");
    let fifo = export.dir.path().join("out.csv");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: mkfifo(3) on a path in our own temp directory.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);

    let (tx, rx) = std::sync::mpsc::channel();
    let reader_path = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(fs::read_to_string(reader_path));
    });

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let output = export
        .command_into(&server, Some(&fifo), true, &[])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    {
        use std::os::unix::fs::FileTypeExt;
        assert!(
            fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo(),
            "the FIFO was replaced"
        );
    }
    let got = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("nothing was written into the FIFO")
        .unwrap();
    assert_eq!(got, "identifier,title\nitem-a,A\n");
}

/// A `--no-resume` run that fails an item an earlier run marked done leaves
/// it not done (its latest entry is the error), so the next run fetches it
/// again instead of skipping an item with no row.
#[tokio::test]
async fn no_resume_failure_unmarks_an_item() {
    let export = Export::new("item-x\nitem-y\n");
    fs::write(export.out(), "identifier,title\nitem-x,X\n").unwrap();
    fs::write(export.joblog(), ok_line("item-x")).unwrap();

    let first = MockServer::start().await;
    mount(&first, "item-x", serde_json::json!({}), Duration::ZERO).await;
    mount(&first, "item-y", item_body("item-y", "Y"), Duration::ZERO).await;
    let out = export.out();
    let output = export
        .command_into(&first, Some(&out), true, &["--no-resume"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_every_marked_item_has_a_row(&export.joblog(), &out);

    let second = MockServer::start().await;
    mount(&second, "item-x", item_body("item-x", "X2"), Duration::ZERO).await;
    let output = export.run(&second);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows(&out), vec!["item-x,X2", "item-y,Y"]);
}

/// The format comes from the `-o` name, not from the name of the file a
/// symlink there points to.
#[tokio::test]
async fn export_through_a_symlink_uses_the_format_of_the_o_name() {
    let export = Export::new("item-a\n");
    let real = export.dir.path().join("data.tsv");
    fs::write(&real, "identifier\ttitle\n").unwrap();
    std::os::unix::fs::symlink(&real, export.out()).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let out = export.out();
    let output = export
        .command_into(&server, Some(&out), false, &[])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        "identifier,title\nitem-a,A\n"
    );
}

/// A read-only output file is not replaced: the export fails to write it,
/// as it did before the rename.
#[tokio::test]
async fn export_does_not_replace_a_read_only_file() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let export = Export::new("item-a\n");
    let earlier = "identifier,title\nitem-z,Z\n";
    fs::write(export.out(), earlier).unwrap();
    fs::set_permissions(export.out(), fs::Permissions::from_mode(0o444)).unwrap();

    let server = MockServer::start().await;
    mount(&server, "item-a", item_body("item-a", "A"), Duration::ZERO).await;
    let out = export.out();
    let output = export
        .command_into(&server, Some(&out), true, &[])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(&out).unwrap(), earlier);
    assert!(!joblog_marks_ok(&export.joblog(), "item-a"));
}

/// An output file its owner cannot write, though another write bit is set
/// (mode 0477), is refused before any request, as a file that cannot be
/// written was before, not replaced by rename.
#[tokio::test]
async fn export_refuses_a_file_the_user_cannot_write() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let export = Export::new("item-a\n");
    let earlier = "identifier,title\nitem-z,Z\n";
    fs::write(export.out(), earlier).unwrap();
    fs::set_permissions(export.out(), fs::Permissions::from_mode(0o477)).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/item-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item_body("item-a", "A")))
        .expect(0)
        .mount(&server)
        .await;
    let out = export.out();
    let output = export
        .command_into(&server, Some(&out), true, &[])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("ermission denied"), "stderr: {stderr}");
    assert_eq!(fs::read_to_string(&out).unwrap(), earlier);
    let mode = fs::metadata(&out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o477, "the file was replaced");
}
