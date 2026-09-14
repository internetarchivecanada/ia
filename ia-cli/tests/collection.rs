use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::NamedTempFile;

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "[general]\nhost = 127.0.0.1:1\n").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn collection_create_missing_collection_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["collection", "create", "test-col"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--collection"));
}

#[test]
fn collection_create_missing_identifier_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["collection", "create", "--collection", "parent"])
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

// ─── Dry-run ─────────────────────────────────────────────────────────────────

#[test]
fn collection_create_dry_run_minimal() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--collection",
            "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dry-run:")
                .and(predicate::str::contains("identifier:"))
                .and(predicate::str::contains("mediatype:"))
                .and(predicate::str::contains("collection"))
                .and(predicate::str::contains("test-col")),
        );
}

#[test]
fn collection_create_dry_run_with_all_fields() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--title",
            "My Title",
            "-D",
            "My description",
            "--subject",
            "subj",
            "--collection",
            "parent",
            "-m",
            "hidden:true",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dry-run:")
                .and(predicate::str::contains("My Title"))
                .and(predicate::str::contains("My description"))
                .and(predicate::str::contains("subj"))
                .and(predicate::str::contains("parent"))
                .and(predicate::str::contains("hidden:"))
                .and(predicate::str::contains("true")),
        );
}

#[test]
fn collection_create_dry_run_json() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--collection",
            "parent",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"identifier\":\"test-col\"")
                .and(predicate::str::contains("\"mediatype\":\"collection\"")),
        );
}

#[test]
fn collection_create_dry_run_no_auth_needed() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--collection",
            "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run:"));
}

// ─── Image / auth ────────────────────────────────────────────────────────────

#[test]
fn collection_create_nonexistent_image_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
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
        .args(["collection", "create", "test-col", "--collection", "parent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("credentials"));
}

// ─── Short flag -D for description ──────────────────────────────────────────

#[test]
fn collection_create_short_d_for_description() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection",
            "create",
            "test-col",
            "--collection",
            "parent",
            "-D",
            "Short desc",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Short desc"));
}
