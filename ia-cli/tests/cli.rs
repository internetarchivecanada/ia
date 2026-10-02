use assert_cmd::Command;
use predicates::prelude::*;
use std::sync::OnceLock;
use tempfile::NamedTempFile;

/// Config shared by every test in this file: no credentials, and a host that
/// refuses connections. Nothing here can reach archive.org, even on a machine
/// whose real `ia.ini` holds keys. Tests that need HTTP start a wiremock server
/// and pass `--host`, which overrides this.
fn hermetic_config() -> &'static NamedTempFile {
    static CONFIG: OnceLock<NamedTempFile> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "[general]\nhost = 127.0.0.1:1\n").unwrap();
        f
    })
}

/// Bare `ia-cli` command with no config of its own. Only for tests that
/// exercise `--config-file` itself; everything else uses `ia()`.
fn ia_unconfigured() -> Command {
    assert_cmd::cargo_bin_cmd!("ia-cli")
}

/// Build an `ia-cli` command that cannot load real credentials or reach the
/// real host. See `hermetic_config`.
fn ia() -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(hermetic_config().path())
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY");
    cmd
}

fn status_json_with_joblog(joblog_content: &str) -> assert_cmd::assert::Assert {
    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");
    std::fs::write(&joblog_path, joblog_content).unwrap();
    ia().args([
        "status",
        "--joblog",
        joblog_path.to_str().unwrap(),
        "--json",
    ])
    .assert()
}

#[test]
fn help_shows_global_options() {
    ia().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--config-file"))
        .stdout(predicate::str::contains("--insecure"))
        .stdout(predicate::str::contains("--host"))
        .stdout(predicate::str::contains("--user-agent-suffix"))
        .stdout(predicate::str::contains("--joblog"))
        .stdout(predicate::str::contains("--log"))
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("--quiet"));
}

#[test]
fn version_flag() {
    ia().arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("ia"));
}

#[test]
fn no_subcommand_shows_help() {
    ia().assert()
        .failure()
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn download_subcommand_help() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--glob"))
        .stdout(predicate::str::contains("--jobs"))
        .stdout(predicate::str::contains("--checksum"))
        .stdout(predicate::str::contains("--count-views"));
}

#[test]
fn search_subcommand_help() {
    ia().args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scrape"))
        .stdout(predicate::str::contains("advanced"))
        .stdout(predicate::str::contains("fts"));
}

#[test]
fn search_scrape_help_has_sort() {
    ia().args(["search", "scrape", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--sort"));
}

#[test]
fn search_fts_help_has_dsl() {
    ia().args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dsl"));
}

#[test]
fn search_fts_help_has_scope() {
    ia().args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--scope"));
}

#[test]
fn search_advanced_help_no_dsl() {
    ia().args(["search", "advanced", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dsl").not());
}

#[test]
fn search_fts_help_no_sort() {
    ia().args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--sort").not());
}

#[test]
fn list_subcommand_help() {
    ia().args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--columns"))
        .stdout(predicate::str::contains("--location"));
}

#[test]
fn metadata_subcommand_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--exists"))
        .stdout(predicate::str::contains("--formats"))
        .stdout(predicate::str::contains("--pretty"))
        .stdout(predicate::str::contains("modify"))
        .stdout(predicate::str::contains("export"));
}

#[test]
fn status_subcommand_help() {
    ia().args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--joblog"));
}

#[test]
fn global_options_before_subcommand() {
    // Global options should be accepted before the subcommand
    ia().args([
        "--insecure",
        "--host",
        "test.archive.org",
        "download",
        "--help",
    ])
    .assert()
    .success();
}

#[test]
fn config_file_flag_accepts_path() {
    ia_unconfigured()
        .args([
            "--config-file",
            "/nonexistent/path.ini",
            "download",
            "--help",
        ])
        .assert()
        .success();
}

#[test]
fn completions_generates_fish() {
    ia().args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c ia"));
}

#[test]
fn completions_generates_bash() {
    ia().args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_ia"));
}

#[test]
fn completions_subcommand_help() {
    ia().args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shell"));
}

#[test]
fn man_prints_root_page_to_stdout() {
    ia().arg("man")
        .assert()
        .success()
        .stdout(predicate::str::contains(".TH ia-cli 1"))
        .stdout(predicate::str::contains("Internet Archive command"));
}

#[test]
fn man_out_dir_writes_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    ia().args(["man", "--out-dir"])
        .arg(dir.path())
        .assert()
        .success();

    // One page per command, named the way git and cargo name theirs.
    for page in [
        "ia-cli.1",
        "ia-cli-download.1",
        "ia-cli-metadata.1",
        "ia-cli-metadata-modify.1",
        "ia-cli-config-print-cookies.1",
    ] {
        assert!(
            dir.path().join(page).is_file(),
            "expected generated man page {page}"
        );
    }

    // clap's built-in `help` subcommand has no page worth generating.
    assert!(!dir.path().join("ia-cli-help.1").exists());
}

#[test]
fn man_page_title_is_hyphenated_but_synopsis_is_the_real_invocation() {
    // `man ia-cli-metadata-modify` is the page name, but what you type is
    // `ia-cli metadata modify`. git-rebase(1) makes the same distinction, and
    // getting it wrong would document a command that cannot be run.
    let dir = tempfile::tempdir().unwrap();
    ia().args(["man", "--out-dir"])
        .arg(dir.path())
        .assert()
        .success();

    let page = std::fs::read_to_string(dir.path().join("ia-cli-metadata-modify.1")).unwrap();
    assert!(
        page.contains(".TH ia-cli-metadata-modify 1"),
        "page title should be the hyphenated page name"
    );
    assert!(
        page.contains(r"ia\-cli metadata modify"),
        "synopsis should show the space-separated invocation, got:\n{page}"
    );
}

#[test]
fn man_rename_overrides_the_command_name() {
    ia().args(["man", "--rename", "ia"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".TH ia 1"));
}

#[test]
fn short_flags_work() {
    ia().args(["-i", "-H", "test.archive.org", "download", "--help"])
        .assert()
        .success();
}

#[test]
fn download_help_shows_dashboard_flag() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dashboard"));
}

#[test]
fn download_help_does_not_show_tui_flag() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tui").not());
}

