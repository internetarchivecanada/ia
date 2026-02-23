use assert_cmd::Command;
use predicates::prelude::*;

fn ia() -> Command {
    assert_cmd::cargo_bin_cmd!("ia")
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
