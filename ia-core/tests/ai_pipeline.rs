//! Integration tests for the AI pipeline and undo functionality.
//!
//! ALL tests use wiremock mocks — ZERO live requests to archive.org or any LLM API.
//! All metadata fixtures and LLM responses are FAKE.

use std::sync::Arc;

use ia_core::ai::pipeline::{PipelineConfig, ReviewMode};
use ia_core::ai::types::{AiConfig, FocusConfig, JoblogChange, JoblogTokens};
use ia_core::ai::undo::undo_from_joblog;
use ia_core::joblog::{self, JoblogEntry, JoblogWriter};
use ia_core::{IaClient, IaConfig};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// =============================================================================
// Test helpers
// =============================================================================

fn mock_config(server_uri: &str) -> IaConfig {
    let mut config = IaConfig::default();
    let host = server_uri
        .strip_prefix("http://")
        .or_else(|| server_uri.strip_prefix("https://"))
        .unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.secure = false;
    config
}

fn mock_config_with_auth(server_uri: &str) -> IaConfig {
    let mut config = mock_config(server_uri);
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    config
}

fn fake_item(identifier: &str) -> serde_json::Value {
    json!({
        "metadata": {
            "identifier": identifier,
            "title": "nasa photo",
            "mediatype": "image",
            "collection": ["test_collection"],
            "date": "July 20 1969"
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    })
}

fn llm_response(changes: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": changes,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 500,
            "completion_tokens": 100,
        }
    })
}

fn llm_changes_json() -> String {
    serde_json::to_string(&json!([
        {
            "field": "title",
            "old_value": "nasa photo",
            "new_value": "NASA Photo",
            "reason": "Fixed capitalization",
            "category": "content"
        },
        {
            "field": "date",
            "old_value": "July 20 1969",
            "new_value": "1969-07-20",
            "reason": "Normalized to ISO 8601",
            "category": "schema"
        }
    ]))
    .unwrap()
}

fn success_response(task_id: u64) -> serde_json::Value {
    json!({
        "success": true,
        "task_id": task_id,
        "log": format!("https://catalogd.archive.org/log/{task_id}")
    })
}

// =============================================================================
// Pipeline integration tests
// =============================================================================

#[tokio::test]
async fn pipeline_headless_dry_run_single_item() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    // Mock IA metadata endpoint
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("test-item")))
        .expect(1)
        .mount(&ia_server)
        .await;

    // Mock LLM chat completions endpoint
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(llm_response(&llm_changes_json())),
        )
        .expect(1)
        .mount(&llm_server)
        .await;

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: None,
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["test-item".to_string()],
        config,
    )
    .await
    .unwrap();

    assert_eq!(summary.items_analyzed, 1);
    assert_eq!(summary.items_with_changes, 1);
    assert_eq!(summary.changes_applied, 2); // title + date
    assert_eq!(summary.items_errored, 0);
    assert_eq!(summary.total_prompt_tokens, 500);
    assert_eq!(summary.total_completion_tokens, 100);
    assert!(summary.elapsed_secs > 0.0);
}

#[tokio::test]
async fn pipeline_headless_dry_run_multiple_items() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    for id in ["item-a", "item-b", "item-c"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_item(id)))
            .mount(&ia_server)
            .await;
    }

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(llm_response(&llm_changes_json())),
        )
        .mount(&llm_server)
        .await;

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 2,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: None,
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec![
            "item-a".to_string(),
            "item-b".to_string(),
            "item-c".to_string(),
        ],
        config,
    )
    .await
    .unwrap();

    assert_eq!(summary.items_analyzed, 3);
    assert_eq!(summary.items_with_changes, 3);
    assert_eq!(summary.changes_applied, 6); // 2 per item
}

