use std::path::Path;

use tracing::{debug, warn};

use crate::ai::types::JoblogChange;
use crate::client::IaClient;
use crate::error::Result;
use crate::joblog::{self, JoblogEntry, JoblogWriter};
use crate::metadata::write::{MetadataOp, ModifyRequest};

/// Result of an undo operation.
#[derive(Debug)]
pub struct UndoSummary {
    pub items_undone: u64,
    pub items_skipped: u64,
    pub items_errored: u64,
    pub changes_reversed: u64,
}

/// Undo AI changes recorded in a joblog file.
///
/// Reads the joblog, filters for `op: "ai"` entries with `status: "ok"`,
/// reverses each change by swapping old/new values, and applies via
/// `metadata::modify()`. Uses `--expect` for optimistic concurrency:
/// if the current value doesn't match what we set, the item was modified
/// since and we skip it.
///
/// Logs undo operations with `op: "ai-undo"`.
pub async fn undo_from_joblog(
    client: &IaClient,
    joblog_path: &Path,
    undo_log_writer: Option<&JoblogWriter>,
    dry_run: bool,
) -> Result<UndoSummary> {
    let entries = joblog::read(joblog_path)?;

    // Filter for successful AI operations
    let ai_entries: Vec<&JoblogEntry> = entries
        .iter()
        .filter(|e| e.op == "ai" && e.status == "ok" && e.changes.is_some())
        .collect();

    let mut summary = UndoSummary {
        items_undone: 0,
        items_skipped: 0,
        items_errored: 0,
        changes_reversed: 0,
    };

    for entry in ai_entries {
        let changes = entry.changes.as_ref().unwrap();
        if changes.is_empty() {
            summary.items_skipped += 1;
            continue;
        }

        debug!(item = %entry.item, changes = changes.len(), "undoing AI changes");

        // Build reverse changes: swap old ↔ new
        let reverse_changes: Vec<(String, serde_json::Value)> = changes
            .iter()
            .map(|c| {
                // old becomes the new value (what we want to restore)
                // If old was None/null, we should remove the field
                let restore_value = match &c.old {
                    Some(v) if !v.is_null() => v.clone(),
                    _ => {
                        // Field didn't exist before — set to REMOVE_TAG to delete
                        serde_json::Value::String(
                            crate::metadata::write::REMOVE_TAG.to_string(),
                        )
                    }
                };
                (c.field.clone(), restore_value)
            })
            .collect();

        // Build expect map: current value should match what we set
        let mut expect = std::collections::HashMap::new();
        for c in changes {
            expect.insert(c.field.clone(), c.new.clone());
        }

        if dry_run {
            summary.items_undone += 1;
            summary.changes_reversed += reverse_changes.len() as u64;
            continue;
        }

        let request = ModifyRequest {
            identifier: entry.item.clone(),
            changes: reverse_changes.clone(),
            op: MetadataOp::Set,
            target: "metadata".to_string(),
            expect: Some(expect),
            priority: Some(-5),
            reduced_priority: true,
        };

        match crate::metadata::write::modify(client, &request).await {
            Ok(response) => {
                if response.success {
                    summary.items_undone += 1;
                    summary.changes_reversed += reverse_changes.len() as u64;

                    if let Some(writer) = undo_log_writer {
                        let undo_changes: Vec<JoblogChange> = changes
                            .iter()
                            .map(|c| JoblogChange {
                                field: c.field.clone(),
                                old: Some(c.new.clone()),
                                new: c.old.clone().unwrap_or(serde_json::Value::Null),
                            })
                            .collect();
                        let undo_entry =
                            JoblogEntry::new("ai-undo", &entry.item, "")
                                .ai_ok(undo_changes, None, 0);
                        writer.write(&undo_entry);
                    }
                } else {
                    let error_msg = response
                        .error
                        .unwrap_or_else(|| "unknown error".to_string());
                    warn!(item = %entry.item, error = %error_msg, "undo failed");
                    summary.items_errored += 1;

                    if let Some(writer) = undo_log_writer {
                        let undo_entry =
                            JoblogEntry::new("ai-undo", &entry.item, "")
                                .ai_error(&error_msg, 0);
                        writer.write(&undo_entry);
                    }
                }
            }
            Err(e) => {
                warn!(item = %entry.item, error = %e, "undo request failed");
                summary.items_errored += 1;

                if let Some(writer) = undo_log_writer {
                    let undo_entry = JoblogEntry::new("ai-undo", &entry.item, "")
                        .ai_error(&e.to_string(), 0);
                    writer.write(&undo_entry);
                }
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_ai_ok_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let writer = JoblogWriter::open(&path).unwrap();
        // AI success entry
        writer.write(
            &JoblogEntry::new("ai", "item1", "").ai_ok(
                vec![JoblogChange {
                    field: "title".to_string(),
                    old: Some(serde_json::json!("old title")),
                    new: serde_json::json!("New Title"),
                }],
                None,
                100,
            ),
        );
        // AI error entry (should be skipped)
        writer.write(
            &JoblogEntry::new("ai", "item2", "").ai_error("failed", 50),
        );
        // Download entry (should be skipped)
        writer.write(
            &JoblogEntry::new("download", "item3", "file.txt").ok(1000, 100),
        );
        // AI skipped entry (should be skipped)
        writer.write(
            &JoblogEntry::new("ai", "item4", "").skipped(),
        );

        let entries = joblog::read(&path).unwrap();
        let ai_ok: Vec<_> = entries
            .iter()
            .filter(|e| e.op == "ai" && e.status == "ok" && e.changes.is_some())
            .collect();

        assert_eq!(ai_ok.len(), 1);
        assert_eq!(ai_ok[0].item, "item1");
    }

    #[test]
    fn reverse_changes_swap_old_new() {
        let changes = vec![
            JoblogChange {
                field: "title".to_string(),
                old: Some(serde_json::json!("Old Title")),
                new: serde_json::json!("New Title"),
            },
            JoblogChange {
                field: "date".to_string(),
                old: None,
                new: serde_json::json!("2026-01-01"),
            },
        ];

        let reverse: Vec<(String, serde_json::Value)> = changes
            .iter()
            .map(|c| {
                let restore = match &c.old {
                    Some(v) if !v.is_null() => v.clone(),
                    _ => serde_json::Value::String("REMOVE_TAG".to_string()),
                };
                (c.field.clone(), restore)
            })
            .collect();

        assert_eq!(reverse.len(), 2);
        assert_eq!(reverse[0].0, "title");
        assert_eq!(reverse[0].1, serde_json::json!("Old Title"));
        assert_eq!(reverse[1].0, "date");
        assert_eq!(reverse[1].1, serde_json::json!("REMOVE_TAG"));
    }
}
