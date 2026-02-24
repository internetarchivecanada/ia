use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch, Semaphore};
use tracing::{debug, error, warn};

use crate::ai::client::LlmClient;
use crate::ai::prompt;
use crate::ai::types::{
    AiConfig, ApplyResult, ApplyStatus, ChangeCategory, ChangeStatus, FocusConfig,
    ItemAnalysis, JoblogChange, JoblogTokens, MetadataChange,
};
use crate::client::IaClient;
use crate::joblog::{JoblogEntry, JoblogWriter};
use crate::metadata::write::{MetadataOp, ModifyRequest};

/// How the reviewer stage should handle suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// Auto-accept all suggestions, output JSONL to stdout.
    Headless,
    /// TUI review (placeholder — completed in Batch 4).
    Interactive,
    /// TUI review, but save to file instead of writing to IA.
    RecordOnly,
}

/// Summary of a completed pipeline run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineSummary {
    pub items_analyzed: u64,
    pub items_with_changes: u64,
    pub items_skipped: u64,
    pub items_errored: u64,
    pub changes_applied: u64,
    pub changes_rejected: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub elapsed_secs: f64,
}

/// Configuration for a pipeline run.
pub struct PipelineConfig {
    pub ai_config: AiConfig,
    pub focus: FocusConfig,
    pub review_mode: ReviewMode,
    pub dry_run: bool,
    pub ai_jobs: usize,
    pub prefetch: usize,
    pub max_tokens_budget: Option<u64>,
    pub joblog_writer: Option<JoblogWriter>,
    /// Output file for record-only mode (JSON array of changes).
    pub output_file: Option<std::path::PathBuf>,
}

/// Run the four-stage AI pipeline.
///
/// 1. Source: fetch metadata for each identifier
/// 2. Analyzer: call LLM to get suggestions
/// 3. Reviewer: headless auto-accept or interactive TUI
/// 4. Writer: apply accepted changes via metadata::modify()
pub async fn run_pipeline(
    client: Arc<IaClient>,
    identifiers: Vec<String>,
    config: PipelineConfig,
) -> crate::Result<PipelineSummary> {
    let start = Instant::now();

    // Shutdown signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Stage channels
    let (source_tx, source_rx) = mpsc::channel(config.prefetch);
    let (analyzer_tx, analyzer_rx) = mpsc::channel(config.prefetch);
    let (writer_tx, writer_rx) = mpsc::channel(32);

    // Shared state
    let llm_client = Arc::new(LlmClient::new(config.ai_config.clone())?);
    let focus = Arc::new(config.focus);
    let system_prompt = Arc::new(prompt::build_system_prompt(&focus)?);
    let ai_jobs_semaphore = Arc::new(Semaphore::new(config.ai_jobs));
    let dry_run = config.dry_run;
    let review_mode = config.review_mode;
    let joblog_writer = config.joblog_writer.clone();
    let max_tokens_budget = config.max_tokens_budget;
    let output_file = config.output_file.clone();
    let record_only = review_mode == ReviewMode::RecordOnly;

    // Stage 1: Source — fetch metadata
    let source_client = client.clone();
    let source_shutdown = shutdown_rx.clone();
    let source_errors = Arc::new(AtomicU64::new(0));
    let source_errors_clone = source_errors.clone();
    let source_handle = tokio::spawn(async move {
        run_source(source_client, identifiers, source_tx, source_shutdown, source_errors_clone).await;
    });

    // Stage 2: Analyzer — call LLM
    let analyzer_shutdown = shutdown_rx.clone();
    let analyzer_handle = tokio::spawn(async move {
        run_analyzer(
            llm_client,
            system_prompt,
            ai_jobs_semaphore,
            source_rx,
            analyzer_tx,
            analyzer_shutdown,
            max_tokens_budget,
        )
        .await;
    });

    // Stage 3: Reviewer — auto-accept (headless) or placeholder
    let reviewer_shutdown = shutdown_rx.clone();
    let reviewer_handle = tokio::spawn(async move {
        run_reviewer(review_mode, analyzer_rx, writer_tx, reviewer_shutdown).await;
    });

    // Stage 4: Writer — apply changes (or save to file in record-only mode)
    let writer_client = client.clone();
    let writer_shutdown = shutdown_rx.clone();
    let writer_handle = tokio::spawn(async move {
        run_writer(
            writer_client,
            dry_run || record_only,
            joblog_writer,
            writer_rx,
            writer_shutdown,
            output_file,
        )
        .await
    });

    // Wait for all stages
    let _ = source_handle.await;
    let _ = analyzer_handle.await;
    let _ = reviewer_handle.await;
    let summary_data = writer_handle.await.map_err(|e| {
        crate::error::IaError::Config(format!("pipeline writer task panicked: {}", e))
    })?;

    // Signal shutdown (cleanup)
    let _ = shutdown_tx.send(true);

    let mut summary = summary_data;
    summary.elapsed_secs = start.elapsed().as_secs_f64();
    summary.items_errored += source_errors.load(Ordering::Relaxed);

    Ok(summary)
}

