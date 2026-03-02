use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::ai::types::{JoblogChange, JoblogTokens};

/// A single joblog entry (one JSON line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoblogEntry {
    /// ISO 8601 timestamp.
    pub ts: String,
    /// Operation type (e.g. "download", "ai", "ai-undo").
    pub op: String,
    /// Item identifier.
    pub item: String,
    /// File name within the item (empty for AI operations).
    pub file: String,
    /// Status: "ok", "skipped", or "error".
    pub status: String,
    /// Bytes transferred (0 for skipped/error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Wall-clock milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Error message (only for status == "error").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Number of retries attempted (only for errors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<usize>,
    /// Metadata changes (for AI operations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<JoblogChange>>,
    /// Token usage (for AI operations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<JoblogTokens>,
}

/// Thread-safe append-only JSONL writer.
#[derive(Clone)]
pub struct JoblogWriter {
    inner: Arc<Mutex<std::fs::File>>,
    path: PathBuf,
}

impl JoblogWriter {
    /// Open (or create) a joblog file for appending.
    pub fn open(path: &Path) -> crate::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(file)),
            path: path.to_path_buf(),
        })
    }

    /// Write a single entry to the joblog.
    pub fn write(&self, entry: &JoblogEntry) {
        let Ok(mut line) = serde_json::to_string(entry) else {
            warn!("failed to serialize joblog entry");
            return;
        };
        line.push('\n');

        if let Ok(mut f) = self.inner.lock() {
            if let Err(e) = f.write_all(line.as_bytes()) {
                warn!(path = %self.path.display(), error = %e, "failed to write joblog entry");
            }
        }
    }

    /// Path to the joblog file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl JoblogEntry {
    /// Create a new entry with the current timestamp.
    pub fn new(op: &str, item: &str, file: &str) -> Self {
        Self {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            op: op.to_string(),
            item: item.to_string(),
            file: file.to_string(),
            status: String::new(),
            bytes: None,
            elapsed_ms: None,
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        }
    }

    pub fn ok(mut self, bytes: u64, elapsed_ms: u64) -> Self {
        self.status = "ok".to_string();
        self.bytes = Some(bytes);
        self.elapsed_ms = Some(elapsed_ms);
        self
    }

    pub fn skipped(mut self) -> Self {
        self.status = "skipped".to_string();
        self
    }

    pub fn error(mut self, msg: &str, retries: usize) -> Self {
        self.status = "error".to_string();
        self.error = Some(msg.to_string());
        self.retries = Some(retries);
        self
    }

    /// Set metadata changes (for AI operations).
    pub fn with_changes(mut self, changes: Vec<JoblogChange>) -> Self {
        self.changes = Some(changes);
        self
    }

    /// Set token usage (for AI operations).
    pub fn with_tokens(mut self, tokens: JoblogTokens) -> Self {
        self.tokens = Some(tokens);
        self
    }

    /// Create an AI operation entry with status "ok".
    pub fn ai_ok(
        mut self,
        changes: Vec<JoblogChange>,
        tokens: Option<JoblogTokens>,
        elapsed_ms: u64,
    ) -> Self {
        self.status = "ok".to_string();
        self.changes = Some(changes);
        self.tokens = tokens;
        self.elapsed_ms = Some(elapsed_ms);
        self
    }

    /// Create an AI operation entry with status "error".
    pub fn ai_error(mut self, msg: &str, elapsed_ms: u64) -> Self {
        self.status = "error".to_string();
        self.error = Some(msg.to_string());
        self.changes = Some(vec![]);
        self.elapsed_ms = Some(elapsed_ms);
        self
    }
}

/// Read and parse a joblog file.
pub fn read(path: &Path) -> crate::Result<Vec<JoblogEntry>> {
    let content = std::fs::read_to_string(path)?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<JoblogEntry>(trimmed) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                warn!(error = %e, "skipping malformed joblog line");
            }
        }
    }
    Ok(entries)
}

