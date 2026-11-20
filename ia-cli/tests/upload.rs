use assert_cmd::Command; // used by cargo_bin_cmd! macro
use predicates::prelude::*;
use std::fs;
use tempfile::{NamedTempFile, TempDir};

/// Helper: create `ia` command with an empty config file to avoid loading real credentials.
/// Most tests in this file exercise CLI argument parsing, validation, and local-only operations
/// and send no HTTP requests; the `--size-hint` ones send theirs to a wiremock server.
fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(config.path())
        // Prevent the binary from picking up real env-var credentials
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY");
    cmd
}

/// Config with no credentials and a host that refuses connections, so no test
/// in this file can reach archive.org. Tests that need HTTP pass `--host` to a mock.
fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "[general]\nhost = 127.0.0.1:1\n").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn upload_no_identifier_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .assert()
        .failure()
        .stderr(predicate::str::contains("identifier is required"));
}

#[test]
fn upload_no_files_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no files provided"));
}

#[test]
fn upload_nonexistent_file_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "my-item",
            "/tmp/ia-cli-test-nonexistent-file-xyz.txt",
            "-m",
            "mediatype:texts",
            "-m",
            "collection:test_collection",
            "--dry-run",
            "--no-verify",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No such file or directory").or(
            predicate::str::contains("The system cannot find the path specified"),
        ));
}

#[test]
fn upload_invalid_metadata_format() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args(["-m", "no-colon-here"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid KEY:VALUE format"));
}

#[test]
fn delete_after_upload_rejects_no_verify() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "-m",
            "collection:test_collection",
            "--delete-after-upload",
            "--no-verify",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--delete-after-upload requires verification",
        ));
}

// ─── Dry run ─────────────────────────────────────────────────────────────────

#[test]
fn upload_dry_run_succeeds_without_credentials() {
    // dry-run returns DryRun before any auth check (single.rs line 71),
    // so it works with an empty config — no HTTP requests are sent.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("hello.txt");
    fs::write(&file, "hello world").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "-m",
            "collection:test_collection",
            "--dry-run",
            "--no-verify",
        ])
        .assert()
        .success()
        // Aggregate display shows item name and summary instead of per-file "dry run" lines
        .stderr(predicate::str::contains("my-item"));
}

#[test]
fn upload_dry_run_with_test_item() {
    // --test-item injects collection:test_collection automatically,
    // so only mediatype is required.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("file.pdf");
    fs::write(&file, "pdf bytes").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "--test-item",
            "--dry-run",
            "--no-verify",
        ])
        .assert()
        .success()
        // Aggregate display shows item name and summary instead of per-file "dry run" lines
        .stderr(predicate::str::contains("my-item"));
}

// ─── JSON output ─────────────────────────────────────────────────────────────

#[test]
fn upload_json_output_format() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("data.bin");
    fs::write(&file, "some data").unwrap();

    let cfg = empty_config();
    let output = ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "-m",
            "collection:test_collection",
            "--dry-run",
            "--no-verify",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    // Each line should be valid JSON (JSONL format)
    for line in stdout.lines() {
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(val["identifier"], "my-item");
        assert_eq!(val["status"], "dry_run");
        assert!(val["bytes"].is_number());
    }
}

#[test]
fn upload_json_and_dashboard_mutually_exclusive() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args(["--json", "--dashboard"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mutually exclusive"));
}

// ─── Dashboard ───────────────────────────────────────────────────────────────

#[test]
fn upload_dashboard_requires_tty() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .arg("--dashboard")
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires an interactive terminal"));
}

#[test]
fn upload_dashboard_on_template_errors() {
    let dir = TempDir::new().unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "--dashboard", "template"])
        .arg(dir.path().as_os_str())
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supported for"));
}

#[test]
fn upload_dashboard_on_cleanup_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "--dashboard", "cleanup", "test-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supported for"));
}

#[test]
fn upload_dashboard_accepted_on_spreadsheet() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--dashboard",
            "--spreadsheet",
            "/tmp/nonexistent-test-file.csv",
        ])
        .assert()
        .failure()
        // Should fail for a reason OTHER than dashboard restriction —
        // verifies --spreadsheet is recognized as a valid dashboard target.
        .stderr(predicate::str::contains("only supported for").not());
}

