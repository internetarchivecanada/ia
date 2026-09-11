use assert_cmd::Command;
use predicates::prelude::*;

fn ia_cmd() -> Command {
    assert_cmd::cargo_bin_cmd!("ia-cli")
}

#[test]
fn download_search_and_itemlist_conflict() {
    ia_cmd()
        .args([
            "download",
            "--search",
            "collection:test",
            "--itemlist",
            "ids.txt",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// Note: `download <name> --search <q>` and `download <name> --itemlist <p>`
// are intentionally NOT conflicts. In batch mode the positional arg is
// treated as a file name applied per item (with optional `{identifier}`
// substitution). See download_search_with_identifier_template for coverage.

#[test]
fn metadata_modify_search_and_itemlist_conflict() {
    ia_cmd()
        .args([
            "metadata",
            "modify",
            "--search",
            "x",
            "--itemlist",
            "f.txt",
            "-m",
            "k:v",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn metadata_modify_identifiers_and_search_conflict() {
    ia_cmd()
        .args([
            "metadata", "modify", "my-item", "--search", "x", "-m", "k:v",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn metadata_export_search_and_itemlist_conflict() {
    ia_cmd()
        .args(["metadata", "export", "--search", "x", "--itemlist", "f.txt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn tasks_submit_identifier_and_search_conflict() {
    ia_cmd()
        .args([
            "tasks", "submit", "my-item", "--search", "x", "--cmd", "derive",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn tasks_submit_search_and_itemlist_conflict() {
    ia_cmd()
        .args([
            "tasks",
            "submit",
            "--search",
            "x",
            "--itemlist",
            "f.txt",
            "--cmd",
            "derive",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