/// Get identifiers of files that failed (for --retry-failed).
pub fn failed_files(entries: &[JoblogEntry]) -> Vec<(String, String)> {
    use std::collections::HashMap;

    // Build a map of (item, file) -> latest status
    let mut latest: HashMap<(String, String), &str> = HashMap::new();
    for entry in entries {
        latest.insert(
            (entry.item.clone(), entry.file.clone()),
            if entry.status == "ok" {
                "ok"
            } else if entry.status == "skipped" {
                "skipped"
            } else {
                "error"
            },
        );
    }

    latest
        .into_iter()
        .filter(|(_, status)| *status == "error")
        .map(|((item, file), _)| (item, file))
        .collect()
}

/// Get unique item identifiers that had any failures.
pub fn failed_items(entries: &[JoblogEntry]) -> Vec<String> {
    let failed = failed_files(entries);
    let mut items: Vec<String> = failed.into_iter().map(|(item, _)| item).collect();
    items.sort();
    items.dedup();
    items
}

/// Summary statistics from a joblog.
#[derive(Debug, Default)]
pub struct JoblogSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// AI-specific summary statistics from a joblog.
#[derive(Debug, Default)]
pub struct AiSummary {
    /// Total AI analysis operations.
    pub items_analyzed: usize,
    /// Items that had changes applied successfully.
    pub items_with_changes: usize,
    /// Items that errored during AI processing.
    pub items_errored: usize,
    /// Items that were skipped.
    pub items_skipped: usize,
    /// Total number of field changes applied.
    pub changes_applied: usize,
    /// Total prompt tokens used.
    pub prompt_tokens: u64,
    /// Total completion tokens used.
    pub completion_tokens: u64,
    /// Number of undo operations performed.
    pub undos: usize,
    /// Number of changes reversed by undo.
    pub changes_reversed: usize,
}

/// Compute summary statistics from entries.
pub fn summarize(entries: &[JoblogEntry]) -> JoblogSummary {
    let mut summary = JoblogSummary {
        total: entries.len(),
        ..Default::default()
    };
    for entry in entries {
        match entry.status.as_str() {
            "ok" => summary.succeeded += 1,
            "error" => summary.failed += 1,
            "skipped" => summary.skipped += 1,
            _ => {}
        }
    }
    summary
}

