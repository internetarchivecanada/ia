//! Joblog state for the Log tab.
//!
//! Parses JSONL joblog entries into display-friendly [`LogEntry`] structs
//! and tails the file for live updates.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ia_core::joblog::JoblogEntry;

const TAIL_INTERVAL_SECS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStatus {
    Uploaded,
    Skipped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: String,
    pub item: String,
    pub file: String,
    pub display_status: LogStatus,
    #[allow(dead_code)] // Parsed from joblog, used in error display expansion
    pub error: Option<String>,
}

impl LogEntry {
    pub fn parse(line: &str) -> Option<Self> {
        let entry: JoblogEntry = serde_json::from_str(line).ok()?;

        // Only show upload entries in the upload dashboard log
        if entry.op != "upload" {
            return None;
        }

        let time = entry
            .ts
            .split('T')
            .nth(1)
            .unwrap_or(&entry.ts)
            .trim_end_matches('Z')
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();

        let display_status = match entry.status.as_str() {
            "ok" => LogStatus::Uploaded,
            "skipped" => LogStatus::Skipped,
            _ => LogStatus::Failed,
        };

        Some(Self {
            time,
            item: entry.item,
            file: entry.file,
            display_status,
            error: entry.error,
        })
    }
}

#[derive(Debug)]
pub struct JoblogState {
    pub entries: Vec<LogEntry>,
    path: Option<PathBuf>,
    file_pos: u64,
    last_tail: Instant,
}

impl JoblogState {
    /// Create an empty state with no file backing (for tests or when no joblog).
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            path: None,
            file_pos: 0,
            last_tail: Instant::now(),
        }
    }

    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        let mut file_pos = 0u64;

        if path.exists() {
            let file = File::open(path)?;
            let reader = BufReader::new(&file);
            for line in reader.lines() {
                let line = line?;
                if let Some(entry) = LogEntry::parse(&line) {
                    entries.push(entry);
                }
            }
            file_pos = file.metadata()?.len();
        }

        Ok(Self {
            entries,
            path: Some(path.to_path_buf()),
            file_pos,
            last_tail: Instant::now(),
        })
    }

    /// Check for new log lines appended since last read.
    pub fn tail(&mut self) {
        let Some(path) = &self.path else { return };
        let Ok(mut file) = File::open(path) else {
            return;
        };
        let Ok(metadata) = file.metadata() else {
            return;
        };

        if metadata.len() <= self.file_pos {
            return; // No new data
        }

        if file.seek(SeekFrom::Start(self.file_pos)).is_err() {
            return;
        }

        let reader = BufReader::new(&file);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(entry) = LogEntry::parse(&line) {
                self.entries.push(entry);
            }
        }

        self.file_pos = metadata.len();
        self.last_tail = Instant::now();
    }

    /// Should we check for new lines? (every 2 seconds)
    pub fn needs_tail(&self) -> bool {
        self.last_tail.elapsed().as_secs() >= TAIL_INTERVAL_SECS
    }

    /// Filter entries by status. None means show all.
    #[cfg(test)]
    pub fn filtered_entries(&self, filter: Option<LogStatus>) -> Vec<&LogEntry> {
        match filter {
            None => self.entries.iter().collect(),
            Some(status) => self
                .entries
                .iter()
                .filter(|e| e.display_status == status)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_log_entry_from_jsonl() {
        let line = r#"{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"nasa-photos","file":"img.jpg","status":"ok","bytes":1024}"#;
        let entry = LogEntry::parse(line).unwrap();
        assert_eq!(entry.time, "14:23:01");
        assert_eq!(entry.item, "nasa-photos");
        assert_eq!(entry.file, "img.jpg");
        assert_eq!(entry.display_status, LogStatus::Uploaded);
    }

    #[test]
    fn test_log_entry_skipped() {
        let line = r#"{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"test","file":"f.txt","status":"skipped"}"#;
        let entry = LogEntry::parse(line).unwrap();
        assert_eq!(entry.display_status, LogStatus::Skipped);
    }

    #[test]
    fn test_log_entry_failed() {
        let line = r#"{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"test","file":"f.txt","status":"error","error":"503 Service Unavailable"}"#;
        let entry = LogEntry::parse(line).unwrap();
        assert_eq!(entry.display_status, LogStatus::Failed);
        assert_eq!(entry.error.as_deref(), Some("503 Service Unavailable"));
    }

    #[test]
    fn test_log_entry_invalid_json() {
        assert!(LogEntry::parse("not json").is_none());
    }

    #[test]
    fn test_log_entry_non_upload_filtered() {
        let line = r#"{"ts":"2026-03-18T14:23:01Z","op":"download","item":"test","file":"f.txt","status":"ok"}"#;
        assert!(LogEntry::parse(line).is_none());
    }

    #[test]
    fn test_joblog_state_load_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"test","file":"a.txt","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:02Z","op":"upload","item":"test","file":"b.txt","status":"skipped"}}"#).unwrap();
        f.flush().unwrap();

        let state = JoblogState::open(f.path()).unwrap();
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].file, "a.txt");
        assert_eq!(state.entries[1].file, "b.txt");
    }

    #[test]
    fn test_joblog_state_tail() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"test","file":"a.txt","status":"ok"}}"#).unwrap();
        f.flush().unwrap();

        let mut state = JoblogState::open(f.path()).unwrap();
        assert_eq!(state.entries.len(), 1);

        // Append a new line
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:05Z","op":"upload","item":"test","file":"c.txt","status":"ok"}}"#).unwrap();
        f.flush().unwrap();

        state.tail();
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[1].file, "c.txt");
    }

    #[test]
    fn test_joblog_state_no_file() {
        let state = JoblogState::open(std::path::Path::new("/nonexistent/path"));
        assert!(state.is_ok());
        assert_eq!(state.unwrap().entries.len(), 0);
    }

    #[test]
    fn test_filter_by_status() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"t","file":"a.txt","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:02Z","op":"upload","item":"t","file":"b.txt","status":"skipped"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:03Z","op":"upload","item":"t","file":"c.txt","status":"error","error":"503"}}"#).unwrap();
        f.flush().unwrap();

        let state = JoblogState::open(f.path()).unwrap();
        assert_eq!(state.filtered_entries(Some(LogStatus::Uploaded)).len(), 1);
        assert_eq!(state.filtered_entries(Some(LogStatus::Skipped)).len(), 1);
        assert_eq!(state.filtered_entries(Some(LogStatus::Failed)).len(), 1);
        assert_eq!(state.filtered_entries(None).len(), 3);
    }
}