/// Stage 1: Fetch metadata for each identifier and send to channel.
async fn run_source(
    client: Arc<IaClient>,
    identifiers: Vec<String>,
    tx: mpsc::Sender<(String, serde_json::Value)>,
    shutdown: watch::Receiver<bool>,
    error_count: Arc<AtomicU64>,
) {
    for identifier in identifiers {
        if *shutdown.borrow() {
            break;
        }

        debug!(identifier = %identifier, "fetching metadata");
        match client.get_item(&identifier).await {
            Ok(meta) => {
                let json = match serde_json::to_value(&meta) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(identifier = %identifier, error = %e, "failed to serialize metadata");
                        error_count.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                if tx.send((identifier, json)).await.is_err() {
                    break; // Channel closed
                }
            }
            Err(e) => {
                warn!(identifier = %identifier, error = %e, "failed to fetch metadata");
                error_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Stage 2: Call LLM to analyze metadata and produce change suggestions.
async fn run_analyzer(
    llm: Arc<LlmClient>,
    system_prompt: Arc<String>,
    semaphore: Arc<Semaphore>,
    mut rx: mpsc::Receiver<(String, serde_json::Value)>,
    tx: mpsc::Sender<ItemAnalysis>,
    mut shutdown: watch::Receiver<bool>,
    max_tokens_budget: Option<u64>,
) {
    let total_tokens = Arc::new(AtomicU64::new(0));

    let mut join_set = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            item = rx.recv() => {
                let Some((identifier, metadata)) = item else { break; };

                // Check token budget
                if let Some(budget) = max_tokens_budget {
                    if total_tokens.load(Ordering::Relaxed) >= budget {
                        debug!("token budget exhausted, stopping analyzer");
                        break;
                    }
                }

                if *shutdown.borrow() { break; }

                let llm = llm.clone();
                let prompt = system_prompt.clone();
                let tx = tx.clone();
                let sem = semaphore.clone();
                let tokens = total_tokens.clone();

                join_set.spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    let user_msg = prompt::build_user_message(&metadata);
                    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

                    match llm.chat(&prompt, &user_msg).await {
                        Ok(response) => {
                            // Track token usage
                            if let Some(ref usage) = response.token_usage {
                                tokens.fetch_add(
                                    usage.prompt_tokens + usage.completion_tokens,
                                    Ordering::Relaxed,
                                );
                            }

                            let changes = parse_llm_changes(&response.content);
                            let analysis = ItemAnalysis {
                                identifier,
                                metadata,
                                changes,
                                token_usage: response.token_usage,
                                analyzed_at: now,
                            };
                            let _ = tx.send(analysis).await;
                        }
                        Err(e) => {
                            error!(identifier = %identifier, error = %e, "LLM analysis failed");
                            // Send empty analysis so the item is tracked
                            let analysis = ItemAnalysis {
                                identifier,
                                metadata,
                                changes: vec![],
                                token_usage: None,
                                analyzed_at: now,
                            };
                            let _ = tx.send(analysis).await;
                        }
                    }
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }

    // Wait for in-flight tasks
    while join_set.join_next().await.is_some() {}
}

/// Parse the LLM response content into MetadataChange structs.
///
/// The LLM should return a JSON array of change objects. This function
/// handles common issues like markdown code fences and malformed JSON.
pub fn parse_llm_changes(content: &str) -> Vec<MetadataChange> {
    // Strip markdown code fences if present
    let trimmed = content.trim();
    let json_str = if trimmed.starts_with("```") {
        // Remove first and last lines (fences)
        let lines: Vec<&str> = trimmed.lines().collect();
        if lines.len() >= 2 {
            lines[1..lines.len() - 1].join("\n")
        } else {
            trimmed.to_string()
        }
    } else {
        trimmed.to_string()
    };

    // Parse as JSON array
    let parsed: Vec<serde_json::Value> = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to parse LLM response as JSON array");
            return vec![];
        }
    };

    parsed
        .into_iter()
        .filter_map(|v| {
            let field = v.get("field")?.as_str()?.to_string();
            let new_value = v.get("new_value").cloned()?;
            let old_value = v.get("old_value").cloned().and_then(|v| {
                if v.is_null() { None } else { Some(v) }
            });
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let category = v
                .get("category")
                .and_then(|c| c.as_str())
                .and_then(parse_category)
                .unwrap_or(ChangeCategory::Content);

            Some(MetadataChange {
                field,
                old_value,
                new_value,
                reason,
                category,
                status: ChangeStatus::Pending,
            })
        })
        .collect()
}

fn parse_category(s: &str) -> Option<ChangeCategory> {
    match s {
        "schema" => Some(ChangeCategory::Schema),
        "content" => Some(ChangeCategory::Content),
        "cross_field" => Some(ChangeCategory::CrossField),
        "missing_field" => Some(ChangeCategory::MissingField),
        _ => None,
    }
}

/// Stage 3: Review suggestions (headless auto-accepts, interactive is a placeholder).
async fn run_reviewer(
    mode: ReviewMode,
    mut rx: mpsc::Receiver<ItemAnalysis>,
    tx: mpsc::Sender<ItemAnalysis>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            item = rx.recv() => {
                let Some(mut analysis) = item else { break; };

                if *shutdown.borrow() { break; }

                match mode {
                    ReviewMode::Headless | ReviewMode::RecordOnly => {
                        // Auto-accept all changes
                        for change in &mut analysis.changes {
                            change.status = ChangeStatus::Accepted;
                        }
                        // Print JSONL to stdout in headless mode
                        if mode == ReviewMode::Headless && !analysis.changes.is_empty() {
                            let output = serde_json::json!({
                                "identifier": analysis.identifier,
                                "changes": analysis.changes,
                            });
                            println!("{}", serde_json::to_string(&output).unwrap_or_default());
                        }
                    }
                    ReviewMode::Interactive => {
                        // Interactive TUI is not yet wired in.
                        // This code path should not be reached — the CLI
                        // validates and rejects interactive mode before
                        // calling run_pipeline. If it somehow gets here,
                        // auto-accept so the pipeline doesn't hang.
                        warn!("interactive review not yet implemented, auto-accepting changes");
                        for change in &mut analysis.changes {
                            change.status = ChangeStatus::Accepted;
                        }
                    }
                }

                if tx.send(analysis).await.is_err() {
                    break;
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }
}

/// Stage 4: Apply accepted changes and log to joblog.
async fn run_writer(
    client: Arc<IaClient>,
    dry_run: bool,
    joblog_writer: Option<JoblogWriter>,
    mut rx: mpsc::Receiver<ItemAnalysis>,
    mut shutdown: watch::Receiver<bool>,
    output_file: Option<std::path::PathBuf>,
) -> PipelineSummary {
    let mut summary = PipelineSummary::default();
    let mut record_entries: Vec<serde_json::Value> = Vec::new();

    loop {
        tokio::select! {
            item = rx.recv() => {
                let Some(analysis) = item else { break; };

                if *shutdown.borrow() { break; }

                let result = process_item(&client, &analysis, dry_run).await;

                // Update summary
                summary.items_analyzed += 1;
                if let Some(ref usage) = analysis.token_usage {
                    summary.total_prompt_tokens += usage.prompt_tokens;
                    summary.total_completion_tokens += usage.completion_tokens;
                }

                match result.status {
                    ApplyStatus::Ok => {
                        summary.items_with_changes += 1;
                        summary.changes_applied += result.changes_applied.len() as u64;
                    }
                    ApplyStatus::Skipped | ApplyStatus::DryRun => {
                        if result.changes_applied.is_empty() {
                            summary.items_skipped += 1;
                        } else {
                            summary.items_with_changes += 1;
                            summary.changes_applied += result.changes_applied.len() as u64;
                        }
                    }
                    ApplyStatus::Error => {
                        summary.items_errored += 1;
                    }
                }

                // Count rejected changes
                for change in &analysis.changes {
                    if change.status == ChangeStatus::Rejected {
                        summary.changes_rejected += 1;
                    }
                }

                // Collect for record-only output
                if output_file.is_some() && !result.changes_applied.is_empty() {
                    record_entries.push(serde_json::json!({
                        "identifier": analysis.identifier,
                        "changes": result.changes_applied,
                    }));
                }

                // Write to joblog
                if let Some(ref writer) = joblog_writer {
                    write_joblog_entry(writer, &analysis, &result);
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }

    // Write record-only output file
    if let Some(ref path) = output_file {
        if let Ok(json) = serde_json::to_string_pretty(&record_entries) {
            if let Err(e) = std::fs::write(path, json) {
                error!(path = %path.display(), error = %e, "failed to write output file");
            }
        }
    }

    summary
}

/// Process a single item: apply accepted changes.
async fn process_item(
    client: &IaClient,
    analysis: &ItemAnalysis,
    dry_run: bool,
) -> ApplyResult {
    let accepted: Vec<&MetadataChange> = analysis
        .changes
        .iter()
        .filter(|c| matches!(c.status, ChangeStatus::Accepted | ChangeStatus::Edited(_)))
        .collect();

    if accepted.is_empty() {
        return ApplyResult {
            identifier: analysis.identifier.clone(),
            changes_applied: vec![],
            status: ApplyStatus::Skipped,
            error: None,
        };
    }

    if dry_run {
        return ApplyResult {
            identifier: analysis.identifier.clone(),
            changes_applied: accepted.into_iter().cloned().collect(),
            status: ApplyStatus::DryRun,
            error: None,
        };
    }

    // Build changes for metadata::modify()
    let changes: Vec<(String, serde_json::Value)> = accepted
        .iter()
        .map(|c| {
            let value = match &c.status {
                ChangeStatus::Edited(v) => v.clone(),
                _ => c.new_value.clone(),
            };
            (c.field.clone(), value)
        })
        .collect();

    let request = ModifyRequest {
        identifier: analysis.identifier.clone(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: Some(-5), // Batch priority
        reduced_priority: true,
    };

    match crate::metadata::write::modify(client, &request).await {
        Ok(response) => {
            if response.success {
                ApplyResult {
                    identifier: analysis.identifier.clone(),
                    changes_applied: accepted.into_iter().cloned().collect(),
                    status: ApplyStatus::Ok,
                    error: None,
                }
            } else {
                ApplyResult {
                    identifier: analysis.identifier.clone(),
                    changes_applied: vec![],
                    status: ApplyStatus::Error,
                    error: response.error,
                }
            }
        }
        Err(e) => ApplyResult {
            identifier: analysis.identifier.clone(),
            changes_applied: vec![],
            status: ApplyStatus::Error,
            error: Some(e.to_string()),
        },
    }
}

/// Write a joblog entry for an AI operation.
fn write_joblog_entry(
    writer: &JoblogWriter,
    analysis: &ItemAnalysis,
    result: &ApplyResult,
) {
    let changes: Vec<JoblogChange> = result
        .changes_applied
        .iter()
        .map(|c| {
            // Use the actual applied value (may differ from new_value if edited)
            let applied_value = match &c.status {
                ChangeStatus::Edited(v) => v.clone(),
                _ => c.new_value.clone(),
            };
            JoblogChange {
                field: c.field.clone(),
                old: c.old_value.clone(),
                new: applied_value,
            }
        })
        .collect();

    let tokens = analysis.token_usage.as_ref().map(|u| JoblogTokens {
        prompt: u.prompt_tokens,
        completion: u.completion_tokens,
    });

    let entry = match result.status {
        ApplyStatus::Ok => {
            JoblogEntry::new("ai", &analysis.identifier, "")
                .ai_ok(changes, tokens, 0)
        }
        ApplyStatus::Skipped | ApplyStatus::DryRun => {
            JoblogEntry::new("ai", &analysis.identifier, "").skipped()
        }
        ApplyStatus::Error => {
            let msg = result.error.as_deref().unwrap_or("unknown error");
            JoblogEntry::new("ai", &analysis.identifier, "")
                .ai_error(msg, 0)
        }
    };

    writer.write(&entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_llm_changes_valid_json() {
        let content = r#"[
            {
                "field": "title",
                "old_value": "nasa photo",
                "new_value": "NASA Photo",
                "reason": "Fixed capitalization",
                "category": "content"
            },
            {
                "field": "date",
                "old_value": null,
                "new_value": "1969-07-20",
                "reason": "Extracted from title",
                "category": "cross_field"
            }
        ]"#;

        let changes = parse_llm_changes(content);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].field, "title");
        assert_eq!(changes[0].old_value, Some(serde_json::json!("nasa photo")));
        assert_eq!(changes[0].new_value, serde_json::json!("NASA Photo"));
        assert_eq!(changes[0].category, ChangeCategory::Content);
        assert_eq!(changes[0].status, ChangeStatus::Pending);
        assert_eq!(changes[1].field, "date");
        assert!(changes[1].old_value.is_none());
        assert_eq!(changes[1].category, ChangeCategory::CrossField);
    }

    #[test]
    fn parse_llm_changes_with_code_fences() {
        let content = "```json\n[\n{\"field\": \"title\", \"new_value\": \"Test\", \"reason\": \"fix\", \"category\": \"content\"}\n]\n```";
        let changes = parse_llm_changes(content);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field, "title");
    }

    #[test]
    fn parse_llm_changes_empty_array() {
        let changes = parse_llm_changes("[]");
        assert!(changes.is_empty());
    }

    #[test]
    fn parse_llm_changes_malformed_json() {
        let changes = parse_llm_changes("not valid json");
        assert!(changes.is_empty());
    }

    #[test]
    fn parse_llm_changes_missing_required_fields() {
        let content = r#"[{"reason": "no field or new_value"}]"#;
        let changes = parse_llm_changes(content);
        assert!(changes.is_empty());
    }

    #[test]
    fn parse_llm_changes_unknown_category_defaults_to_content() {
        let content = r#"[{"field": "title", "new_value": "Test", "reason": "fix", "category": "unknown_cat"}]"#;
        let changes = parse_llm_changes(content);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].category, ChangeCategory::Content);
    }

    #[test]
    fn parse_llm_changes_no_category_defaults_to_content() {
        let content = r#"[{"field": "title", "new_value": "Test", "reason": "fix"}]"#;
        let changes = parse_llm_changes(content);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].category, ChangeCategory::Content);
    }

    #[test]
    fn parse_llm_changes_all_categories() {
        let content = r#"[
            {"field": "a", "new_value": "1", "reason": "", "category": "schema"},
            {"field": "b", "new_value": "2", "reason": "", "category": "content"},
            {"field": "c", "new_value": "3", "reason": "", "category": "cross_field"},
            {"field": "d", "new_value": "4", "reason": "", "category": "missing_field"}
        ]"#;
        let changes = parse_llm_changes(content);
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].category, ChangeCategory::Schema);
        assert_eq!(changes[1].category, ChangeCategory::Content);
        assert_eq!(changes[2].category, ChangeCategory::CrossField);
        assert_eq!(changes[3].category, ChangeCategory::MissingField);
    }

    #[test]
    fn pipeline_summary_default() {
        let summary = PipelineSummary::default();
        assert_eq!(summary.items_analyzed, 0);
        assert_eq!(summary.changes_applied, 0);
        assert_eq!(summary.elapsed_secs, 0.0);
    }
}
