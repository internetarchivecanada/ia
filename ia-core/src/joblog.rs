use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// A single joblog entry (one JSON line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoblogEntry {
    /// ISO 8601 timestamp.
    pub ts: String,
    /// Operation type (e.g. "download").
    pub op: String,
    /// Item identifier.
    pub item: String,
    /// File name within the item.
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
}