// ─── Template subcommand ─────────────────────────────────────────────────────

#[test]
fn upload_template_generates_csv_to_stdout() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file1.txt"), "a").unwrap();
    fs::write(dir.path().join("file2.pdf"), "b").unwrap();

    let cfg = empty_config();
    let output = ia_with_config(&cfg)
        .arg("upload")
        .arg("template")
        .arg(dir.path().as_os_str())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    // Header row
    assert!(stdout.starts_with("identifier,file,REMOTE_NAME,mediatype,"));
    // Data rows (one per file)
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 files
    assert!(stdout.contains("file1.txt"));
    assert!(stdout.contains("file2.pdf"));
}

#[test]
fn upload_template_to_file() {
    let scan_dir = TempDir::new().unwrap();
    fs::write(scan_dir.path().join("doc.txt"), "content").unwrap();

    let out_dir = TempDir::new().unwrap();
    let output_csv = out_dir.path().join("output.csv");

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("template")
        .arg(scan_dir.path().as_os_str())
        .arg("-o")
        .arg(output_csv.as_os_str())
        .assert()
        .success()
        .stderr(predicate::str::contains("Wrote 1 rows"));

    let content = fs::read_to_string(&output_csv).unwrap();
    assert!(content.starts_with("identifier,file,"));
    assert!(content.contains("doc.txt"));
}

#[test]
fn upload_template_empty_dir_errors() {
    let dir = TempDir::new().unwrap();
    // dir is empty — no files

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("template")
        .arg(dir.path().as_os_str())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no files found"));
}

#[test]
fn upload_template_tsv_format() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("sample.txt"), "data").unwrap();

    let cfg = empty_config();
    let output = ia_with_config(&cfg)
        .arg("upload")
        .arg("template")
        .arg(dir.path().as_os_str())
        .args(["--format", "tsv"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    // TSV uses tabs between columns
    let header = stdout.lines().next().unwrap();
    assert!(header.contains('\t'));
    assert!(header.contains("identifier"));
    assert!(header.contains("file"));
    assert!(header.contains("mediatype"));
}

// ─── Spreadsheet (batch upload) ──────────────────────────────────────────────

#[test]
fn upload_spreadsheet_nonexistent_file() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            "/tmp/ia-cli-test-nonexistent-spreadsheet.csv",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read spreadsheet"));
}

// ─── Deprecated import subcommand ────────────────────────────────────────────

#[test]
fn upload_deprecated_import_warns() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "import",
            "/tmp/ia-cli-test-nonexistent-spreadsheet.csv",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("'ia upload import' is deprecated"))
        .stderr(predicate::str::contains("--spreadsheet"));
}

// ─── Cleanup subcommand ──────────────────────────────────────────────────────

#[test]
fn upload_cleanup_requires_credentials() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "cleanup", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("credentials required"));
}

// ─── Spreadsheet dry-run ─────────────────────────────────────────────────────

#[test]
fn upload_spreadsheet_csv_dry_run() {
    let dir = TempDir::new().unwrap();
    let data_file = dir.path().join("hello.txt");
    fs::write(&data_file, "hello world").unwrap();

    let csv_path = dir.path().join("batch.csv");
    fs::write(
        &csv_path,
        format!(
            "identifier,file,mediatype,collection\ntest-item-dry,{},texts,test_collection\n",
            data_file.display()
        ),
    )
    .unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            csv_path.to_str().unwrap(),
            "--dry-run",
            "--no-collection-check",
        ])
        .assert()
        .success();
}

#[test]
fn upload_spreadsheet_dry_run_lists_the_metadata_and_name_the_upload_sends() {
    // The listing used its own merge, which kept one value of subject[0]/
    // subject[1] and a -m value next to it (#97); it now shows what the
    // upload sends, and the REMOTE_NAME (#98).
    let dir = TempDir::new().unwrap();
    let data_file = dir.path().join("hello.txt");
    fs::write(&data_file, "hello world").unwrap();

    let csv_path = dir.path().join("batch.csv");
    fs::write(
        &csv_path,
        format!(
            "identifier,file,REMOTE_NAME,mediatype,collection,subject[0],subject[1]\n\
             test-item-dry,{},renamed.txt,texts,test_collection,alpha,beta\n",
            data_file.display()
        ),
    )
    .unwrap();

    let cfg = empty_config();
    let out = ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            csv_path.to_str().unwrap(),
            "-m",
            "subject:from-cli",
            "-m",
            "creator:cli-creator",
            "--dry-run",
            "--no-collection-check",
        ])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(out).unwrap();
    assert!(
        stderr.contains("alpha") && stderr.contains("beta"),
        "{stderr}"
    );
    assert!(stderr.contains("cli-creator"), "{stderr}");
    assert!(!stderr.contains("from-cli"), "{stderr}");
    assert!(stderr.contains("renamed.txt"), "{stderr}");
    assert!(!stderr.contains("remote_name"), "{stderr}");
}