/// Compute AI-specific summary statistics from entries.
///
/// Returns `None` if there are no AI entries in the joblog.
pub fn ai_summarize(entries: &[JoblogEntry]) -> Option<AiSummary> {
    let ai_entries: Vec<&JoblogEntry> = entries
        .iter()
        .filter(|e| e.op == "ai" || e.op == "ai-undo")
        .collect();

    if ai_entries.is_empty() {
        return None;
    }

    let mut summary = AiSummary::default();

    for entry in &ai_entries {
        match entry.op.as_str() {
            "ai" => match entry.status.as_str() {
                "ok" => {
                    summary.items_analyzed += 1;
                    if let Some(ref changes) = entry.changes {
                        if !changes.is_empty() {
                            summary.items_with_changes += 1;
                            summary.changes_applied += changes.len();
                        }
                    }
                    if let Some(ref tokens) = entry.tokens {
                        summary.prompt_tokens += tokens.prompt;
                        summary.completion_tokens += tokens.completion;
                    }
                }
                "error" => {
                    summary.items_errored += 1;
                    if let Some(ref tokens) = entry.tokens {
                        summary.prompt_tokens += tokens.prompt;
                        summary.completion_tokens += tokens.completion;
                    }
                }
                "skipped" => {
                    summary.items_skipped += 1;
                }
                _ => {}
            },
            "ai-undo" => {
                if entry.status == "ok" {
                    summary.undos += 1;
                    if let Some(ref changes) = entry.changes {
                        summary.changes_reversed += changes.len();
                    }
                }
            }
            _ => {}
        }
    }

    Some(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serializes_to_json() {
        let entry = JoblogEntry::new("download", "nasa", "photo.jpg").ok(4200000, 2100);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"op\":\"download\""));
        assert!(json.contains("\"item\":\"nasa\""));
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"bytes\":4200000"));
        // error and retries should not be present
        assert!(!json.contains("\"error\""));
        assert!(!json.contains("\"retries\""));
    }

    #[test]
    fn error_entry_includes_error_and_retries() {
        let entry =
            JoblogEntry::new("download", "nasa", "video.mp4").error("timeout after 12s", 5);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("\"error\":\"timeout after 12s\""));
        assert!(json.contains("\"retries\":5"));
        // bytes and elapsed should not be present
        assert!(!json.contains("\"bytes\""));
        assert!(!json.contains("\"elapsed_ms\""));
    }

    #[test]
    fn skipped_entry_is_minimal() {
        let entry = JoblogEntry::new("download", "nasa", "thumb.jpg").skipped();
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"status\":\"skipped\""));
        assert!(!json.contains("\"bytes\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn roundtrip_through_json() {
        let entry = JoblogEntry::new("download", "nasa", "photo.jpg").ok(100, 50);
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JoblogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op, "download");
        assert_eq!(parsed.item, "nasa");
        assert_eq!(parsed.file, "photo.jpg");
        assert_eq!(parsed.status, "ok");
        assert_eq!(parsed.bytes, Some(100));
    }

    #[test]
    fn write_and_read_joblog_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let writer = JoblogWriter::open(&path).unwrap();
        writer.write(&JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50));
        writer.write(&JoblogEntry::new("download", "nasa", "b.jpg").error("timeout", 3));
        writer.write(&JoblogEntry::new("download", "nasa", "c.jpg").skipped());

        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, "ok");
        assert_eq!(entries[1].status, "error");
        assert_eq!(entries[2].status, "skipped");
    }

    #[test]
    fn failed_files_returns_latest_status() {
        let entries = vec![
            JoblogEntry::new("download", "nasa", "a.jpg").error("timeout", 3),
            JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50), // retry succeeded
            JoblogEntry::new("download", "nasa", "b.jpg").error("404", 0),
        ];

        let failed = failed_files(&entries);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], ("nasa".to_string(), "b.jpg".to_string()));
    }

    #[test]
    fn summarize_counts_statuses() {
        let entries = vec![
            JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50),
            JoblogEntry::new("download", "nasa", "b.jpg").ok(200, 60),
            JoblogEntry::new("download", "nasa", "c.jpg").error("timeout", 3),
            JoblogEntry::new("download", "nasa", "d.jpg").skipped(),
        ];
        let summary = summarize(&entries);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn read_handles_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let entries = read(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        let content = "{\"ts\":\"2026-02-20T15:30:00Z\",\"op\":\"download\",\"item\":\"nasa\",\"file\":\"a.jpg\",\"status\":\"ok\"}\nnot json\n{\"ts\":\"2026-02-20T15:30:01Z\",\"op\":\"download\",\"item\":\"nasa\",\"file\":\"b.jpg\",\"status\":\"ok\"}\n";
        std::fs::write(&path, content).unwrap();
        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 2);
    }

    // --- AI joblog extension tests ---

    #[test]
    fn ai_entry_serializes_with_changes_and_tokens() {
        let entry = JoblogEntry::new("ai", "nasa_photo", "").ai_ok(
            vec![
                JoblogChange {
                    field: "date".to_string(),
                    old: None,
                    new: serde_json::json!("1969-07-20"),
                },
                JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("nasa photo")),
                    new: serde_json::json!("NASA Photo"),
                },
            ],
            Some(JoblogTokens {
                prompt: 1200,
                completion: 300,
            }),
            2100,
        );

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"op\":\"ai\""));
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"changes\""));
        assert!(json.contains("\"tokens\""));
        assert!(json.contains("\"date\""));
        assert!(json.contains("\"1969-07-20\""));
        assert!(json.contains("\"prompt\":1200"));
    }

    #[test]
    fn ai_error_entry_has_empty_changes() {
        let entry = JoblogEntry::new("ai", "bad_item", "")
            .ai_error("metadata write failed: 403", 500);

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("\"error\":\"metadata write failed: 403\""));
        assert!(json.contains("\"changes\":[]"));
        assert!(!json.contains("\"tokens\""));
    }

    #[test]
    fn ai_entry_roundtrip() {
        let entry = JoblogEntry::new("ai", "test_item", "").ai_ok(
            vec![JoblogChange {
                field: "title".to_string(),
                old: Some(serde_json::json!("old")),
                new: serde_json::json!("new"),
            }],
            Some(JoblogTokens {
                prompt: 500,
                completion: 100,
            }),
            1000,
        );

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JoblogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op, "ai");
        assert_eq!(parsed.item, "test_item");
        assert_eq!(parsed.status, "ok");
        let changes = parsed.changes.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field, "title");
        let tokens = parsed.tokens.unwrap();
        assert_eq!(tokens.prompt, 500);
        assert_eq!(tokens.completion, 100);
    }

    #[test]
    fn backward_compat_download_entry_has_no_ai_fields() {
        let entry = JoblogEntry::new("download", "nasa", "photo.jpg").ok(4200000, 2100);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("\"changes\""));
        assert!(!json.contains("\"tokens\""));
    }

    #[test]
    fn backward_compat_parse_old_download_entries() {
        let old_json = r#"{"ts":"2026-02-20T15:30:00Z","op":"download","item":"nasa","file":"a.jpg","status":"ok","bytes":100,"elapsed_ms":50}"#;
        let entry: JoblogEntry = serde_json::from_str(old_json).unwrap();
        assert_eq!(entry.op, "download");
        assert_eq!(entry.item, "nasa");
        assert!(entry.changes.is_none());
        assert!(entry.tokens.is_none());
    }

    #[test]
    fn ai_entry_with_builder_methods() {
        let entry = JoblogEntry::new("ai", "test", "")
            .with_changes(vec![JoblogChange {
                field: "date".to_string(),
                old: None,
                new: serde_json::json!("2026-01-01"),
            }])
            .with_tokens(JoblogTokens {
                prompt: 800,
                completion: 200,
            });

        assert!(entry.changes.is_some());
        assert!(entry.tokens.is_some());
        assert_eq!(entry.changes.unwrap().len(), 1);
    }

    #[test]
    fn failed_items_includes_ai_ops() {
        let entries = vec![
            JoblogEntry::new("ai", "good_item", "").ai_ok(vec![], None, 100),
            JoblogEntry::new("ai", "bad_item", "")
                .ai_error("write failed", 200),
        ];
        let failed = failed_items(&entries);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], "bad_item");
    }

    #[test]
    fn write_and_read_mixed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.jsonl");

        let writer = JoblogWriter::open(&path).unwrap();
        writer.write(&JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50));
        writer.write(
            &JoblogEntry::new("ai", "nasa", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("old")),
                    new: serde_json::json!("new"),
                }],
                Some(JoblogTokens {
                    prompt: 1000,
                    completion: 200,
                }),
                1500,
            ),
        );

        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].op, "download");
        assert!(entries[0].changes.is_none());
        assert_eq!(entries[1].op, "ai");
        assert!(entries[1].changes.is_some());
    }

    // --- AI summary tests ---

    #[test]
    fn ai_summarize_returns_none_for_no_ai_entries() {
        let entries = vec![
            JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50),
            JoblogEntry::new("download", "nasa", "b.jpg").ok(200, 60),
        ];
        assert!(ai_summarize(&entries).is_none());
    }

    #[test]
    fn ai_summarize_returns_none_for_empty() {
        assert!(ai_summarize(&[]).is_none());
    }

    #[test]
    fn ai_summarize_counts_analyzed_and_changes() {
        let entries = vec![
            JoblogEntry::new("ai", "item1", "").ai_ok(
                vec![
                    JoblogChange {
                        field: "title".to_string(),
                        old: Some(serde_json::json!("old")),
                        new: serde_json::json!("New"),
                    },
                    JoblogChange {
                        field: "date".to_string(),
                        old: None,
                        new: serde_json::json!("2026-01-01"),
                    },
                ],
                Some(JoblogTokens {
                    prompt: 1000,
                    completion: 200,
                }),
                500,
            ),
            JoblogEntry::new("ai", "item2", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("bad")),
                    new: serde_json::json!("Good"),
                }],
                Some(JoblogTokens {
                    prompt: 800,
                    completion: 150,
                }),
                400,
            ),
            // Item with no changes (ok but empty changes)
            JoblogEntry::new("ai", "item3", "").ai_ok(vec![], None, 100),
        ];

        let summary = ai_summarize(&entries).unwrap();
        assert_eq!(summary.items_analyzed, 3);
        assert_eq!(summary.items_with_changes, 2);
        assert_eq!(summary.changes_applied, 3);
        assert_eq!(summary.prompt_tokens, 1800);
        assert_eq!(summary.completion_tokens, 350);
        assert_eq!(summary.items_errored, 0);
        assert_eq!(summary.items_skipped, 0);
        assert_eq!(summary.undos, 0);
    }

    #[test]
    fn ai_summarize_counts_errors_and_skips() {
        let entries = vec![
            JoblogEntry::new("ai", "item1", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: None,
                    new: serde_json::json!("Title"),
                }],
                None,
                100,
            ),
            JoblogEntry::new("ai", "item2", "").ai_error("write failed", 200),
            JoblogEntry::new("ai", "item3", "").skipped(),
        ];

        let summary = ai_summarize(&entries).unwrap();
        assert_eq!(summary.items_analyzed, 1);
        assert_eq!(summary.items_errored, 1);
        assert_eq!(summary.items_skipped, 1);
    }

    #[test]
    fn ai_summarize_counts_undos() {
        let entries = vec![
            JoblogEntry::new("ai", "item1", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("old")),
                    new: serde_json::json!("new"),
                }],
                Some(JoblogTokens {
                    prompt: 500,
                    completion: 100,
                }),
                100,
            ),
            JoblogEntry::new("ai-undo", "item1", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("new")),
                    new: serde_json::json!("old"),
                }],
                None,
                50,
            ),
        ];

        let summary = ai_summarize(&entries).unwrap();
        assert_eq!(summary.items_analyzed, 1);
        assert_eq!(summary.changes_applied, 1);
        assert_eq!(summary.undos, 1);
        assert_eq!(summary.changes_reversed, 1);
        assert_eq!(summary.prompt_tokens, 500);
        assert_eq!(summary.completion_tokens, 100);
    }

    #[test]
    fn ai_summarize_ignores_download_entries() {
        let entries = vec![
            JoblogEntry::new("download", "nasa", "a.jpg").ok(100, 50),
            JoblogEntry::new("ai", "nasa", "").ai_ok(
                vec![JoblogChange {
                    field: "date".to_string(),
                    old: None,
                    new: serde_json::json!("1969-07-20"),
                }],
                Some(JoblogTokens {
                    prompt: 1200,
                    completion: 300,
                }),
                1500,
            ),
            JoblogEntry::new("download", "nasa", "b.jpg").ok(200, 60),
        ];

        let summary = ai_summarize(&entries).unwrap();
        assert_eq!(summary.items_analyzed, 1);
        assert_eq!(summary.changes_applied, 1);
        assert_eq!(summary.prompt_tokens, 1200);
    }
}
