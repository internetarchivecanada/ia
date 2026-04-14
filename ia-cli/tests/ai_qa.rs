//! Integration tests for `ia ai qa` and `ia ai config` subcommands.

#![cfg(feature = "alpha")]

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::NamedTempFile;

fn ia() -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    // Remove env vars that might interfere
    cmd.env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET")
        .env_remove("OPENAI_API_KEY")
        .env_remove("IA_AI_API_KEY")
        .env_remove("IA_AI_QA_API_KEY")
        .env_remove("IA_AI_QA_MODEL")
        .env_remove("IA_AI_QA_BASE_URL")
        .env_remove("IA_AI_QA_PROVIDER");
    cmd
}

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = ia();
    cmd.arg("--config-file").arg(config.path());
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

// ── ia ai (top-level) ───────────────────────────────────────────────────

#[test]
fn ai_help_shows_qa_and_config() {
    ia().args(["ai", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("qa"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn ai_requires_subcommand() {
    // `ia ai` with no subcommand should show help or error
    ia().args(["ai"]).assert().failure();
}

// ── ia ai qa ────────────────────────────────────────────────────────────

#[test]
fn ai_qa_help_shows_expected_flags() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--promote"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--confidence"))
        .stdout(predicate::str::contains("--min-field-confidence"))
        .stdout(predicate::str::contains("--search"))
        .stdout(predicate::str::contains("--itemlist"));
}

#[test]
fn ai_qa_no_input_errors() {
    ia().args(["ai", "qa"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identifier").or(predicate::str::contains("no input")));
}

#[test]
fn ai_qa_no_model_no_config_errors() {
    // With no config and no --model, should fail asking for a model
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["ai", "qa", "some-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no LLM"));
}

#[test]
fn ai_qa_no_base_url_no_config_errors() {
    // --model provided but no --base-url and no config
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["ai", "qa", "--model", "test-model", "some-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("base URL").or(predicate::str::contains("base_url")));
}

#[test]
fn ai_qa_no_api_key_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai",
            "qa",
            "--model",
            "test-model",
            "--base-url",
            "https://api.openai.com/v1",
            "some-item",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("API key")
                .or(predicate::str::contains("api_key"))
                .and(predicate::str::contains("[ai-qa]")),
        );
}

#[test]
fn ai_qa_localhost_no_api_key_ok() {
    // Localhost URLs should skip the API key check (may succeed or fail on item fetch,
    // but should NOT fail on "API key" config validation)
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai",
            "qa",
            "--model",
            "llava",
            "--base-url",
            "http://localhost:11434/v1",
            "some-item",
        ])
        .assert()
        .stderr(predicate::str::contains("API key").not());
}

#[test]
fn ai_qa_config_section_fallback() {
    // [ai] section values should be used when [ai-qa] is absent.
    // Gets past config validation; may succeed or fail on item fetch.
    let cfg = NamedTempFile::new().unwrap();
    fs::write(
        cfg.path(),
        "[ai]\nbase_url = https://api.openai.com/v1\nmodel = gpt-4o\napi_key = test-key\n",
    )
    .unwrap();

    ia_with_config(&cfg)
        .args(["ai", "qa", "some-item"])
        .assert()
        .stderr(
            predicate::str::contains("no LLM model")
                .not()
                .and(predicate::str::contains("no LLM base URL").not())
                .and(predicate::str::contains("no LLM API key").not()),
        );
}

#[test]
fn ai_qa_help_shows_provider_flag() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--provider"));
}

// ── ia ai config ────────────────────────────────────────────────────────

#[test]
fn ai_config_help_shows_subcommands() {
    ia().args(["ai", "config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("edit"));
}

#[test]
fn ai_config_show_requires_collection() {
    ia().args(["ai", "config", "show"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("COLLECTION")
                .or(predicate::str::contains("collection"))
                .or(predicate::str::contains("required")),
        );
}

#[test]
fn ai_config_create_requires_collection() {
    ia().args(["ai", "config", "create"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("COLLECTION")
                .or(predicate::str::contains("collection"))
                .or(predicate::str::contains("required")),
        );
}

#[test]
fn ai_config_create_dry_run_shows_defaults() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["ai", "config", "create", "test-collection", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-5-nano"))
        .stdout(predicate::str::contains("pageInfo"))
        .stdout(predicate::str::contains("json_schema"));
}

#[test]
fn ai_config_create_dry_run_json() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai",
            "config",
            "create",
            "test-collection",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"model_name\""))
        .stdout(predicate::str::contains("\"prompt\""))
        .stdout(predicate::str::contains("\"schema\""));
}

#[test]
fn ai_config_create_custom_model() {
    let cfg = empty_config();
    let output = ia_with_config(&cfg)
        .args([
            "ai",
            "config",
            "create",
            "test-collection",
            "--dry-run",
            "--model",
            "custom-model-v2",
        ])
        .assert()
        .success();

    output.stdout(predicate::str::contains("custom-model-v2"));
}

#[test]
fn ai_config_create_from_file() {
    let cfg = empty_config();
    let config_file = NamedTempFile::with_suffix(".json").unwrap();
    fs::write(
        config_file.path(),
        r#"{
            "result": {
                "model_name": "from-file-model",
                "prompt": "test prompt",
                "pageInfo": [{"type": "cover"}],
                "schema": {
                    "format": {
                        "type": "json_schema",
                        "name": "test",
                        "schema": {"type": "object"}
                    }
                }
            }
        }"#,
    )
    .unwrap();

    ia_with_config(&cfg)
        .args([
            "ai",
            "config",
            "create",
            "test-collection",
            "--dry-run",
            "--from-file",
        ])
        .arg(config_file.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("from-file-model"));
}

#[test]
fn ai_config_edit_requires_collection_arg() {
    // Edit without collection argument should fail
    ia().args(["ai", "config", "edit"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("COLLECTION")
                .or(predicate::str::contains("collection"))
                .or(predicate::str::contains("required")),
        );
}

#[test]
fn ai_qa_help_shows_new_flags() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--print-prompt"))
        .stdout(predicate::str::contains("--image-urls"));
}

#[test]
fn ai_qa_print_prompt_conflicts_with_promote() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai",
            "qa",
            "--print-prompt",
            "--promote",
            "--api-key",
            "test",
            "--model",
            "test-model",
            "--base-url",
            "https://api.openai.com/v1",
            "some-item",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--print-prompt")
                .and(predicate::str::contains("--promote"))
                .or(predicate::str::contains("cannot be used with")),
        );
}

// ── ia download --zip-list / --zip-member ───────────────────────────────

#[test]
fn download_zip_list_requires_identifier() {
    ia().args(["download", "--zip-list", "test.zip"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identifier"));
}

#[test]
fn download_zip_member_requires_identifier() {
    ia().args(["download", "--zip-member", "test.zip/path/file.jp2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identifier"));
}

#[test]
fn download_zip_convert_requires_zip_member() {
    // --zip-convert without --zip-member should error
    ia().args(["download", "item", "--zip-convert", "jpg"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("zip-member").or(predicate::str::contains("requires")));
}

#[test]
fn download_help_shows_zip_options() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--zip-list"))
        .stdout(predicate::str::contains("--zip-member"))
        .stdout(predicate::str::contains("--zip-convert"));
}
