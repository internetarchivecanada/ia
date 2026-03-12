use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::NamedTempFile;

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn collection_create_missing_title_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--title"));
}

#[test]
fn collection_create_missing_description_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--subject",
            "subj",
            "--collection",
            "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--description"));
}

#[test]
fn collection_create_missing_subject_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--collection",
            "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--subject"));
}

#[test]
fn collection_create_missing_collection_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--collection"));
}

#[test]
fn collection_create_missing_identifier_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
        ])
        .assert()
        .failure();
}

#[test]
fn collection_create_invalid_metadata_format() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "-m",
            "no-colon",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid KEY:VALUE format"));
}

#[test]
fn collection_create_dry_run_no_auth_needed() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run:"));
}

#[test]
fn collection_create_dry_run_json() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"identifier\":\"test-col\""));
}

#[test]
fn collection_create_nonexistent_image_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "--image",
            "/tmp/ia-test-nonexistent-image.png",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn collection_alias_col_works() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "col",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run:"));
}

#[test]
fn collection_create_no_auth_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "Title",
            "--description",
            "desc",
            "--subject",
            "subj",
            "--collection",
            "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("credentials"));
}