#[tokio::test]
async fn pipeline_writes_joblog() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("test-item")))
        .mount(&ia_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(llm_response(&llm_changes_json())),
        )
        .mount(&llm_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");
    let joblog_writer = JoblogWriter::open(&joblog_path).unwrap();

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: Some(joblog_writer),
        output_file: None,
    };

    ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["test-item".to_string()],
        config,
    )
    .await
    .unwrap();

    // Verify joblog was written
    let entries = joblog::read(&joblog_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].op, "ai");
    assert_eq!(entries[0].item, "test-item");
    // Dry-run items are logged as skipped
    assert_eq!(entries[0].status, "skipped");
}

#[tokio::test]
async fn pipeline_source_error_tracked_in_summary() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    // One item succeeds, one fails (404)
    Mock::given(method("GET"))
        .and(path("/metadata/good-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("good-item")))
        .mount(&ia_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/bad-item"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&ia_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(llm_response(&llm_changes_json())),
        )
        .mount(&llm_server)
        .await;

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: None,
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["good-item".to_string(), "bad-item".to_string()],
        config,
    )
    .await
    .unwrap();

    assert_eq!(summary.items_analyzed, 1);
    assert_eq!(summary.items_errored, 1); // bad-item tracked as error
}

#[tokio::test]
async fn pipeline_no_changes_from_llm() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/perfect-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("perfect-item")))
        .mount(&ia_server)
        .await;

    // LLM returns empty array (no changes needed)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(llm_response("[]")))
        .mount(&llm_server)
        .await;

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: None,
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["perfect-item".to_string()],
        config,
    )
    .await
    .unwrap();

    assert_eq!(summary.items_analyzed, 1);
    assert_eq!(summary.items_with_changes, 0);
    assert_eq!(summary.changes_applied, 0);
    assert_eq!(summary.items_skipped, 1);
}

#[tokio::test]
async fn pipeline_record_only_writes_output_file() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("test-item")))
        .mount(&ia_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(llm_response(&llm_changes_json())),
        )
        .mount(&llm_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("output.json");

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::RecordOnly,
        dry_run: false, // record-only skips HTTP write but isn't dry_run
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: Some(output_path.clone()),
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["test-item".to_string()],
        config,
    )
    .await
    .unwrap();

    assert_eq!(summary.items_analyzed, 1);
    assert_eq!(summary.items_with_changes, 1);

    // Verify output file was written
    let content = std::fs::read_to_string(&output_path).unwrap();
    let records: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["identifier"], "test-item");
}

#[tokio::test]
async fn pipeline_llm_error_tracks_item() {
    let ia_server = MockServer::start().await;
    let llm_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_item("test-item")))
        .mount(&ia_server)
        .await;

    // LLM returns a permanent error (401)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&llm_server)
        .await;

    let ia_client = IaClient::from_config(mock_config(&ia_server.uri())).unwrap();

    let config = PipelineConfig {
        ai_config: AiConfig {
            base_url: llm_server.uri(),
            api_key: Some("bad-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        },
        focus: FocusConfig::default(),
        review_mode: ReviewMode::Headless,
        dry_run: true,
        ai_jobs: 1,
        prefetch: 5,
        max_tokens_budget: None,
        joblog_writer: None,
        output_file: None,
    };

    let summary = ia_core::ai::pipeline::run_pipeline(
        Arc::new(ia_client),
        vec!["test-item".to_string()],
        config,
    )
    .await
    .unwrap();

    // LLM error → empty changes → item is tracked but skipped
    assert_eq!(summary.items_analyzed, 1);
    assert_eq!(summary.items_skipped, 1);
    assert_eq!(summary.changes_applied, 0);
}

// =============================================================================
// Undo integration tests
// =============================================================================

#[tokio::test]
async fn undo_dry_run() {
    let server = MockServer::start().await;

    // No HTTP calls expected in dry run
    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");

    // Write a fake AI joblog entry
    let writer = JoblogWriter::open(&joblog_path).unwrap();
    writer.write(
        &JoblogEntry::new("ai", "test-item", "").ai_ok(
            vec![JoblogChange {
                field: "title".to_string(),
                old: Some(json!("old title")),
                new: json!("New Title"),
            }],
            Some(JoblogTokens {
                prompt: 500,
                completion: 100,
            }),
            1000,
        ),
    );

    let summary = undo_from_joblog(&client, &joblog_path, None, true)
        .await
        .unwrap();

    assert_eq!(summary.items_undone, 1);
    assert_eq!(summary.changes_reversed, 1);
    assert_eq!(summary.items_errored, 0);
}

#[tokio::test]
async fn undo_applies_reverse_changes() {
    let server = MockServer::start().await;

    // Mock GET to return current metadata
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {
                "identifier": "test-item",
                "title": "New Title",
                "date": "1969-07-20",
                "mediatype": "image",
                "collection": ["test"]
            },
            "files": [],
            "server": "ia000000.us.archive.org"
        })))
        .mount(&server)
        .await;

    // Mock POST for the undo write
    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "task_id": 99999
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");

    let writer = JoblogWriter::open(&joblog_path).unwrap();
    writer.write(
        &JoblogEntry::new("ai", "test-item", "").ai_ok(
            vec![
                JoblogChange {
                    field: "title".to_string(),
                    old: Some(json!("old title")),
                    new: json!("New Title"),
                },
                JoblogChange {
                    field: "date".to_string(),
                    old: None,
                    new: json!("1969-07-20"),
                },
            ],
            None,
            500,
        ),
    );

    let summary = undo_from_joblog(&client, &joblog_path, None, false)
        .await
        .unwrap();

    assert_eq!(summary.items_undone, 1);
    assert_eq!(summary.changes_reversed, 2);
    assert_eq!(summary.items_errored, 0);
}