#[test]
fn metadata_write_flags_in_modify_help() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--metadata"))
        .stdout(predicate::str::contains("--target"))
        .stdout(predicate::str::contains("--expect"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--priority"))
        .stdout(predicate::str::contains("--reduced-priority"));
}

#[test]
fn metadata_no_identifier_errors() {
    // Read mode with no identifier from piped stdin should warn and exit 0
    ia().args(["metadata"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no identifiers found"));
}

#[test]
fn metadata_modify_no_identifier_errors() {
    // Write mode with no identifier should fail
    ia().args(["metadata", "modify", "-m", "title:New"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifiers"));
}

#[test]
fn metadata_immutable_field_warning() {
    // Attempting to modify an immutable field should warn (fails with auth error too)
    ia().args(["metadata", "modify", "test-item", "-m", "identifier:new_id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("immutable"));
}

#[test]
fn metadata_admin_field_warning() {
    // Attempting to modify an admin-only field should warn
    ia().args(["metadata", "modify", "test-item", "-m", "mediatype:audio"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));
}

#[test]
fn metadata_import_nonexistent_file_errors() {
    ia().args(["metadata", "import", "/tmp/nonexistent_ia_test_file.csv"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn metadata_import_csv_accepted() {
    // import with a valid CSV should be accepted as a write operation.
    // It will fail with auth error after reading the CSV, which is fine.
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item,New Title\n").unwrap();

    let result = ia()
        .args(["metadata", "import", csv_path.to_str().unwrap()])
        .assert()
        .failure();

    // Should NOT contain "not yet implemented"
    result.stderr(predicate::str::contains("not yet implemented").not());
}

#[test]
fn metadata_import_shows_deprecation_warning() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item,New Title\n").unwrap();

    ia().args(["metadata", "import", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("deprecated"));
}

#[test]
fn metadata_spreadsheet_shown_in_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--spreadsheet"));
}

#[test]
fn metadata_spreadsheet_nonexistent_file_errors() {
    ia().args([
        "metadata",
        "--spreadsheet",
        "/tmp/nonexistent_ia_test_file.csv",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn metadata_spreadsheet_csv_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item,New Title\n").unwrap();

    let result = ia()
        .args(["metadata", "--spreadsheet", csv_path.to_str().unwrap()])
        .assert()
        .failure();

    // Should reach the spreadsheet import logic (fails with auth, not with parse error)
    result.stderr(predicate::str::contains("not yet implemented").not());
}

#[test]
fn metadata_export_itemlist_shown_in_help() {
    ia().args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--itemlist"));
}

#[test]
fn help_output_contains_examples_section() {
    ia().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"));
}

#[test]
fn short_help_omits_examples() {
    ia().arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:").not());
}

#[test]
fn download_long_help_has_examples() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia download"));
}

#[test]
fn download_short_help_omits_examples() {
    ia().args(["download", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:").not());
}

#[test]
fn search_long_help_has_examples() {
    ia().args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia search"));
}

#[test]
fn search_short_help_omits_examples() {
    ia().args(["search", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:").not());
}

#[test]
fn metadata_long_help_has_examples() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia metadata"));
}

#[test]
fn list_long_help_has_examples() {
    ia().args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia list"));
}

#[test]
fn status_long_help_has_examples() {
    ia().args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia status"));
}

#[test]
fn completions_long_help_has_examples() {
    ia().args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia completions"));
}

#[test]
fn json_and_dashboard_are_mutually_exclusive() {
    ia().args(["download", "test-item", "--json", "--dashboard"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mutually exclusive"));
}

// -- download file args tests --

#[test]
fn download_help_shows_file_example() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ia download nasa NASAarchiveLogo.jpg",
        ));
}

#[test]
fn download_help_shows_piped_example() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ia search -q collection:nasa --json | ia download",
        ));
}

// -- --json integration tests --

#[test]
fn status_json_empty_joblog() {
    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("empty.jsonl");
    std::fs::write(&joblog_path, "").unwrap();

    let output = ia()
        .args([
            "status",
            "--joblog",
            joblog_path.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["total"], 0);
    assert_eq!(v["succeeded"], 0);
    assert_eq!(v["failed"], 0);
    assert_eq!(v["skipped"], 0);
    assert!(v["failures"].as_array().unwrap().is_empty());
}

#[test]
fn status_json_with_successes() {
    let entries = [
        r#"{"ts":"2026-01-01T00:00:00Z","op":"download","item":"nasa","file":"photo.jpg","status":"ok","bytes":4200,"elapsed_ms":100}"#,
        r#"{"ts":"2026-01-01T00:00:01Z","op":"download","item":"nasa","file":"thumb.jpg","status":"skipped"}"#,
    ];

    let output = status_json_with_joblog(&entries.join("\n"))
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["total"], 2);
    assert_eq!(v["succeeded"], 1);
    assert_eq!(v["failed"], 0);
    assert_eq!(v["skipped"], 1);
    assert!(v["failures"].as_array().unwrap().is_empty());
}

#[test]
fn status_json_with_failures_exits_1() {
    let entries = [
        r#"{"ts":"2026-01-01T00:00:00Z","op":"download","item":"nasa","file":"photo.jpg","status":"ok","bytes":4200,"elapsed_ms":100}"#,
        r#"{"ts":"2026-01-01T00:00:01Z","op":"download","item":"broken","file":"video.mp4","status":"error","error":"connection reset"}"#,
    ];

    let output = status_json_with_joblog(&entries.join("\n"))
        .failure()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["total"], 2);
    assert_eq!(v["succeeded"], 1);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["failures"].as_array().unwrap().len(), 1);
    assert_eq!(v["failures"][0]["item"], "broken");
    assert_eq!(v["failures"][0]["file"], "video.mp4");
    assert_eq!(v["failures"][0]["error"], "connection reset");
}

#[test]
fn status_json_output_is_valid_single_line_json() {
    let entries = [
        r#"{"ts":"2026-01-01T00:00:00Z","op":"download","item":"a","file":"1.jpg","status":"ok","bytes":100,"elapsed_ms":10}"#,
        r#"{"ts":"2026-01-01T00:00:01Z","op":"download","item":"b","file":"2.jpg","status":"error","error":"timeout"}"#,
        r#"{"ts":"2026-01-01T00:00:02Z","op":"download","item":"c","file":"3.jpg","status":"skipped"}"#,
    ];

    let output = status_json_with_joblog(&entries.join("\n"))
        .failure()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    // Must be single line (not pretty-printed)
    assert_eq!(stdout.trim().lines().count(), 1);
    // Must be valid JSON
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    // Must have all required fields
    assert!(v.get("total").is_some());
    assert!(v.get("succeeded").is_some());
    assert!(v.get("failed").is_some());
    assert!(v.get("skipped").is_some());
    assert!(v.get("failures").is_some());
}

#[test]
fn status_json_no_human_output_on_stderr() {
    let entries = [
        r#"{"ts":"2026-01-01T00:00:00Z","op":"download","item":"a","file":"1.jpg","status":"ok","bytes":100,"elapsed_ms":10}"#,
    ];

    status_json_with_joblog(&entries.join("\n"))
        .success()
        // No styled/colored human output on stderr
        .stderr(predicate::str::contains("Job log:").not())
        .stderr(predicate::str::contains("Succeeded").not())
        .stderr(predicate::str::contains("Total operations").not());
}

#[test]
fn status_json_nonexistent_joblog_fails() {
    ia().args(["status", "--joblog", "/nonexistent/path.jsonl", "--json"])
        .assert()
        .failure();
}

// Verify --json flag shows in help for all commands
#[test]
fn all_commands_show_json_in_help() {
    for cmd in ["download", "list", "metadata", "search", "status"] {
        ia().args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--json"));
    }
    // Config subcommands each have their own --json flag
    for subcmd in [
        "show",
        "login",
        "check",
        "whoami",
        "print-cookies",
        "print-auth",
    ] {
        ia().args(["config", subcmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--json"));
    }
}

// Verify --json examples in long help
#[test]
fn download_help_shows_json_example() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ia download nasa --json"));
}

#[test]
fn list_help_shows_json_example() {
    ia().args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ia list nasa --json"));
}

#[test]
fn metadata_help_shows_json_example() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ia metadata"));
}

#[test]
fn status_help_shows_json_example() {
    ia().args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

// --- ia metadata subcommand restructuring ---

#[test]
fn metadata_subcommands_shown_in_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("modify"))
        .stdout(predicate::str::contains("append"))
        .stdout(predicate::str::contains("append-list"))
        .stdout(predicate::str::contains("insert"))
        .stdout(predicate::str::contains("remove"))
        .stdout(predicate::str::contains("import"));
}

#[test]
fn metadata_modify_help_has_metadata_flag() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--metadata"));
}

#[test]
fn metadata_modify_help_has_short_m_flag() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-m"));
}

#[test]
fn metadata_modify_help_has_target() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--target"));
}

#[test]
fn metadata_export_help_has_output() {
    ia().args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"));
}

#[test]
fn metadata_import_help_has_dry_run() {
    ia().args(["metadata", "import", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn metadata_modify_help_no_exists_flag() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--exists").not());
}

#[test]
fn metadata_bare_read_still_requires_identifier() {
    // Bare `ia metadata` with no args from piped stdin should warn and exit 0
    ia().args(["metadata"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no identifiers found"));
}

#[test]
fn metadata_append_list_help_has_metadata_flag() {
    ia().args(["metadata", "append-list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--metadata"))
        .stdout(predicate::str::contains("-m"));
}

#[test]
fn metadata_import_requires_file() {
    ia().args(["metadata", "import"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("FILE").or(predicate::str::contains("file")));
}

// --- ia ai subcommand restructuring ---

#[cfg(feature = "alpha")]
#[test]
fn ai_help_shows_qa_and_config() {
    ia().args(["ai", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("qa"))
        .stdout(predicate::str::contains("config"));
}

#[cfg(feature = "alpha")]
#[test]
fn ai_qa_no_input_errors() {
    // `ia ai qa` with no args should error about missing input
    ia().args(["ai", "qa"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifier").or(predicate::str::contains("no input")));
}

#[cfg(feature = "alpha")]
#[test]
fn ai_qa_help_has_expected_flags() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--promote"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--confidence"));
}

#[cfg(feature = "alpha")]
#[test]
fn ai_config_show_requires_collection() {
    ia().args(["ai", "config", "show"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("COLLECTION").or(predicate::str::contains("collection")));
}

#[cfg(feature = "alpha")]
#[test]
fn ai_config_help_shows_subcommands() {
    ia().args(["ai", "config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("edit"));
}

#[cfg(all(feature = "alpha", feature = "ai-analyze"))]
#[test]
fn ai_undo_subcommand_requires_joblog() {
    ia().args(["ai", "undo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("JOBLOG").or(predicate::str::contains("joblog")));
}

#[test]
fn metadata_export_help_shows_supported_formats() {
    // -o should be accepted (not bail with "not yet implemented")
    // and help text should mention supported formats
    ia().args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CSV"))
        .stdout(predicate::str::contains("XLSX"))
        .stdout(predicate::str::contains("JSONL"));
}

#[test]
fn metadata_import_with_column_prefixes() {
    // import with column prefixes should be accepted
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(
        &csv_path,
        "identifier,append-list:subject\ntest-item,science\n",
    )
    .unwrap();

    ia().args(["metadata", "import", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        // Should fail with auth error, not parse error
        .stderr(predicate::str::contains("not yet implemented").not());
}

#[test]
fn metadata_import_indexed_columns_accepted() {
    // CSV with subject[0], subject[1] columns should be parsed correctly
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(
        &csv_path,
        "identifier,subject[0],subject[1],title\ntest-item,science,nasa,Apollo 11\n",
    )
    .unwrap();

    ia().args(["metadata", "import", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        // Should fail with auth error, not parse error — indexed columns are valid
        .stderr(predicate::str::contains("parse").not());
}

// --- metadata export redesign ---

#[test]
fn metadata_export_accepts_file_positional_arg() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("items.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item-nonexistent,Test\n").unwrap();

    // Should succeed (reads identifier from CSV, fetches metadata)
    // Not fail with "unknown argument" or "unexpected argument"
    ia().args(["metadata", "export", csv_path.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn metadata_export_file_not_found_error() {
    ia().args(["metadata", "export", "/nonexistent/file.csv"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read identifiers from"));
}

#[test]
fn metadata_export_no_input_shows_help() {
    // No files, no search, terminal stdin → helpful error
    ia().args(["metadata", "export"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No input").or(predicate::str::contains("no input")));
}

#[test]
fn metadata_export_plain_text_itemlist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ids.txt");
    std::fs::write(&path, "test-nonexistent-id\n").unwrap();

    // Should succeed (reads identifiers from plain text, fetches metadata)
    ia().args(["metadata", "export", path.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn metadata_export_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    let csv1 = dir.path().join("a.csv");
    let csv2 = dir.path().join("b.csv");
    std::fs::write(&csv1, "identifier\nid1\n").unwrap();
    std::fs::write(&csv2, "identifier\nid2\n").unwrap();

    // Should succeed with 2 items from 2 files
    ia().args([
        "metadata",
        "export",
        csv1.to_str().unwrap(),
        csv2.to_str().unwrap(),
    ])
    .assert()
    .success();
}

#[test]
fn metadata_export_help_shows_file_input() {
    ia().args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("items.csv"));
}

// --- multi-ID bare mode ---

#[test]
fn metadata_bare_multiple_ids_accepted() {
    // Multiple positional IDs should be accepted and produce JSONL output.
    //
    // CRITICAL: this test MUST NOT hit archive.org. A previous version ran
    // `ia metadata id1 id2 id3` against the live metadata API (read-only GETs
    // on every test run) and its success depended on archive.org returning
    // `200 {}` for nonexistent items. We now point `--host` at a wiremock
    // server that serves the same `{}` responses.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());

    rt.block_on(async {
        for id in ["id1", "id2", "id3"] {
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(format!("/metadata/{id}")))
                .respond_with(
                    wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
                )
                .mount(&server)
                .await;
        }
    });

    let host = server.uri().strip_prefix("http://").unwrap().to_string();

    ia().args([
        "--insecure",
        "--host",
        &host,
        "metadata",
        "id1",
        "id2",
        "id3",
    ])
    .assert()
    .success();
}

#[test]
fn metadata_bare_exists_rejects_multiple_ids() {
    ia().args(["metadata", "id1", "id2", "--exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("single identifier"));
}

#[test]
fn metadata_bare_formats_rejects_multiple_ids() {
    ia().args(["metadata", "id1", "id2", "--formats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("single identifier"));
}

// --- import stdin hint ---

#[test]
fn metadata_import_no_file_suggests_modify() {
    // No file arg, no stdin → should suggest modify
    ia().args(["metadata", "import"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ia metadata modify"));
}

#[test]
fn metadata_import_stdin_suggests_modify() {
    // Piped stdin without a file → should suggest modify
    ia().args(["metadata", "import"])
        .write_stdin("some-identifier\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("ia metadata modify"));
}

// --- file-like identifier hint ---

#[test]
fn metadata_bare_file_like_identifier_hints_export() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("items.csv");
    std::fs::write(&csv_path, "identifier\nnasa\n").unwrap();

    // Running bare mode with a file path should hint at export
    ia().args(["metadata", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("ia metadata export")
                .or(predicate::str::contains("ia md export")),
        );
}

// --- export joblog ---

#[test]
fn metadata_export_writes_joblog() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    let log = dir.path().join("export.log");

    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
    ])
    .assert()
    .success();

    let content = std::fs::read_to_string(&log).unwrap();
    assert!(!content.is_empty(), "joblog should have entries");
    assert!(content.contains("\"op\":\"export\""));
    assert!(content.contains("\"item\":\"test-nonexistent-id\""));
}

#[test]
fn metadata_export_progress_bar_shown() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // indicatif suppresses the spinner label when not a TTY, but the summary
    // separator and stats are always printed
    ia().args(["metadata", "export", "--itemlist", ids.to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("──────────"));
}

#[test]
fn metadata_export_shows_summary_stats() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // Summary should show "items exported" and "fetched"
    ia().args(["metadata", "export", "--itemlist", ids.to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("items exported"))
        .stderr(predicate::str::contains("fetched"));
}

#[test]
fn metadata_export_quiet_suppresses_progress() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // -q should suppress the progress bar but still show summary
    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "-q",
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("Exporting metadata...").not())
    .stderr(predicate::str::contains("items exported"));
}

// ── Export resume tests ──────────────────────────────────────────────────────

#[test]
fn metadata_export_resume_all_done_does_not_truncate_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate output file with existing data
    let output = dir.path().join("out.jsonl");
    std::fs::write(
        &output,
        "{\"identifier\":\"item1\",\"title\":\"First\"}\n{\"identifier\":\"item2\",\"title\":\"Second\"}\n",
    )
    .unwrap();

    // Pre-populate joblog marking both items as done
    let log = dir.path().join("export.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"export\",\"item\":\"item2\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    // Item list with same identifiers
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\nitem2\n").unwrap();

    // Run export — should early-return without truncating the output file
    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("already exported"));

    // Output file must NOT be truncated
    let content = std::fs::read_to_string(&output).unwrap();
    assert!(
        content.contains("item1"),
        "output file should still contain item1, got: {content}"
    );
    assert!(
        content.contains("item2"),
        "output file should still contain item2, got: {content}"
    );
}

#[test]
fn metadata_export_resume_all_done_does_not_truncate_csv() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate output CSV with existing data
    let output = dir.path().join("out.csv");
    std::fs::write(&output, "identifier,title\nitem1,First\nitem2,Second\n").unwrap();

    // Pre-populate joblog marking both items as done
    let log = dir.path().join("export.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"export\",\"item\":\"item2\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\nitem2\n").unwrap();

    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("already exported"));

    // Output CSV must NOT be truncated
    let content = std::fs::read_to_string(&output).unwrap();
    assert!(
        content.contains("item1"),
        "CSV should still contain item1, got: {content}"
    );
}

#[test]
fn metadata_export_resume_appends_to_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate output file with one existing record
    let output = dir.path().join("out.jsonl");
    std::fs::write(&output, "{\"identifier\":\"item1\",\"title\":\"First\"}\n").unwrap();

    // Joblog marks item1 as done, item2 still pending (will 404 but that's ok)
    let log = dir.path().join("export.log");
    std::fs::write(
        &log,
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
    )
    .unwrap();

    // Item list has item1 (done) and a nonexistent item (will fail)
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\ntest-nonexistent-export-resume\n").unwrap();

    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("previously completed"));

    // Original record must still be present (JSONL appends, doesn't truncate)
    let content = std::fs::read_to_string(&output).unwrap();
    assert!(
        content.contains("item1"),
        "JSONL should still contain item1 after resume, got: {content}"
    );
}

#[test]
fn metadata_export_resume_csv_merges_existing() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate output CSV with one existing record
    let output = dir.path().join("out.csv");
    std::fs::write(&output, "identifier,title\nitem1,First\n").unwrap();

    // Joblog marks item1 as done
    let log = dir.path().join("export.log");
    std::fs::write(
        &log,
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":100,\"elapsed_ms\":50}\n",
    )
    .unwrap();

    // Item list has item1 (done) and a nonexistent item (will fail)
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\ntest-nonexistent-export-csv\n").unwrap();

    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("previously completed"));

    // Original record must still be present after CSV merge
    let content = std::fs::read_to_string(&output).unwrap();
    assert!(
        content.contains("item1"),
        "CSV should still contain item1 after resume, got: {content}"
    );
    assert!(
        content.contains("First"),
        "CSV should preserve field values, got: {content}"
    );
}

#[test]
fn metadata_export_jsonl_matches_stdout_output() {
    // Export a single item to both JSONL file and stdout, verify identical output.
    // Uses a wiremock server — ZERO live requests to archive.org.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());

    let mock_item = serde_json::json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Test Item",
            "collection": ["test_collection"],
            "mediatype": "texts"
        },
        "files": [
            {"name": "file.txt", "size": "100", "format": "Text", "source": "original"}
        ],
        "server": "ia000000.us.archive.org",
        "d1": "ia000000.us.archive.org",
        "d2": "ia000001.us.archive.org",
        "dir": "/0/items/test-item",
        "files_count": 1,
        "item_size": 100,
        "is_dark": false
    });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/test-item"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&mock_item))
            .mount(&server)
            .await;
    });

    let host = server.uri().strip_prefix("http://").unwrap().to_string();

    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-item\n").unwrap();

    // Stdout mode
    let stdout_result = ia()
        .args([
            "--insecure",
            "--host",
            &host,
            "metadata",
            "export",
            "--itemlist",
            ids.to_str().unwrap(),
            "-qq",
        ])
        .output()
        .unwrap();
    let stdout_str = String::from_utf8_lossy(&stdout_result.stdout);
    let stdout_json: serde_json::Value = serde_json::from_str(stdout_str.trim()).unwrap();

    // File mode (JSONL)
    let output = dir.path().join("out.jsonl");
    ia().args([
        "--insecure",
        "--host",
        &host,
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "-qq",
    ])
    .assert()
    .success();

    let file_content = std::fs::read_to_string(&output).unwrap();
    let file_json: serde_json::Value = serde_json::from_str(file_content.trim()).unwrap();

    // Both must have the same top-level structure
    assert!(
        stdout_json.get("metadata").is_some(),
        "stdout should have nested 'metadata' object"
    );
    assert!(
        file_json.get("metadata").is_some(),
        "JSONL file should have nested 'metadata' object, not flattened fields"
    );
    assert!(
        file_json.get("files").is_some(),
        "JSONL file should have 'files' array"
    );
    assert!(
        file_json.get("server").is_some() || file_json.get("d1").is_some(),
        "JSONL file should have server info"
    );

    // Verify arrays are preserved (not flattened to collection[0], collection[1])
    let md = file_json.get("metadata").unwrap();
    if let Some(coll) = md.get("collection") {
        assert!(
            coll.is_array() || coll.is_string(),
            "collection should be array or string, not indexed keys"
        );
    }

    // The two outputs should be identical JSON
    assert_eq!(
        stdout_json, file_json,
        "JSONL file output must match stdout output exactly"
    );
}

// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn search_advanced_help_has_rows() {
    ia().args(["search", "advanced", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--rows"));
}

// =============================================================================
// Compound metadata operations
// =============================================================================

#[test]
fn metadata_compound_trailing_plus_error() {
    ia().args(["metadata", "modify", "test-item", "-m", "title:New", "+"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected operation after +"));
}

#[test]
fn metadata_compound_unknown_op_error() {
    ia().args([
        "metadata",
        "modify",
        "test-item",
        "-m",
        "title:New",
        "+",
        "download",
        "-m",
        "x:y",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("unknown operation"));
}

#[test]
fn metadata_compound_shared_option_in_continuation_error() {
    ia().args([
        "metadata",
        "modify",
        "test-item",
        "-m",
        "title:New",
        "+",
        "remove",
        "--dry-run",
        "-m",
        "x:y",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("first operation segment"));
}

#[test]
fn metadata_compound_empty_continuation_error() {
    ia().args([
        "metadata",
        "modify",
        "test-item",
        "-m",
        "title:New",
        "+",
        "+",
        "remove",
        "-m",
        "x:y",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("expected operation after +"));
}

#[test]
fn metadata_import_with_compound_errors() {
    // import uses its own column-prefix system, not compatible with +
    ia().args(["metadata", "import", "data.csv", "+", "remove", "-m", "x:y"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "compound operations (+) cannot be used with --spreadsheet",
        ));
}

#[test]
fn metadata_help_mentions_compound_ops() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Compound operations (single request)",
        ));
}

#[test]
fn metadata_modify_help_mentions_compound_ops() {
    ia().args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Chain multiple operations with +"));
}

#[test]
fn metadata_empty_stdin_warns_and_exits_zero() {
    ia().args(["metadata"])
        .write_stdin("\n  \n\n")
        .assert()
        .success()
        .stderr(predicate::str::contains("no identifiers found"));
}

#[test]
fn metadata_batch_exists_with_itemlist_accepted() {
    // Clap should accept --exists with --itemlist (no longer rejected).
    // Will fail on network/file read, but should NOT fail on arg parsing.
    ia().args([
        "metadata",
        "--itemlist",
        "/tmp/nonexistent_ia_test_ids.txt",
        "--exists",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used with").not());
}

#[test]
fn metadata_batch_formats_with_itemlist_accepted() {
    ia().args([
        "metadata",
        "--itemlist",
        "/tmp/nonexistent_ia_test_ids.txt",
        "--formats",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used with").not());
}

// ─── Metadata shorthand: runtime validation ─────────────────────────────────

#[test]
fn metadata_dry_run_without_m_errors() {
    ia().args(["metadata", "nasa", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires -m or a write subcommand",
        ));
}

#[test]
fn metadata_target_without_m_errors() {
    ia().args(["metadata", "nasa", "--target", "files/foo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires -m or a write subcommand",
        ));
}

#[test]
fn metadata_expect_without_m_errors() {
    ia().args(["metadata", "nasa", "--expect", "title:old"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires -m or a write subcommand",
        ));
}

#[test]
fn metadata_priority_without_m_errors() {
    ia().args(["metadata", "nasa", "--priority", "5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires -m or a write subcommand",
        ));
}

#[test]
fn metadata_reduced_priority_without_m_errors() {
    ia().args(["metadata", "nasa", "--reduced-priority"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires -m or a write subcommand",
        ));
}

#[test]
fn metadata_shorthand_m_accepted() {
    // -m at top level should be accepted by clap (not rejected as unknown argument).
    //
    // CRITICAL: this test MUST NOT hit archive.org. A previous version ran
    // `ia metadata nasa -m title:Test` for real, and on a machine with valid IA
    // credentials it repeatedly overwrote the live `nasa` item's title. We now
    // point `--host` at a wiremock server that mocks the metadata GET, and pass
    // `--dry-run` so even the mocked server never sees a write.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());

    let mock_item = serde_json::json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Original Title",
            "mediatype": "texts"
        },
        "files": [],
        "server": "ia000000.us.archive.org",
        "d1": "ia000000.us.archive.org",
        "d2": "ia000001.us.archive.org",
        "dir": "/0/items/test-item",
        "files_count": 0,
        "item_size": 0,
        "is_dark": false
    });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/test-item"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&mock_item))
            .mount(&server)
            .await;
    });

    let host = server.uri().strip_prefix("http://").unwrap().to_string();

    let output = ia()
        .args([
            "--insecure",
            "--host",
            &host,
            "metadata",
            "test-item",
            "-m",
            "title:Test",
            "--dry-run",
        ])
        .output()
        .expect("failed to run ia");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "clap rejected -m: {stderr}"
    );
}

// ── Metadata modify resume tests ────────────────────────────────────────────

#[test]
fn metadata_modify_resume_skips_completed_items() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate joblog marking both items as done
    let log = dir.path().join("modify.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"modify\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"modify\",\"item\":\"item2\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\nitem2\n").unwrap();

    // Run modify — should early-return without hitting the API
    ia().args([
        "metadata",
        "modify",
        "--itemlist",
        ids.to_str().unwrap(),
        "-m",
        "title:Test",
        "--joblog",
        log.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("already modified"));
}

#[test]
fn metadata_modify_resume_only_retries_failed() {
    // CRITICAL: this test MUST NOT hit archive.org. A previous version ran the
    // retry POST against the live server, which on a machine with valid IA
    // credentials would have mutated the live `item2` item. We now route all
    // requests through a wiremock server: GET returns a stub item, POST returns
    // 500 so the retry surface still produces "1 of 1 failed".
    let dir = tempfile::tempdir().unwrap();

    // item1 succeeded, item2 failed — only item2 should be retried
    let log = dir.path().join("modify.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"modify\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"modify\",\"item\":\"item2\",\"file\":\"\",\"status\":\"error\",\"error\":\"timeout\"}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "item1\nitem2\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());
    let item2_json = serde_json::json!({
        "metadata": {
            "identifier": "item2",
            "title": "Original Title",
            "mediatype": "texts"
        },
        "files": [],
        "server": "ia000000.us.archive.org",
        "d1": "ia000000.us.archive.org",
        "d2": "ia000001.us.archive.org",
        "dir": "/0/items/item2",
        "files_count": 0,
        "item_size": 0,
        "is_dark": false
    });
    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item2"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&item2_json))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/metadata/item2"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("mock failure"))
            .mount(&server)
            .await;
    });
    let host = server.uri().strip_prefix("http://").unwrap().to_string();

    // Run modify — item1 is skipped (resumed), item2 is retried and fails (mock 500)
    let output = ia()
        .env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &host,
            "metadata",
            "modify",
            "--itemlist",
            ids.to_str().unwrap(),
            "-m",
            "title:Test",
            "--joblog",
            log.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run ia");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "should fail because item2 retry hits the mocked 500, got: {stderr}"
    );
    assert!(
        stderr.contains("already modified"),
        "should mention resumed items, got: {stderr}"
    );
    assert!(
        stderr.contains("1 of 1"),
        "should report 1 of 1 failed (only item2 was retried), got: {stderr}"
    );
}

#[test]
fn metadata_import_resume_skips_completed_items() {
    let dir = tempfile::tempdir().unwrap();

    // Pre-populate joblog marking both items as done
    let log = dir.path().join("import.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"modify\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"modify\",\"item\":\"item2\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    // Spreadsheet with same identifiers
    let csv = dir.path().join("data.csv");
    std::fs::write(&csv, "identifier,title\nitem1,First\nitem2,Second\n").unwrap();

    // Run import — should early-return without hitting the API
    ia().args([
        "metadata",
        "--spreadsheet",
        csv.to_str().unwrap(),
        "--joblog",
        log.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("already modified"));
}

#[test]
fn metadata_import_resume_only_retries_failed() {
    // CRITICAL: this test MUST NOT hit archive.org — see the matching note on
    // `metadata_modify_resume_only_retries_failed`. All HTTP traffic is routed
    // to a wiremock server; the POST is mocked to 500 to preserve the
    // "1 of 1 failed" assertion.
    let dir = tempfile::tempdir().unwrap();

    // item1 succeeded, item2 failed
    let log = dir.path().join("import.log");
    let joblog = concat!(
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"modify\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":50}\n",
        "{\"ts\":\"2026-01-01T00:00:01Z\",\"op\":\"modify\",\"item\":\"item2\",\"file\":\"\",\"status\":\"error\",\"error\":\"timeout\"}\n",
    );
    std::fs::write(&log, joblog).unwrap();

    let csv = dir.path().join("data.csv");
    std::fs::write(&csv, "identifier,title\nitem1,First\nitem2,Second\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());
    let item2_json = serde_json::json!({
        "metadata": {
            "identifier": "item2",
            "title": "Original Title",
            "mediatype": "texts"
        },
        "files": [],
        "server": "ia000000.us.archive.org",
        "d1": "ia000000.us.archive.org",
        "d2": "ia000001.us.archive.org",
        "dir": "/0/items/item2",
        "files_count": 0,
        "item_size": 0,
        "is_dark": false
    });
    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item2"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&item2_json))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/metadata/item2"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("mock failure"))
            .mount(&server)
            .await;
    });
    let host = server.uri().strip_prefix("http://").unwrap().to_string();

    // Run import — item1 skipped, item2 retried and fails (mock 500)
    let output = ia()
        .env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &host,
            "metadata",
            "--spreadsheet",
            csv.to_str().unwrap(),
            "--joblog",
            log.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run ia");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "should fail because item2 retry hits the mocked 500, got: {stderr}"
    );
    assert!(
        stderr.contains("already modified"),
        "should mention resumed items, got: {stderr}"
    );
    assert!(
        stderr.contains("1 of 1"),
        "should report 1 of 1 failed (only item2 was retried), got: {stderr}"
    );
}

#[test]
fn jobs_zero_rejected_at_parse_time() {
    // clap rejects --jobs 0 before any command logic (or network) runs.
    ia().args(["--jobs", "0", "search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be at least 1"));
}

#[test]
fn jobs_non_numeric_rejected() {
    ia().args(["--jobs", "lots", "search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a number"));
}

// -- --min-speed (#11) --

#[test]
fn download_help_describes_min_speed() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--min-speed <RATE>"))
        // The rule, in numbers the user can act on.
        .stdout(predicate::str::contains("10K"))
        .stdout(predicate::str::contains("60 s"))
        .stdout(predicate::str::contains("30 s"))
        .stdout(predicate::str::contains("Range"))
        .stdout(predicate::str::contains("--retries"))
        .stdout(predicate::str::contains("0 disables"))
        // Example in the Examples section.
        .stdout(predicate::str::contains("--min-speed 0"));
}

#[test]
fn download_short_help_mentions_min_speed() {
    ia().args(["download", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--min-speed <RATE>"));
}

#[test]
fn download_rejects_bad_min_speed() {
    ia().args(["download", "test-item", "--min-speed", "10KB"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--min-speed"))
        .stderr(predicate::str::contains("10K"));
}

/// The struct-level `long_about` must reach `--help`. A doc comment on the
/// `Commands::Download` variant used to replace it with the one-line about.
#[test]
fn download_long_help_shows_long_about() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Download files from the Internet Archive.",
        ))
        .stdout(predicate::str::contains("Each file streams to <name>.part"));
    ia().args(["download", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Each file streams to").not())
        .stdout(predicate::str::contains("Download files from an item"));
}

// -- md5 mismatch handling (#14) --

#[test]
fn download_help_describes_md5_mismatch_handling() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".md5-mismatch"))
        .stdout(predicate::str::contains("twice in a row"))
        .stdout(predicate::str::contains(
            "deleted once a later attempt verifies",
        ));
}

// -- update: GitHub requests honor Retry-After --

#[cfg(feature = "self-update")]
#[test]
fn update_help_describes_retries() {
    ia().args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Retry-After"))
        .stdout(predicate::str::contains("1 s to 30 s"));
}

// -- ai: LLM request retries honor Retry-After --

#[cfg(feature = "alpha")]
#[test]
fn ai_help_describes_llm_retries() {
    ia().args(["ai", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Retry-After"));
}

// -- search retries a 429 with Retry-After --

#[test]
fn search_help_describes_429_handling() {
    for backend in ["scrape", "advanced", "fts"] {
        ia().args(["search", backend, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("429"))
            .stdout(predicate::str::contains("Retry-After"));
    }
}

// -- download retries honor Retry-After --

#[test]
fn download_help_describes_retry_waits_and_retry_after() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Retry-After"))
        .stdout(predicate::str::contains("up to a cap that doubles"));
}

// -- a 2xx on multipart complete is the upload; no post-completion check --

#[test]
fn upload_help_does_not_promise_a_post_completion_check() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not yet verified").not())
        .stdout(predicate::str::contains("5 minutes").not());
}

// -- a multipart resume is validated against the local file (#19) --

#[test]
fn upload_help_describes_resume_validation() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("matches the local file"))
        .stdout(predicate::str::contains("size and md5"));
}

// -- a failed multipart part leaves the upload on IA (#18) --

#[test]
fn upload_help_describes_kept_multipart_upload_on_part_failure() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("names its upload ID"))
        .stdout(predicate::str::contains(
            "ia upload cleanup my-item big.iso --abort",
        ))
        .stdout(predicate::str::contains("ITEM FILE --abort"))
        .stdout(predicate::str::contains(
            "cleaned up by IA after 30 days or more",
        ))
        .stdout(predicate::str::contains("resumes from the parts"));
}

#[test]
fn cleanup_help_says_unfinished_uploads_may_be_cleaned_up() {
    ia().args(["upload", "cleanup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "cleaned up by IA after 30 days or more",
        ));
}

// -- upload retries back off; --retry-sleep is gone --

#[test]
fn upload_help_has_no_retry_sleep_and_describes_backoff() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--retry-sleep").not())
        .stdout(predicate::str::contains("--retries"))
        .stdout(predicate::str::contains("up to a cap that doubles"))
        .stdout(predicate::str::contains("Retry-After: 0"))
        .stdout(predicate::str::contains("Retry-After"))
        .stdout(predicate::str::contains("--multipart --retries 20"));
}

#[test]
fn upload_rejects_retry_sleep() {
    ia().args(["upload", "test-item", "Cargo.toml", "--retry-sleep", "5"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--retry-sleep"));
}

// -- upload cleanup lists by default; aborting needs a flag (#24) --
//
// The binary runs against a wiremock server standing in for IA-S3:
// `GET /<item>?uploads` lists the item's unfinished uploads,
// `GET /<item>/<key>?uploadId=` lists an upload's parts, and
// `DELETE /<item>/<key>?uploadId=` aborts one. Every test mounts the DELETE
// with an exact expectation so an abort that must not happen is caught.

struct CleanupFixture {
    rt: tokio::runtime::Runtime,
    server: wiremock::MockServer,
    host: String,
}

/// Two unfinished uploads on `my-item`: `a.bin` (u1, parts of 10 and 5
/// bytes) and `b.bin` (u2, one part of 7 bytes). `deletes_a` and
/// `deletes_b` say how many aborts each may receive.
fn cleanup_fixture(deletes_a: u64, deletes_b: u64) -> CleanupFixture {
    cleanup_fixture_with(deletes_a, deletes_b, false)
}

/// As [`cleanup_fixture`]; with `b_abort_fails` the abort of `b.bin`
/// answers 403 AccessDenied.
fn cleanup_fixture_with(deletes_a: u64, deletes_b: u64, b_abort_fails: bool) -> CleanupFixture {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());
    rt.block_on(async {
        Mock::given(method("GET"))
            .and(path("/my-item"))
            .and(query_param("uploads", ""))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<ListMultipartUploadsResult>\
                 <Upload><Key>a.bin</Key><UploadId>u1</UploadId><Initiated>2026-10-02T01:00:00.000Z</Initiated></Upload>\
                 <Upload><Key>b.bin</Key><UploadId>u2</UploadId><Initiated>2026-10-02T02:00:00.000Z</Initiated></Upload>\
                 </ListMultipartUploadsResult>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/my-item/a.bin"))
            .and(query_param("uploadId", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>10</Size></Part><Part><PartNumber>2</PartNumber><ETag>"e2"</ETag><Size>5</Size></Part></ListPartsResult>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/my-item/b.bin"))
            .and(query_param("uploadId", "u2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<ListPartsResult><Part><PartNumber>1</PartNumber><ETag>"e1"</ETag><Size>7</Size></Part></ListPartsResult>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/my-item/a.bin"))
            .and(query_param("uploadId", "u1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(deletes_a)
            .mount(&server)
            .await;
        let b_response = if b_abort_fails {
            ResponseTemplate::new(403).set_body_string(
                "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
            )
        } else {
            ResponseTemplate::new(204)
        };
        Mock::given(method("DELETE"))
            .and(path("/my-item/b.bin"))
            .and(query_param("uploadId", "u2"))
            .respond_with(b_response)
            .expect(deletes_b)
            .mount(&server)
            .await;
    });
    let host = server.uri().strip_prefix("http://").unwrap().to_string();
    CleanupFixture { rt, server, host }
}

fn cleanup_cmd(fx: &CleanupFixture, args: &[&str]) -> Command {
    let mut cmd = ia();
    cmd.env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &fx.host,
            "upload",
            "cleanup",
            "my-item",
        ])
        .args(args);
    cmd
}

#[test]
fn cleanup_with_file_lists_that_file_and_aborts_nothing() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["a.bin"])
        .assert()
        .success()
        .stderr(predicate::str::contains("u1"))
        .stderr(predicate::str::contains("2 parts"))
        .stderr(predicate::str::contains("15 B"))
        .stderr(predicate::str::contains("u2").not())
        .stderr(predicate::str::contains("aborted my-item").not())
        .stderr(predicate::str::contains(
            "Nothing aborted. Add --abort to abort this upload.",
        ));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_without_file_lists_every_upload_with_parts_and_bytes() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &[])
        .assert()
        .success()
        .stderr(predicate::str::contains("u1"))
        .stderr(predicate::str::contains("u2"))
        .stderr(predicate::str::contains("1 part"))
        .stderr(predicate::str::contains("7 B"));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_abort_requires_file() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["--abort"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("FILE"));
}

#[test]
fn cleanup_abort_and_abort_all_conflict() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["a.bin", "--abort", "--abort-all"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn cleanup_abort_deletes_only_that_file() {
    let fx = cleanup_fixture(1, 0);
    cleanup_cmd(&fx, &["a.bin", "--abort"])
        .assert()
        .success()
        .stderr(predicate::str::contains("aborted my-item/a.bin"))
        .stderr(predicate::str::contains("2 parts"));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_abort_all_deletes_every_upload() {
    let fx = cleanup_fixture(1, 1);
    cleanup_cmd(&fx, &["--abort-all"])
        .assert()
        .success()
        .stderr(predicate::str::contains("aborted my-item/a.bin"))
        .stderr(predicate::str::contains("aborted my-item/b.bin"));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_dry_run_abort_sends_nothing() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["a.bin", "--abort", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("would abort my-item/a.bin"));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_json_listing_has_parts_and_bytes() {
    let fx = cleanup_fixture(0, 0);
    let out = cleanup_cmd(&fx, &["--json"]).output().unwrap();
    assert!(out.status.success());
    // JSONL: one object per upload per line, as `ia list --json` does.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    let a = lines
        .iter()
        .find(|u| u["key"] == "a.bin")
        .expect("a.bin listed");
    assert_eq!(a["upload_id"], "u1");
    assert_eq!(a["parts"], 2);
    assert_eq!(a["bytes"], 15);
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_json_with_no_uploads_prints_nothing() {
    let fx = cleanup_fixture(0, 0);
    let out = cleanup_cmd(&fx, &["zzz.bin", "--json"]).output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_file_with_no_uploads_says_so() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["zzz.bin"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "No incomplete uploads of 'zzz.bin'",
        ));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_json_abort_reports_each_abort_as_a_line() {
    let fx = cleanup_fixture(1, 0);
    let out = cleanup_cmd(&fx, &["a.bin", "--abort", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    assert_eq!(v["action"], "aborted");
    assert_eq!(v["upload_id"], "u1");
    assert_eq!(v["bytes"], 15);
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_abort_all_dry_run_lists_both_and_sends_nothing() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["--abort-all", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("would abort my-item/a.bin"))
        .stderr(predicate::str::contains("would abort my-item/b.bin"));
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_dry_run_alone_lists_and_sends_nothing() {
    let fx = cleanup_fixture(0, 0);
    cleanup_cmd(&fx, &["--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Nothing aborted"));
    fx.rt.block_on(fx.server.verify());
}

/// When the second of two aborts fails, the first stands and the error
/// says so; the exit code is 1.
#[test]
fn cleanup_abort_all_failing_midway_reports_what_stands() {
    let fx = cleanup_fixture_with(1, 1, true);
    cleanup_cmd(&fx, &["--abort-all"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("aborted my-item/a.bin"))
        .stderr(predicate::str::contains("AccessDenied"))
        .stderr(predicate::str::contains("earlier aborts stand"));
    fx.rt.block_on(fx.server.verify());
}

/// Under --json an error is the JSON error object on stderr, not plain text.
#[test]
fn cleanup_json_error_is_a_json_object_on_stderr() {
    let fx = cleanup_fixture_with(1, 1, true);
    let out = cleanup_cmd(&fx, &["--abort-all", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr.lines().last().unwrap_or("");
    let v: serde_json::Value =
        serde_json::from_str(last).unwrap_or_else(|_| panic!("stderr: {stderr}"));
    assert!(v["error"]["code"].is_string(), "{stderr}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("AccessDenied"),
        "{stderr}"
    );
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_json_dry_run_reports_would_abort() {
    let fx = cleanup_fixture(0, 0);
    let out = cleanup_cmd(&fx, &["a.bin", "--abort", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let line = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(v["action"], "would_abort");
    assert_eq!(v["upload_id"], "u1");
    assert_eq!(v["parts"], 2);
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn cleanup_help_says_which_flags_abort() {
    ia().args(["upload", "cleanup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--abort"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("without confirmation").not())
        .stdout(predicate::str::contains("Lists by default"));
}

// -- every command's long help renders (#17) --
//
// A doc comment on a `Commands` variant in lib.rs becomes that command's
// `about` and clears the `long_about` on its argument struct, so the
// paragraphs written for `ia <cmd> --help` silently vanish. Each row is a
// phrase from the struct's long_about; the one-liners `ia --help` shows are
// pinned too, so moving them onto the structs changes nothing visible.

#[test]
fn every_command_renders_its_long_help() {
    let table: &[(&str, &str, &str)] = &[
        (
            "collection",
            "Collections are items",
            "Create and manage collections",
        ),
        ("list", "Displays a table of files", "List files in an item"),
        (
            "metadata",
            "Read or modify Internet Archive item metadata",
            "Read or modify item metadata",
        ),
        (
            "search",
            "Uses the scrape API by default",
            "Search the Internet Archive",
        ),
        (
            "status",
            "Show a summary of a job log file",
            "Show job log summary",
        ),
        (
            "tasks",
            "Lists, submits, reruns, and inspects tasks",
            "Manage archive.org catalog tasks",
        ),
        (
            "upload",
            "Uploads one or more files to a single item",
            "Upload files to the Internet Archive",
        ),
        (
            "verify",
            "Verify that local files exist on archive.org",
            "Verify local files exist",
        ),
        (
            "completions",
            "Prints a completion script to stdout",
            "Generate shell completions",
        ),
        ("man", "Writes one page per", "Generate roff man pages"),
        (
            "config",
            "view configuration, validate credentials",
            "Configure credentials and settings",
        ),
        (
            "download",
            "Each file streams to <name>.part",
            "Download files from an item",
        ),
    ];
    for (cmd, long_phrase, _) in table {
        ia().args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(*long_phrase).name(cmd));
    }
    let top = ia().arg("--help").output().unwrap();
    let top = String::from_utf8_lossy(&top.stdout);
    for (cmd, _, one_liner) in table {
        assert!(
            top.contains(one_liner),
            "ia --help lost the one-liner for {cmd}: {top}"
        );
    }
}

// -- the global --joblog and --no-resume help say what resumes (#17) --

#[test]
fn global_resume_flags_say_what_resumes() {
    for cmd in [
        vec!["--help"],
        vec!["download", "--help"],
        vec!["upload", "--help"],
    ] {
        ia().args(&cmd)
            .assert()
            .success()
            .stdout(predicate::str::contains("upload all files fresh").not())
            .stdout(predicate::str::contains(
                "finished files for upload; fully finished items for download",
            ))
            .stdout(predicate::str::contains("(download, upload, ai)").not())
            .stdout(predicate::str::contains("process every file or item again"))
            .stdout(predicate::str::contains("tasks submit"));
    }
}

#[test]
fn upload_help_says_what_an_interrupted_single_put_does() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sent again from byte 0"));
}

// -- --no-resume is honored by every command that reads the joblog --
//
// metadata export, metadata modify and tasks submit skip items a joblog
// records as done. --no-resume must make each of them process the item
// again. Each test logs item1 as done, runs with --no-resume against
// wiremock, and expects the item's request to be made.

struct NoResumeFixture {
    dir: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
    server: wiremock::MockServer,
    host: String,
}

fn no_resume_fixture(op: &str, ids: &str) -> NoResumeFixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("log.jsonl"),
        format!(
            "{{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"{op}\",\"item\":\"item1\",\"file\":\"\",\"status\":\"ok\",\"bytes\":0,\"elapsed_ms\":1}}\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("ids.txt"), ids).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(wiremock::MockServer::start());
    let host = server.uri().strip_prefix("http://").unwrap().to_string();
    NoResumeFixture {
        dir,
        rt,
        server,
        host,
    }
}

fn item1_json() -> serde_json::Value {
    serde_json::json!({
        "metadata": {"identifier": "item1", "title": "T", "mediatype": "texts"},
        "files": [], "server": "ia0.us.archive.org", "d1": "ia0.us.archive.org",
        "d2": "ia1.us.archive.org", "dir": "/0/items/item1", "files_count": 0,
        "item_size": 0, "is_dark": false
    })
}

#[test]
fn metadata_export_no_resume_exports_a_logged_item_again() {
    let fx = no_resume_fixture("export", "item1\n");
    fx.rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item1"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(item1_json()))
            .expect(1)
            .mount(&fx.server)
            .await;
    });
    let out = fx.dir.path().join("out.jsonl");
    ia().args([
        "--insecure",
        "--host",
        &fx.host,
        "--joblog",
        fx.dir.path().join("log.jsonl").to_str().unwrap(),
        "--no-resume",
        "metadata",
        "export",
        "--itemlist",
        fx.dir.path().join("ids.txt").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ])
    .assert()
    .success();
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn metadata_modify_no_resume_modifies_a_logged_item_again() {
    let fx = no_resume_fixture("modify", "item1\n");
    fx.rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item1"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(item1_json()))
            .mount(&fx.server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/metadata/item1"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("mock failure"))
            .expect(1)
            .mount(&fx.server)
            .await;
    });
    ia().env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &fx.host,
            "--joblog",
            fx.dir.path().join("log.jsonl").to_str().unwrap(),
            "--no-resume",
            "metadata",
            "modify",
            "--itemlist",
            fx.dir.path().join("ids.txt").to_str().unwrap(),
            "-m",
            "title:Test",
        ])
        .assert()
        .failure();
    fx.rt.block_on(fx.server.verify());
}

#[test]
fn tasks_submit_no_resume_submits_a_logged_item_again() {
    let fx = no_resume_fixture("task-submit", "item1\n");
    fx.rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/services/tasks.php"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("mock failure"))
            .expect(1)
            .mount(&fx.server)
            .await;
    });
    ia().env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &fx.host,
            "--joblog",
            fx.dir.path().join("log.jsonl").to_str().unwrap(),
            "--no-resume",
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--itemlist",
            fx.dir.path().join("ids.txt").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifiers provided").not());
    fx.rt.block_on(fx.server.verify());
}

/// The default, pinned: without --no-resume a logged item is skipped and
/// only the other one is submitted.
#[test]
fn tasks_submit_resume_skips_a_logged_item() {
    let fx = no_resume_fixture("task-submit", "item1\nitem2\n");
    fx.rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/services/tasks.php"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("mock failure"))
            .expect(1)
            .mount(&fx.server)
            .await;
    });
    ia().env("IA_ACCESS_KEY_ID", "test-access")
        .env("IA_SECRET_ACCESS_KEY", "test-secret")
        .args([
            "--insecure",
            "--host",
            &fx.host,
            "--joblog",
            fx.dir.path().join("log.jsonl").to_str().unwrap(),
            "tasks",
            "submit",
            "--cmd",
            "derive",
            "--itemlist",
            fx.dir.path().join("ids.txt").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipping 1 already-submitted"));
    fx.rt.block_on(fx.server.verify());
}

/// The default for export, against the mock rather than live archive.org:
/// item1 is logged as done and not fetched; item2 is.
#[test]
fn metadata_export_resume_skips_a_logged_item() {
    let fx = no_resume_fixture("export", "item1\nitem2\n");
    fx.rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item1"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(item1_json()))
            .expect(0)
            .mount(&fx.server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/metadata/item2"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(item1_json()))
            .expect(1)
            .mount(&fx.server)
            .await;
    });
    let out = fx.dir.path().join("out.jsonl");
    ia().args([
        "--insecure",
        "--host",
        &fx.host,
        "--joblog",
        fx.dir.path().join("log.jsonl").to_str().unwrap(),
        "metadata",
        "export",
        "--itemlist",
        fx.dir.path().join("ids.txt").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ])
    .assert()
    .success();
    fx.rt.block_on(fx.server.verify());
}

// -- upload: a dead send is abandoned; there is no --min-speed on upload --

#[test]
fn upload_help_keeps_the_dead_send_rule_out_except_what_retries_counts() {
    ia().args(["upload", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--min-speed").not())
        .stdout(predicate::str::contains("moves no bytes").not())
        .stdout(predicate::str::contains("a send dead for 60 s spends one"));
}

#[test]
fn upload_rejects_min_speed() {
    ia().args(["upload", "my-item", "Cargo.toml", "--min-speed", "10K"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unexpected argument"));
}

// -- help states each rule once, on the flag it belongs to (Part 2, PR 2) --

#[test]
fn download_checksum_short_help_says_what_is_checked_without_it() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Verify md5 checksums (without it only the size is checked)",
        ))
        .stdout(predicate::str::contains("slower, reads every local file").not());
}

#[test]
fn download_help_states_the_retry_wait_rule_once() {
    let out = ia().args(["download", "--help"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.matches("up to a cap that doubles").count(),
        1,
        "the retry wait rule belongs on --retries alone:\n{text}"
    );
    assert!(!text.contains("No byte is lost"), "{text}");
    assert!(!text.contains("one more than --retries"), "{text}");
    assert!(!text.contains("own budget of three"), "{text}");
}

#[test]
fn upload_help_has_no_size_recommendation_and_no_restated_flag_block() {
    let out = ia().args(["upload", "--help"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("for large files or unreliable connections"),
        "{text}"
    );
    assert!(!text.contains("5 GB"), "{text}");
    assert!(!text.contains("Integrity & Skip Behavior"), "{text}");
    assert!(!text.contains("automatic with --joblog"), "{text}");
    assert_eq!(
        text.matches("up to a cap that doubles").count(),
        1,
        "the retry wait rule belongs on --retries alone:\n{text}"
    );
}

#[test]
fn search_help_names_no_json_code_in_prose() {
    for backend in ["scrape", "advanced", "fts"] {
        ia().args(["search", backend, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("rate_limited").not())
            .stdout(predicate::str::contains("1 s to 60 s"));
    }
}
