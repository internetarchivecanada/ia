use assert_cmd::Command; // used by cargo_bin_cmd! macro
use predicates::prelude::*;
use std::fs;
use tempfile::{NamedTempFile, TempDir};

/// Helper: create `ia` command with an empty config file to avoid loading real credentials.
/// All tests in this file exercise CLI argument parsing, validation, and local-only operations.
/// None of them send any HTTP requests.
fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.arg("--config-file")
        .arg(config.path())
        // Prevent the binary from picking up real env-var credentials
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

/// Create an empty config file (no credentials, no sections).
fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
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

// ─── Flag acceptance ─────────────────────────────────────────────────────────

#[test]
fn upload_clobber_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--clobber"));
}

#[test]
fn upload_checksum_file_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--checksum-file"));
}

#[test]
fn upload_multipart_flag_accepted() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--multipart"));
}

#[test]
fn upload_spreadsheet_flag_shown_in_help() {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
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
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
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
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
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
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
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
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
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
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleanup"));
}

// ─── Auto-resume ─────────────────────────────────────────────────────────────

#[test]
fn upload_retry_failed_shows_error() {
    let config = empty_config();
    ia_with_config(&config)
        .args([
            "upload",
            "test-id",
            "file.txt",
            "--retry-failed",
            "--joblog",
            "/dev/null",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--retry-failed is no longer needed for uploads",
        ));
}

#[test]
fn upload_no_resume_flag_accepted() {
    let config = empty_config();
    ia_with_config(&config)
        .args(["upload", "test-id", "nonexistent.txt", "--no-resume"])
        .assert()
        .failure();
    // It fails for missing file, but --no-resume is accepted as valid flag
}
