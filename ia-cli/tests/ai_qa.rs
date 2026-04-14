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

// ── ia ai qa -o / --from-results ────────────────────────────────────────

#[test]
fn ai_qa_help_shows_output_and_from_results_flags() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--from-results"));
}

#[test]
fn ai_qa_from_results_conflicts_with_search() {
    let cfg = empty_config();
    let results_file = NamedTempFile::with_suffix(".jsonl").unwrap();
    fs::write(results_file.path(), "").unwrap();

    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(results_file.path())
        .args(["--search", "collection:test"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--from-results").and(predicate::str::contains("--search")),
        );
}

#[test]
fn ai_qa_from_results_conflicts_with_itemlist() {
    let cfg = empty_config();
    let results_file = NamedTempFile::with_suffix(".jsonl").unwrap();
    fs::write(results_file.path(), "").unwrap();
    let itemlist = NamedTempFile::new().unwrap();
    fs::write(itemlist.path(), "item1\n").unwrap();

    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(results_file.path())
        .args(["--itemlist"])
        .arg(itemlist.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--from-results").and(predicate::str::contains("--itemlist")),
        );
}

#[test]
fn ai_qa_from_results_conflicts_with_identifiers() {
    let cfg = empty_config();
    let results_file = NamedTempFile::with_suffix(".jsonl").unwrap();
    fs::write(results_file.path(), "").unwrap();

    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(results_file.path())
        .args(["some-item"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--from-results").and(predicate::str::contains("identifiers")),
        );
}

#[test]
fn ai_qa_from_results_conflicts_with_promote() {
    let cfg = empty_config();
    let results_file = NamedTempFile::with_suffix(".jsonl").unwrap();
    fs::write(results_file.path(), "").unwrap();

    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(results_file.path())
        .args(["--promote"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--from-results").and(predicate::str::contains("--promote")),
        );
}

#[test]
fn ai_qa_unsupported_output_extension() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai",
            "qa",
            "-o",
            "results.pdf",
            "--model",
            "test",
            "--base-url",
            "http://localhost:11434/v1",
            "some-item",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported output format"));
}

#[test]
fn ai_qa_from_results_reads_and_formats() {
    use tempfile::TempDir;

    let cfg = empty_config();
    let dir = TempDir::new().unwrap();

    // Create a valid JSONL file with a QaResult
    let jsonl_path = dir.path().join("input.jsonl");
    let qa_result = serde_json::json!({
        "identifier": "test-item-123",
        "overall_confidence": 0.92,
        "verdict": "pass",
        "extraction_model": "gpt-5-nano",
        "qa_model": "claude-sonnet-4-6",
        "fields": {
            "title": {
                "extracted_value": "My Book",
                "verdict": "correct",
                "confidence": 0.95
            }
        },
        "token_usage": null,
        "elapsed_ms": 1000,
        "existing_metadata": {
            "title": "My Book"
        },
        "pages_sent": [
            {"leaf_num": 0, "page_type": "cover"},
            {"leaf_num": 3, "page_type": "title"}
        ]
    });
    fs::write(&jsonl_path, serde_json::to_string(&qa_result).unwrap()).unwrap();

    // --from-results → --json to stdout
    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_path)
        .args(["--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test-item-123"));

    // --from-results → -o XLSX
    let xlsx_path = dir.path().join("output.xlsx");
    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_path)
        .args(["-o"])
        .arg(&xlsx_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Wrote"));

    assert!(xlsx_path.exists());

    // --from-results → -o CSV
    let csv_path = dir.path().join("output.csv");
    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_path)
        .args(["-o"])
        .arg(&csv_path)
        .assert()
        .success();

    let csv_content = fs::read_to_string(&csv_path).unwrap();
    assert!(csv_content.contains("identifier"));
    assert!(csv_content.contains("test-item-123"));
    assert!(csv_content.contains("correct"));
}

#[test]
fn ai_qa_from_results_backward_compat_old_format() {
    // Old-format JSONL (no existing_metadata, no pages_sent) should work
    let cfg = empty_config();
    let dir = tempfile::TempDir::new().unwrap();

    let jsonl_path = dir.path().join("old.jsonl");
    let old_result = serde_json::json!({
        "identifier": "old-item",
        "overall_confidence": 0.85,
        "verdict": "pass",
        "extraction_model": "gpt-5-nano",
        "qa_model": "claude-sonnet-4-6",
        "fields": {
            "date": {
                "extracted_value": "1990",
                "verdict": "correct",
                "confidence": 0.9
            }
        },
        "token_usage": null,
        "elapsed_ms": 500
    });
    fs::write(&jsonl_path, serde_json::to_string(&old_result).unwrap()).unwrap();

    // Should succeed with --json
    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_path)
        .args(["--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("old-item"));

    // Should succeed writing XLSX
    let xlsx_path = dir.path().join("old.xlsx");
    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_path)
        .args(["-o"])
        .arg(&xlsx_path)
        .assert()
        .success();
    assert!(xlsx_path.exists());
}

#[test]
fn ai_qa_from_results_multiple_outputs() {
    let cfg = empty_config();
    let dir = tempfile::TempDir::new().unwrap();

    let jsonl_input = dir.path().join("input.jsonl");
    let result = serde_json::json!({
        "identifier": "multi-out",
        "overall_confidence": 0.9,
        "verdict": "pass",
        "extraction_model": "gpt-5-nano",
        "qa_model": "claude-sonnet-4-6",
        "fields": {"title": {"extracted_value": "Test", "verdict": "correct", "confidence": 0.95}},
        "token_usage": null,
        "elapsed_ms": 100
    });
    fs::write(&jsonl_input, serde_json::to_string(&result).unwrap()).unwrap();

    let xlsx_path = dir.path().join("out.xlsx");
    let jsonl_path = dir.path().join("out.jsonl");

    ia_with_config(&cfg)
        .args(["ai", "qa", "--from-results"])
        .arg(&jsonl_input)
        .args(["-o"])
        .arg(&xlsx_path)
        .args(["-o"])
        .arg(&jsonl_path)
        .assert()
        .success();

    assert!(xlsx_path.exists());
    assert!(jsonl_path.exists());
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