#[test]
fn upload_spreadsheet_dry_run_refuses_a_remote_name_the_upload_would_refuse() {
    // The dry run checks every item's remote keys as the upload does.
    let dir = TempDir::new().unwrap();
    let data_file = dir.path().join("hello.txt");
    fs::write(&data_file, "hello world").unwrap();

    let csv_path = dir.path().join("batch.csv");
    fs::write(
        &csv_path,
        format!(
            "identifier,file,REMOTE_NAME,mediatype,collection\n\
             test-item-dry,{},../x.txt,texts,test_collection\n",
            data_file.display()
        ),
    )
    .unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            csv_path.to_str().unwrap(),
            "--dry-run",
            "--no-collection-check",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("../x.txt"));
}

// ─── Flag acceptance ─────────────────────────────────────────────────────────

#[test]
fn upload_clobber_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--clobber"));
}

#[test]
fn upload_checksum_file_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--checksum-file"));
}

#[test]
fn upload_multipart_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--multipart"));
}

#[test]
fn upload_spreadsheet_flag_shown_in_help() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--spreadsheet"));
}

#[test]
fn upload_multipart_dry_run_succeeds() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("hello.txt");
    fs::write(&file, "hello world").unwrap();

    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "-m",
            "collection:test_collection",
            "--multipart",
            "--dry-run",
            "--no-collection-check",
        ])
        .assert()
        .success();
}

// ─── Help text ───────────────────────────────────────────────────────────────

#[test]
fn upload_help_shows_subcommands() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--spreadsheet"))
        .stdout(predicate::str::contains("template"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--test-item"))
        .stdout(predicate::str::contains("--no-verify"))
        .stdout(predicate::str::contains("--remote-name"))
        .stdout(predicate::str::contains("--checksum-file"));
}

#[test]
fn upload_help_shows_spreadsheet_options() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--spreadsheet"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--test-item"));
}

#[test]
fn upload_template_help_shows_options() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "template", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DIR"))
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--format"))
        .stdout(predicate::str::contains("--identifier-from-filename"))
        .stdout(predicate::str::contains("--identifier-from-dirname"))
        .stdout(predicate::str::contains("--identifier-prefix"));
}

#[test]
fn upload_cleanup_help_shows_options() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "cleanup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("IDENTIFIER"))
        .stdout(predicate::str::contains("--abort-all"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("incomplete multipart"));
}

#[test]
fn upload_help_shows_cleanup_subcommand() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleanup"));
}

// ─── Auto-resume ─────────────────────────────────────────────────────────────

#[test]
fn upload_no_resume_flag_accepted() {
    let config = empty_config();
    ia_with_config(&config)
        .args(["upload", "test-id", "nonexistent.txt", "--no-resume"])
        .assert()
        .failure();
    // It fails for missing file, but --no-resume is accepted as valid flag
}

// ─── --size-hint ─────────────────────────────────────────────────────────────

fn one_file(dir: &TempDir) -> std::path::PathBuf {
    let file = dir.path().join("a.txt");
    fs::write(&file, "content").unwrap();
    file
}

#[test]
fn size_hint_with_no_size_hint_is_a_usage_error() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args(["--size-hint", "5", "--no-size-hint", "--dry-run"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn size_hint_with_no_size_hint_is_a_usage_error_with_a_spreadsheet() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            "batch.csv",
            "--size-hint",
            "5",
            "--no-size-hint",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn size_hint_must_be_a_whole_number_of_bytes() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    for bad in ["abc", "1.5", "-5", "10GB", ""] {
        ia_with_config(&cfg)
            .arg("upload")
            .arg("my-item")
            .arg(file.as_os_str())
            .arg(format!("--size-hint={bad}"))
            .arg("--dry-run")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("--size-hint"));
    }
}

