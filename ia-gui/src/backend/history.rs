use crate::backend::metadata::format_bytes;
use crate::DownloadHistoryData;
use slint::SharedString;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Load download history from a job log file and group by item.
pub fn load_history(path: &Path) -> Vec<DownloadHistoryData> {
    let entries = match ia_core::joblog::read(path) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    // Group by item
    let mut items: HashMap<String, ItemHistory> = HashMap::new();
    for entry in &entries {
        let item = items.entry(entry.item.clone()).or_insert_with(|| ItemHistory {
            files: 0,
            bytes: 0,
            has_failure: false,
            date: entry.ts.clone(),
        });
        item.files += 1;
        item.bytes += entry.bytes.unwrap_or(0);
        if entry.status == "error" {
            item.has_failure = true;
        }
        // Use latest timestamp
        if entry.ts > item.date {
            item.date = entry.ts.clone();
        }
    }

    let mut result: Vec<DownloadHistoryData> = items
        .into_iter()
        .map(|(identifier, hist)| DownloadHistoryData {
            identifier: SharedString::from(&identifier),
            status: SharedString::from(if hist.has_failure { "failed" } else { "success" }),
            files_count: SharedString::from(format!("{}", hist.files)),
            size: SharedString::from(format_bytes(hist.bytes)),
            date: SharedString::from(format_date(&hist.date)),
        })
        .collect();

    // Sort by date descending
    result.sort_by(|a, b| b.date.as_str().cmp(a.date.as_str()));
    result
}

/// Get the default job log path.
pub fn default_joblog_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ia")
        .join("joblog.jsonl")
}

/// Get identifiers of failed items from the job log.
pub fn failed_identifiers(path: &Path) -> Vec<String> {
    let entries = match ia_core::joblog::read(path) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut failed: HashMap<String, bool> = HashMap::new();
    for entry in &entries {
        if entry.status == "error" {
            failed.insert(entry.item.clone(), true);
        }
    }

    failed.into_keys().collect()
}

struct ItemHistory {
    files: usize,
    bytes: u64,
    has_failure: bool,
    date: String,
}

/// Format an ISO timestamp to a shorter date string.
fn format_date(ts: &str) -> String {
    // Take just YYYY-MM-DD from ISO 8601
    if ts.len() >= 10 {
        ts[..10].to_string()
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_date() {
        assert_eq!(format_date("2026-02-21T18:30:00Z"), "2026-02-21");
        assert_eq!(format_date("2026-02-21"), "2026-02-21");
        assert_eq!(format_date("short"), "short");
    }

    #[test]
    fn test_load_history_missing_file() {
        let result = load_history(Path::new("/nonexistent/path/joblog.jsonl"));
        assert!(result.is_empty());
    }

    #[test]
    fn test_default_joblog_path() {
        let path = default_joblog_path();
        assert!(path.to_str().unwrap().contains("joblog.jsonl"));
    }

    #[test]
    fn test_failed_identifiers_missing_file() {
        let result = failed_identifiers(Path::new("/nonexistent/path/joblog.jsonl"));
        assert!(result.is_empty());
    }
}
