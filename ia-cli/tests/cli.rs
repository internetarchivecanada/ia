use assert_cmd::Command;
use predicates::prelude::*;

fn ia() -> Command {
    assert_cmd::cargo_bin_cmd!("ia")
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
        .stdout(predicate::str::contains("--debug"))
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
        .stdout(predicate::str::contains("--checksum"));
}

#[test]
fn search_subcommand_help() {
    ia().args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--itemlist"))
        .stdout(predicate::str::contains("--num-found"))
        .stdout(predicate::str::contains("--fts"));
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
        .stdout(predicate::str::contains("--pretty"));
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
    ia().args(["--insecure", "--host", "test.archive.org", "download", "--help"])
        .assert()
        .success();
}

#[test]
fn config_file_flag_accepts_path() {
    ia().args(["--config-file", "/nonexistent/path.ini", "download", "--help"])
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
fn metadata_write_flags_in_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--modify"))
        .stdout(predicate::str::contains("--append"))
        .stdout(predicate::str::contains("--append-list"))
        .stdout(predicate::str::contains("--insert"))
        .stdout(predicate::str::contains("--remove"))
        .stdout(predicate::str::contains("--target"))
        .stdout(predicate::str::contains("--expect"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--spreadsheet"))
        .stdout(predicate::str::contains("--priority"))
        .stdout(predicate::str::contains("--reduced-priority"));
}

#[test]
fn metadata_write_short_flags_in_help() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-m"))
        .stdout(predicate::str::contains("-a"))
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-I"))
        .stdout(predicate::str::contains("-r"));
}

#[test]
fn metadata_write_flags_conflict() {
    // --modify and --append should conflict (same ArgGroup)
    ia().args(["metadata", "test", "--modify=title:X", "--append=title:Y"])
        .assert()
        .failure();
}

#[test]
fn metadata_no_identifier_errors() {
    // Read mode with no identifier should fail
    ia().args(["metadata"])
        .assert()
        .failure();
}

#[test]
fn metadata_modify_no_identifier_errors() {
    // Write mode with no identifier should fail
    ia().args(["metadata", "--modify=title:New"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifiers"));
}

#[test]
fn metadata_immutable_field_warning() {
    // Attempting to modify an immutable field should warn (fails with auth error too)
    ia().args(["metadata", "test-item", "--modify=identifier:new_id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("immutable"));
}

#[test]
fn metadata_admin_field_warning() {
    // Attempting to modify an admin-only field should warn
    ia().args(["metadata", "test-item", "--modify=mediatype:audio"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));
}

#[test]
fn metadata_spreadsheet_nonexistent_file_errors() {
    // --spreadsheet with a nonexistent file should report a read error
    ia().args(["metadata", "--spreadsheet=/tmp/nonexistent_ia_test_file.csv"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read spreadsheet"));
}

#[test]
fn metadata_spreadsheet_no_write_op_accepted() {
    // --spreadsheet alone (without explicit write op flag) should be accepted
    // as a write operation, not rejected as "not yet implemented".
    // It will fail with auth error after reading the CSV, which is fine.
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item,New Title\n").unwrap();

    let result = ia()
        .args(["metadata", &format!("--spreadsheet={}", csv_path.display())])
        .assert()
        .failure();

    // Should NOT contain the old "not yet implemented" message
    result.stderr(predicate::str::contains("not yet implemented").not());
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

#[test]
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
        .stdout(predicate::str::contains("ia metadata nasa --json"));
}

#[test]
fn status_help_shows_json_example() {
    ia().args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

fn metadata_spreadsheet_with_append_list() {
    // --spreadsheet combined with --append-list should be accepted.
    // The flag value is ignored; only the op mode (AppendList) is used.
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("test.csv");
    std::fs::write(&csv_path, "identifier,subject\ntest-item,science\n").unwrap();

    ia().args([
        "metadata",
        &format!("--spreadsheet={}", csv_path.display()),
        "--append-list=subject:placeholder",
    ])
    .assert()
    .failure()
    // Should warn that flag values are ignored in spreadsheet mode
    .stderr(predicate::str::contains("write flag values are ignored"))
    // Should fail with auth, not "not yet implemented"
    .stderr(predicate::str::contains("not yet implemented").not());
}