#[test]
fn size_hint_with_a_size_hint_header_is_refused() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args([
            "--size-hint",
            "5",
            "--header",
            "X-Archive-Size-Hint:6",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--size-hint and --header x-archive-size-hint",
        ));
}

/// A config for a mock host, with S3 keys.
fn mock_config(host: &str) -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(
        f.path(),
        format!(
            "[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = test-access\nsecret = test-secret\n"
        ),
    )
    .unwrap();
    f
}

/// Every x-archive-size-hint line on the PUTs the server received, by path.
async fn put_hints(server: &wiremock::MockServer) -> Vec<(String, Vec<String>)> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .map(|r| {
            (
                r.url.path().to_string(),
                r.headers
                    .get_all("x-archive-size-hint")
                    .iter()
                    .map(|v| v.to_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// `ia upload --size-hint` reaches IA-S3 exactly as given, on the first file.
#[tokio::test(flavor = "multi_thread")]
async fn size_hint_is_sent_as_given() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "aaaa").unwrap();
    fs::write(&b, "bbbb").unwrap();

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "test-item"])
        .arg(a.as_os_str())
        .arg(b.as_os_str())
        .args([
            "--size-hint=2000000499999744",
            "--no-verify",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();

    let hints = put_hints(&server).await;
    assert_eq!(
        hints,
        vec![
            ("/test-item/a.txt".into(), vec!["2000000499999744".into()]),
            ("/test-item/b.txt".into(), vec![]),
        ]
    );
}

/// The same through `--spreadsheet`: each item's first file carries it.
#[tokio::test(flavor = "multi_thread")]
async fn size_hint_is_sent_as_given_with_a_spreadsheet() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.txt");
    fs::write(&a, "aaaa").unwrap();
    let sheet = dir.path().join("batch.csv");
    fs::write(
        &sheet,
        format!("identifier,file\ntest-item,{}\n", a.display()),
    )
    .unwrap();

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "--spreadsheet"])
        .arg(sheet.as_os_str())
        .args([
            "--size-hint",
            "2000000499999744",
            "--no-verify",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();

    assert_eq!(
        put_hints(&server).await,
        vec![("/test-item/a.txt".into(), vec!["2000000499999744".into()])]
    );
}

// ─── --no-multipart (#179) ───────────────────────────────────────────────────

#[test]
fn no_multipart_with_multipart_is_a_usage_error() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args(["--no-multipart", "--multipart", "--dry-run"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn no_multipart_with_multipart_is_a_usage_error_with_a_spreadsheet() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--spreadsheet",
            "batch.csv",
            "--no-multipart",
            "--multipart",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn no_multipart_with_multipart_is_a_usage_error_with_import() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "import",
            "batch.csv",
            "--no-multipart",
            "--multipart",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// A mock that answers single PUTs and refuses, and counts, any multipart
/// request: the item listing, initiate, completion, abort.
async fn single_put_only_server(puts: u64) -> wiremock::MockServer {
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(puts)
        .mount(&server)
        .await;
    server
}

/// A one-row spreadsheet for `test-item` with a file in `dir`.
fn one_row_sheet(dir: &TempDir) -> std::path::PathBuf {
    let a = dir.path().join("a.txt");
    fs::write(&a, "aaaa").unwrap();
    let sheet = dir.path().join("batch.csv");
    fs::write(
        &sheet,
        format!("identifier,file\ntest-item,{}\n", a.display()),
    )
    .unwrap();
    sheet
}

/// `ia upload --no-multipart` sends the file as a single PUT and does not
/// list the item's unfinished multipart uploads first.
#[tokio::test(flavor = "multi_thread")]
async fn no_multipart_sends_no_listing() {
    let server = single_put_only_server(1).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "test-item"])
        .arg(file.as_os_str())
        .args([
            "--no-multipart",
            "--no-verify",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
    server.verify().await;
}

/// The same through `--spreadsheet`.
#[tokio::test(flavor = "multi_thread")]
async fn no_multipart_sends_no_listing_with_a_spreadsheet() {
    let server = single_put_only_server(1).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let sheet = one_row_sheet(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "--spreadsheet"])
        .arg(sheet.as_os_str())
        .args([
            "--no-multipart",
            "--no-verify",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
    server.verify().await;
}

/// The same through the deprecated `import` subcommand.
#[tokio::test(flavor = "multi_thread")]
async fn no_multipart_sends_no_listing_with_import() {
    let server = single_put_only_server(1).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let sheet = one_row_sheet(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "import"])
        .arg(sheet.as_os_str())
        .args([
            "--no-multipart",
            "--no-verify",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
    server.verify().await;
}

// ─── --multipart[=auto|on|off|SIZE] (#181) ───────────────────────────────────

/// A dry run of one file with the given extra arguments.
fn dry_run_with(args: &[&str]) -> assert_cmd::assert::Assert {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("upload")
        .arg("my-item")
        .arg(file.as_os_str())
        .args(args)
        .args([
            "-m",
            "mediatype:texts",
            "--dry-run",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
}

#[test]
fn multipart_takes_auto_on_off_or_a_size() {
    for value in ["auto", "on", "off", "5000", "100M", "100m", "512K", "1G"] {
        dry_run_with(&[&format!("--multipart={value}")]).success();
    }
}

#[test]
fn multipart_refuses_other_values_and_names_the_accepted_ones() {
    for bad in [
        "0",
        "1.5G",
        "10MB",
        "-1",
        "",
        "ON",
        "1T",
        "99999999999999999999",
        "18446744073709551615G",
    ] {
        dry_run_with(&[&format!("--multipart={bad}")])
            .code(2)
            .stderr(predicate::str::contains("auto, on, off"));
    }
}

#[test]
fn multipart_zero_points_to_on() {
    dry_run_with(&["--multipart=0"])
        .code(2)
        .stderr(predicate::str::contains("--multipart=on"));
}

/// Bare `--multipart` takes no value from the next argument: the item and
/// the file after it stay positionals, wherever the flag stands.
#[test]
fn bare_multipart_leaves_the_following_positionals_alone() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "--multipart", "my-item"])
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "--dry-run",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
    ia_with_config(&cfg)
        .args(["upload", "my-item", "--multipart"])
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "--dry-run",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
}

const EVERY_MULTIPART_FORM: [&str; 5] = [
    "--multipart",
    "--multipart=on",
    "--multipart=off",
    "--multipart=auto",
    "--multipart=100M",
];

#[test]
fn every_multipart_form_with_no_multipart_is_a_usage_error() {
    for form in EVERY_MULTIPART_FORM {
        dry_run_with(&["--no-multipart", form])
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn every_multipart_form_with_no_multipart_is_a_usage_error_with_a_spreadsheet_and_import() {
    let cfg = empty_config();
    for form in EVERY_MULTIPART_FORM {
        ia_with_config(&cfg)
            .args([
                "upload",
                "--spreadsheet",
                "batch.csv",
                "--no-multipart",
                form,
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
        ia_with_config(&cfg)
            .args(["upload", "import", "batch.csv", "--no-multipart", form])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

/// A mock for one file over a 1 MiB cutoff (`over.bin`, a one-part
/// multipart upload after one listing of its name) and one at it
/// (`at.bin`, a single PUT).
async fn cutoff_server() -> wiremock::MockServer {
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/over.bin"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>cut-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/over.bin"))
        .and(query_param("partNumber", "1"))
        .and(query_param("uploadId", "cut-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/over.bin"))
        .and(query_param("uploadId", "cut-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/over.bin"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/at.bin"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/at.bin"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    server
}

/// `over.bin` (1 MiB + 1 byte) and `at.bin` (1 MiB) in `dir`.
fn over_and_at(dir: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let over = dir.path().join("over.bin");
    let at = dir.path().join("at.bin");
    fs::write(&over, vec![1u8; 1024 * 1024 + 1]).unwrap();
    fs::write(&at, vec![2u8; 1024 * 1024]).unwrap();
    (over, at)
}

/// A spreadsheet of `over.bin` and `at.bin` for `test-item`.
fn over_and_at_sheet(dir: &TempDir) -> std::path::PathBuf {
    let (over, at) = over_and_at(dir);
    let sheet = dir.path().join("batch.csv");
    fs::write(
        &sheet,
        format!(
            "identifier,file\ntest-item,{}\ntest-item,{}\n",
            over.display(),
            at.display()
        ),
    )
    .unwrap();
    sheet
}

const UPLOAD_QUIET: [&str; 3] = ["--no-verify", "--clobber", "--no-collection-check"];

#[tokio::test(flavor = "multi_thread")]
async fn multipart_size_sends_files_over_it_as_multipart() {
    let server = cutoff_server().await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let (over, at) = over_and_at(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "test-item"])
        .arg(over.as_os_str())
        .arg(at.as_os_str())
        .arg("--multipart=1M")
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn multipart_size_with_a_spreadsheet() {
    let server = cutoff_server().await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let sheet = over_and_at_sheet(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "--spreadsheet"])
        .arg(sheet.as_os_str())
        .arg("--multipart=1M")
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn multipart_size_with_import() {
    let server = cutoff_server().await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let sheet = over_and_at_sheet(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "import"])
        .arg(sheet.as_os_str())
        .arg("--multipart=1M")
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

/// The default sends both files, each 1 MiB or so and so under the 100 MiB
/// cutoff, as single PUTs with no listing.
#[tokio::test(flavor = "multi_thread")]
async fn the_default_sends_files_under_100_mib_as_single_puts() {
    let server = single_put_only_server(2).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let (over, at) = over_and_at(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "test-item"])
        .arg(over.as_os_str())
        .arg(at.as_os_str())
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

/// Bare `--multipart` means `on`: a 1 KiB file, far under any cutoff, is
/// sent as a multipart upload (listing, initiate, part, complete).
#[tokio::test(flavor = "multi_thread")]
async fn bare_multipart_sends_a_small_file_as_multipart() {
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test-item"))
        .and(query_param("uploads", ""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ListMultipartUploadsResult></ListMultipartUploadsResult>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.txt"))
        .and(query_param("uploads", ""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<InitiateMultipartUploadResult><UploadId>on-1</UploadId></InitiateMultipartUploadResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test-item/a.txt"))
        .and(query_param("partNumber", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test-item/a.txt"))
        .and(query_param("uploadId", "on-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(query_param_is_missing("partNumber"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);

    ia_with_config(&cfg)
        .args([
            "--insecure",
            "-H",
            &host,
            "upload",
            "test-item",
            "--multipart",
        ])
        .arg(file.as_os_str())
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

// ─── --multipart / --no-multipart before `import` (#181) ─────────────────────

#[test]
fn multipart_flags_across_import_conflict_like_on_one_side() {
    let cfg = empty_config();
    for args in [
        ["--no-multipart", "import", "batch.csv", "--multipart"],
        ["--multipart", "import", "batch.csv", "--no-multipart"],
        ["--multipart=5M", "import", "batch.csv", "--multipart"],
        ["--no-multipart", "import", "batch.csv", "--no-multipart"],
    ] {
        ia_with_config(&cfg)
            .arg("upload")
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot be used"));
    }
}

/// `--multipart=1M` before `import` takes effect: the file over 1 MiB is
/// multipart, the one at it a single PUT.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_size_before_import_takes_effect() {
    let server = cutoff_server().await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let sheet = over_and_at_sheet(&dir);

    ia_with_config(&cfg)
        .args([
            "--insecure",
            "-H",
            &host,
            "upload",
            "--multipart=1M",
            "import",
        ])
        .arg(sheet.as_os_str())
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

/// A sparse file one byte over the default 100 MiB cutoff: the only size at
/// which `off` and the default differ.
fn just_over_100_mib(dir: &TempDir) -> std::path::PathBuf {
    let p = dir.path().join("big.bin");
    let f = fs::File::create(&p).unwrap();
    f.set_len(100 * 1024 * 1024 + 1).unwrap();
    p
}

/// `--multipart=off` sends a file over 100 MiB as one single PUT, with no
/// multipart request of any kind.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_off_sends_a_file_over_100_mib_as_a_single_put() {
    let server = single_put_only_server(1).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let big = just_over_100_mib(&dir);

    ia_with_config(&cfg)
        .args(["--insecure", "-H", &host, "upload", "test-item"])
        .arg(big.as_os_str())
        .arg("--multipart=off")
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

/// `--no-multipart` before `import` takes effect the same way.
#[tokio::test(flavor = "multi_thread")]
async fn no_multipart_before_import_sends_a_file_over_100_mib_as_a_single_put() {
    let server = single_put_only_server(1).await;
    let host = server.uri().replace("http://", "");
    let cfg = mock_config(&host);
    let dir = TempDir::new().unwrap();
    let big = just_over_100_mib(&dir);
    let sheet = dir.path().join("batch.csv");
    fs::write(
        &sheet,
        format!("identifier,file\ntest-item,{}\n", big.display()),
    )
    .unwrap();

    ia_with_config(&cfg)
        .args([
            "--insecure",
            "-H",
            &host,
            "upload",
            "--no-multipart",
            "import",
        ])
        .arg(sheet.as_os_str())
        .args(UPLOAD_QUIET)
        .assert()
        .success();
    server.verify().await;
}

/// A dry run says how each file would be sent.
#[test]
fn dry_run_says_how_each_file_would_be_sent() {
    let dir = TempDir::new().unwrap();
    let (over, at) = over_and_at(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "my-item"])
        .arg(over.as_os_str())
        .arg(at.as_os_str())
        .args([
            "--multipart=1M",
            "-m",
            "mediatype:texts",
            "--dry-run",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_match(r"over\.bin .*multipart").unwrap())
        .stderr(predicate::str::is_match(r"at\.bin .*single PUT").unwrap());
}

/// `--multipart 500M` (no `=`) reads 500M as a file; the error says so.
#[test]
fn a_detached_multipart_value_gets_a_hint() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "my-item", "--multipart", "500M"])
        .arg(file.as_os_str())
        .args(["--dry-run", "--clobber", "--no-collection-check"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--multipart=500M"));
}

/// `ia upload --multipart 500M two.bin`: clap reads 500M as the item
/// identifier. Refused before any request, with how to write either intent;
/// the same on a dry run, and in the JSON error shape with `--json`.
#[test]
fn a_multipart_value_read_as_the_identifier_is_refused() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    for extra in [&[][..], &["--dry-run"][..]] {
        ia_with_config(&cfg)
            .args(["upload", "--multipart", "500M"])
            .arg(file.as_os_str())
            .args(extra)
            .args([
                "--clobber",
                "--no-collection-check",
                "-m",
                "mediatype:texts",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("read as the item identifier"))
            .stderr(predicate::str::contains("--multipart=500M"))
            .stderr(predicate::str::contains("--multipart=on 500M"));
    }
    let out = ia_with_config(&cfg)
        .args(["upload", "--multipart", "500M"])
        .arg(file.as_os_str())
        .args(["--json", "--dry-run", "--clobber", "--no-collection-check"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let line = String::from_utf8(out).unwrap();
    let json: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(json["error"]["code"], "invalid_argument", "{line}");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--multipart=on 500M"),
        "{line}"
    );
}

/// The same before the deprecated `import`, where the stray value would
/// otherwise be dropped silently; with `--spreadsheet`, clap already refuses
/// the stray positional.
#[test]
fn a_multipart_value_before_import_or_a_spreadsheet_is_refused() {
    let dir = TempDir::new().unwrap();
    let sheet = over_and_at_sheet(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "--multipart", "500M", "import"])
        .arg(sheet.as_os_str())
        .args(["--dry-run", "--clobber", "--no-collection-check"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--multipart=500M"));
    ia_with_config(&cfg)
        .args(["upload", "--multipart", "500M", "--spreadsheet"])
        .arg(sheet.as_os_str())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// No detached value: `--multipart=on 500M file` uploads to an item named
/// 500M (here a dry run), and `my-item 500M --multipart` fails on the
/// missing file with no `=` advice.
#[test]
fn no_hint_where_no_value_was_detached() {
    let dir = TempDir::new().unwrap();
    let file = one_file(&dir);
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["upload", "--multipart=on", "500M"])
        .arg(file.as_os_str())
        .args([
            "-m",
            "mediatype:texts",
            "--dry-run",
            "--clobber",
            "--no-collection-check",
        ])
        .assert()
        .success();
    ia_with_config(&cfg)
        .args(["upload", "my-item", "500M", "--multipart"])
        .args(["--dry-run", "--clobber", "--no-collection-check"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--multipart=500M").not());
}