#[tokio::test]
async fn undo_skips_non_ai_entries() {
    let server = MockServer::start().await;
    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");

    let writer = JoblogWriter::open(&joblog_path).unwrap();
    // Only download entries - no AI entries
    writer.write(&JoblogEntry::new("download", "item1", "file.jpg").ok(1000, 100));
    writer.write(&JoblogEntry::new("download", "item2", "file.mp4").ok(2000, 200));

    let summary = undo_from_joblog(&client, &joblog_path, None, false)
        .await
        .unwrap();

    // Nothing to undo
    assert_eq!(summary.items_undone, 0);
    assert_eq!(summary.changes_reversed, 0);
}

#[tokio::test]
async fn undo_skips_ai_error_entries() {
    let server = MockServer::start().await;
    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let joblog_path = dir.path().join("test.jsonl");

    let writer = JoblogWriter::open(&joblog_path).unwrap();
    // Only failed AI entries
    writer.write(&JoblogEntry::new("ai", "bad-item", "").ai_error("write failed", 200));

    let summary = undo_from_joblog(&client, &joblog_path, None, false)
        .await
        .unwrap();

    assert_eq!(summary.items_undone, 0);
    assert_eq!(summary.changes_reversed, 0);
}

#[tokio::test]
async fn undo_writes_joblog() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {
                "identifier": "test-item",
                "title": "New Title",
                "mediatype": "image",
                "collection": ["test"]
            },
            "files": [],
            "server": "ia000000.us.archive.org"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "task_id": 10001
        })))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let undo_path = dir.path().join("undo.jsonl");

    // Write source joblog
    let source_writer = JoblogWriter::open(&source_path).unwrap();
    source_writer.write(
        &JoblogEntry::new("ai", "test-item", "").ai_ok(
            vec![JoblogChange {
                field: "title".to_string(),
                old: Some(json!("old title")),
                new: json!("New Title"),
            }],
            None,
            500,
        ),
    );

    // Write undo with logging
    let undo_writer = JoblogWriter::open(&undo_path).unwrap();
    undo_from_joblog(&client, &source_path, Some(&undo_writer), false)
        .await
        .unwrap();

    // Verify undo joblog
    let undo_entries = joblog::read(&undo_path).unwrap();
    assert_eq!(undo_entries.len(), 1);
    assert_eq!(undo_entries[0].op, "ai-undo");
    assert_eq!(undo_entries[0].item, "test-item");
    assert_eq!(undo_entries[0].status, "ok");
}
