# Dashboard Redesign Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the upload dashboard as an interactive, multi-tab mission control with dense data, vi-like navigation, archive.org visual identity, and S3 task monitoring.

**Architecture:** The existing `UploadDashboard` is replaced by a new multi-tab dashboard that implements the existing `Dashboard` trait from `framework.rs`. Each tab implements a `TabView` trait. Shared state (`S3TaskState`, `JoblogState`) is extracted into dedicated modules. The existing `UploadTuiState` and `run_dashboard_sync` event loop are reused unchanged.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, `open` crate (new dep)

**Spec:** `docs/plans/2026-03-18-dashboard-redesign-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `ia-cli/src/tui/theme.rs` | Create | Color constants, true-color/256 fallback detection |
| `ia-cli/src/tui/tab.rs` | Create | `TabView` trait, `TabId` enum |
| `ia-cli/src/tui/s3_state.rs` | Create | `S3TaskState` — task counts, global count, full task list, polling logic |
| `ia-cli/src/tui/joblog_state.rs` | Create | `JoblogState` — parsed log entries, tail reader |
| `ia-cli/src/tui/search.rs` | Create | `SearchState` — vim-style text input, filter cycling |
| `ia-cli/src/tui/upload_tab.rs` | Create | Upload tab: panels for S3, items, transfers, throughput |
| `ia-cli/src/tui/tasks_tab.rs` | Create | Tasks tab: S3 summary + scrollable task table |
| `ia-cli/src/tui/log_tab.rs` | Create | Log tab: human-readable joblog viewer |
| `ia-cli/src/tui/errors_tab.rs` | Create | Errors tab: upload errors + S3 task errors |
| `ia-cli/src/tui/help.rs` | Create | Help overlay: context-sensitive popup |
| `ia-cli/src/tui/dashboard.rs` | Create | `MultiTabDashboard` — wraps tabs, implements `Dashboard` trait, renders header/footer |
| `ia-cli/src/tui/mod.rs` | Modify | Add new module declarations, update exports |
| `ia-cli/src/tui/upload_app.rs` | Modify | Keep `UploadTuiState` + `update()`, replace `UploadDashboard` with new dashboard, update `run_upload_tui`/`run_upload_batch_tui` to pass joblog path |
| `ia-cli/src/tui/widgets.rs` | Modify | Add shared panel rendering helpers (S3 tasks panel, header bar, tab bar) |
| `ia-cli/src/commands/upload.rs` | Modify | Pass joblog path to dashboard entry points |
| `ia-cli/Cargo.toml` | Modify | Add `open` crate dependency |

---

### Task 1: Color Theme Module

**Files:**
- Create: `ia-cli/src/tui/theme.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for color fallback detection**

Create `ia-cli/src/tui/theme.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_true_color_detection() {
        // When COLORTERM=truecolor, should return true-color values
        let theme = Theme::for_env("truecolor");
        assert_eq!(theme.bg, Color::Rgb(28, 28, 28));
    }

    #[test]
    fn test_256_color_fallback() {
        // When COLORTERM is not truecolor, should return indexed colors
        let theme = Theme::for_env("");
        assert_eq!(theme.bg, Color::Indexed(234));
    }

    #[test]
    fn test_all_accent_colors_present() {
        let theme = Theme::detect();
        // Verify all accent colors are defined
        let _ = theme.maroon;
        let _ = theme.maroon_bright;
        let _ = theme.gold;
        let _ = theme.blue;
        let _ = theme.green;
        let _ = theme.red;
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::theme -- --nocapture`
Expected: Compilation failure — `Theme` not defined yet.

- [ ] **Step 3: Implement Theme struct**

```rust
use ratatui::style::Color;

pub struct Theme {
    // Background & text
    pub bg: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_very_muted: Color,
    pub border: Color,

    // Accent colors (archive.org-inspired)
    pub maroon: Color,        // #8b1a1a — section labels, borders
    pub maroon_bright: Color,  // #cd5c5c — header decorations, key hints
    pub gold: Color,           // #ffd700 — queued, active, rate-limited
    pub blue: Color,           // #5b8dd9 — running
    pub green: Color,          // #3fb950 — completed
    pub red: Color,            // #f85149 — errors
}

impl Theme {
    pub fn detect() -> Self {
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        Self::for_env(&colorterm)
    }

    pub fn for_env(colorterm: &str) -> Self {
        let true_color = matches!(colorterm, "truecolor" | "24bit");
        if true_color {
            Self::true_color()
        } else {
            Self::indexed()
        }
    }

    fn true_color() -> Self {
        Self {
            bg: Color::Rgb(28, 28, 28),
            text: Color::Rgb(212, 212, 212),
            text_secondary: Color::Rgb(153, 153, 153),
            text_muted: Color::Rgb(102, 102, 102),
            text_very_muted: Color::Rgb(68, 68, 68),
            border: Color::Rgb(51, 51, 51),
            maroon: Color::Rgb(139, 26, 26),
            maroon_bright: Color::Rgb(205, 92, 92),
            gold: Color::Rgb(255, 215, 0),
            blue: Color::Rgb(91, 141, 217),
            green: Color::Rgb(63, 185, 80),
            red: Color::Rgb(248, 81, 73),
        }
    }

    fn indexed() -> Self {
        Self {
            bg: Color::Indexed(234),
            text: Color::Indexed(252),
            text_secondary: Color::Indexed(246),
            text_muted: Color::Indexed(242),
            text_very_muted: Color::Indexed(238),
            border: Color::Indexed(236),
            maroon: Color::Indexed(88),
            maroon_bright: Color::Indexed(167),
            gold: Color::Indexed(220),
            blue: Color::Indexed(68),
            green: Color::Indexed(71),
            red: Color::Indexed(196),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::theme -- --nocapture`
Expected: All 3 tests pass.

- [ ] **Step 5: Add module to mod.rs**

In `ia-cli/src/tui/mod.rs`, add:
```rust
pub mod theme;
```

- [ ] **Step 6: Run `just ci`**

Run: `just ci`
Expected: All checks pass (fmt, check, test, doc).

- [ ] **Step 7: Commit**

```bash
git add ia-cli/src/tui/theme.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add color theme module with archive.org palette

Add Theme struct with true-color and 256-color fallback.
Colors: maroon, gold, blue, green, red accents on #1c1c1c dark bg.
Detects COLORTERM env var for capability detection."
```

---

### Task 2: TabView Trait + Tab Framework

**Files:**
- Create: `ia-cli/src/tui/tab.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for TabId**

Create `ia-cli/src/tui/tab.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_id_from_index() {
        assert_eq!(TabId::from_index(0), Some(TabId::Upload));
        assert_eq!(TabId::from_index(1), Some(TabId::Tasks));
        assert_eq!(TabId::from_index(2), Some(TabId::Log));
        assert_eq!(TabId::from_index(3), Some(TabId::Errors));
        assert_eq!(TabId::from_index(4), None);
    }

    #[test]
    fn test_tab_id_label() {
        assert_eq!(TabId::Upload.label(), "❶ Upload");
        assert_eq!(TabId::Tasks.label(), "❷ Tasks");
        assert_eq!(TabId::Log.label(), "❸ Log");
        assert_eq!(TabId::Errors.label(), "❹ Errors");
    }

    #[test]
    fn test_tab_id_index() {
        assert_eq!(TabId::Upload.index(), 0);
        assert_eq!(TabId::Errors.index(), 3);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::tab -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement TabView trait and TabId**

```rust
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::theme::Theme;

/// Trait for individual tab views within a multi-tab dashboard.
pub trait TabView {
    /// Render the tab's content into the given area.
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme);

    /// Handle a key event. Returns true if the key was consumed.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool;

    /// Called on each tick for periodic updates (polling, tail reads, etc.).
    fn tick(&mut self);

    /// Return key hints for the footer, specific to this tab.
    /// Each tuple is (key_label, description).
    fn key_hints(&self) -> Vec<(&str, &str)>;
}

/// Identifies which tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Upload,
    Tasks,
    Log,
    Errors,
}

impl TabId {
    pub const ALL: [TabId; 4] = [TabId::Upload, TabId::Tasks, TabId::Log, TabId::Errors];

    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(TabId::Upload),
            1 => Some(TabId::Tasks),
            2 => Some(TabId::Log),
            3 => Some(TabId::Errors),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            TabId::Upload => 0,
            TabId::Tasks => 1,
            TabId::Log => 2,
            TabId::Errors => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabId::Upload => "❶ Upload",
            TabId::Tasks => "❷ Tasks",
            TabId::Log => "❸ Log",
            TabId::Errors => "❹ Errors",
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::tab -- --nocapture`
Expected: All 3 tests pass.

- [ ] **Step 5: Add module to mod.rs**

```rust
pub mod tab;
```

- [ ] **Step 6: Run `just ci` and commit**

Run: `just ci`

```bash
git add ia-cli/src/tui/tab.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add TabView trait and TabId enum

TabView trait defines draw/handle_key/tick/key_hints for tab views.
TabId enum with Upload/Tasks/Log/Errors variants, index mapping,
and display labels."
```

---

### Task 3: S3TaskState Extraction

**Files:**
- Create: `ia-cli/src/tui/s3_state.rs`
- Modify: `ia-cli/src/tui/mod.rs`

**Context:** Currently S3 task polling is embedded in `upload_app.rs` lines 422-453 inside `run_dashboard_and_summarize`. It writes to `UploadTuiState.tasks_queued/running/error`. We need to extract this into a standalone `S3TaskState` that stores the full task list (for the Tasks tab), tracks global count, supports user/global toggle, and polls every 15s.

- [ ] **Step 1: Write tests for S3TaskState**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let state = S3TaskState::new("test@example.com".to_string());
        assert_eq!(state.queued, 0);
        assert_eq!(state.running, 0);
        assert_eq!(state.errors, 0);
        assert_eq!(state.global_count, 0);
        assert!(!state.is_rate_limited);
        assert!(state.tasks.is_empty());
        assert!(!state.show_global);  // default: show user tasks only
    }

    #[test]
    fn test_toggle_view() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        assert!(!state.show_global);
        state.toggle_view();
        assert!(state.show_global);
        state.toggle_view();
        assert!(!state.show_global);
    }

    #[test]
    fn test_update_from_summary() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        state.update_summary(23, 4, 1);
        assert_eq!(state.queued, 23);
        assert_eq!(state.running, 4);
        assert_eq!(state.errors, 1);
    }

    #[test]
    fn test_update_global_count() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        state.update_global_count(847);
        assert_eq!(state.global_count, 847);
    }

    #[test]
    fn test_update_tasks() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        let tasks = vec![
            S3TaskEntry {
                identifier: "test-item".to_string(),
                cmd: "s3-put".to_string(),
                status: "queued".to_string(),
                submittime: "2026-03-18 14:00:00".to_string(),
            },
        ];
        state.update_tasks(tasks.clone());
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0].identifier, "test-item");
    }

    #[test]
    fn test_set_rate_limited() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        state.set_rate_limited(true);
        assert!(state.is_rate_limited);
        state.set_rate_limited(false);
        assert!(!state.is_rate_limited);
    }

    #[test]
    fn test_needs_poll_respects_interval() {
        let mut state = S3TaskState::new("test@example.com".to_string());
        // Just created, should need poll
        assert!(state.needs_poll());
        // After marking polled, should not need poll
        state.mark_polled();
        assert!(!state.needs_poll());
    }

    #[test]
    fn test_seconds_since_poll() {
        let state = S3TaskState::new("test@example.com".to_string());
        // Just created, should be 0
        assert!(state.seconds_since_poll() < 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::s3_state -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement S3TaskState**

```rust
use std::time::{Duration, Instant};

/// Poll interval for S3 task queries.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// A simplified task entry for display in the Tasks tab.
#[derive(Debug, Clone)]
pub struct S3TaskEntry {
    pub identifier: String,
    pub cmd: String,
    pub status: String,
    pub submittime: String,
}

/// Shared S3 task state used across all tabs.
#[derive(Debug)]
pub struct S3TaskState {
    /// User's email for filtering.
    pub email: String,
    /// Summary counts from the user's tasks.
    pub queued: u32,
    pub running: u32,
    pub errors: u32,
    /// Total task count across all users.
    pub global_count: u32,
    /// Whether any item is currently rate-limited.
    pub is_rate_limited: bool,
    /// Full task list for the Tasks tab.
    pub tasks: Vec<S3TaskEntry>,
    /// Whether to show global tasks (true) or user tasks (false).
    pub show_global: bool,
    /// When we last polled the API.
    last_polled: Instant,
}

impl S3TaskState {
    pub fn new(email: String) -> Self {
        Self {
            email,
            queued: 0,
            running: 0,
            errors: 0,
            global_count: 0,
            is_rate_limited: false,
            tasks: Vec::new(),
            show_global: false,
            last_polled: Instant::now() - POLL_INTERVAL, // trigger immediate first poll
        }
    }

    pub fn update_summary(&mut self, queued: u32, running: u32, errors: u32) {
        self.queued = queued;
        self.running = running;
        self.errors = errors;
    }

    pub fn update_global_count(&mut self, count: u32) {
        self.global_count = count;
    }

    pub fn update_tasks(&mut self, tasks: Vec<S3TaskEntry>) {
        self.tasks = tasks;
    }

    pub fn toggle_view(&mut self) {
        self.show_global = !self.show_global;
    }

    pub fn set_rate_limited(&mut self, limited: bool) {
        self.is_rate_limited = limited;
    }

    pub fn needs_poll(&self) -> bool {
        self.last_polled.elapsed() >= POLL_INTERVAL
    }

    pub fn mark_polled(&mut self) {
        self.last_polled = Instant::now();
    }

    pub fn seconds_since_poll(&self) -> u64 {
        self.last_polled.elapsed().as_secs()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::s3_state -- --nocapture`
Expected: All 7 tests pass.

- [ ] **Step 5: Add module to mod.rs**

```rust
pub mod s3_state;
```

- [ ] **Step 6: Run `just ci` and commit**

```bash
git add ia-cli/src/tui/s3_state.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): extract S3TaskState for shared task monitoring

Standalone S3TaskState with summary counts, global count, full task
list, user/global toggle, 15s poll interval, rate-limit tracking.
Previously embedded in UploadTuiState."
```

---

### Task 4: JoblogState — Log Entry Parsing and Tail Reader

**Files:**
- Create: `ia-cli/src/tui/joblog_state.rs`
- Modify: `ia-cli/src/tui/mod.rs`

**Context:** `ia-core/src/joblog.rs` has `JoblogEntry` (lines 12-42) with fields: `ts`, `op`, `item`, `file`, `status`, `bytes`, `elapsed_ms`, `error`. It's serialized as JSONL. We need a `JoblogState` that reads the file, parses entries into a display-friendly `LogEntry`, and tails for new entries.

- [ ] **Step 1: Write tests for LogEntry parsing and JoblogState**

```rust
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
    fn test_joblog_state_load_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:01Z","op":"upload","item":"test","file":"a.txt","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:23:02Z","op":"upload","item":"test","file":"b.txt","status":"skipped"}}"#).unwrap();
        f.flush().unwrap();

        let mut state = JoblogState::open(f.path()).unwrap();
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
        assert!(state.is_ok());  // Should gracefully handle missing file
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::joblog_state -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement JoblogState**

```rust
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
    pub time: String,       // HH:MM:SS extracted from ISO timestamp
    pub item: String,
    pub file: String,
    pub display_status: LogStatus,
    pub error: Option<String>,
}

impl LogEntry {
    pub fn parse(line: &str) -> Option<Self> {
        let entry: JoblogEntry = serde_json::from_str(line).ok()?;

        // Only show upload entries
        if entry.op != "upload" {
            return None;
        }

        let time = entry.ts
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
        let Ok(mut file) = File::open(path) else { return };
        let Ok(metadata) = file.metadata() else { return };

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
    pub fn filtered_entries(&self, filter: Option<LogStatus>) -> Vec<&LogEntry> {
        match filter {
            None => self.entries.iter().collect(),
            Some(status) => self.entries.iter().filter(|e| e.display_status == status).collect(),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::joblog_state -- --nocapture`
Expected: All 8 tests pass.

- [ ] **Step 5: Add `tempfile` dev-dependency if not present, add module to mod.rs**

Check `ia-cli/Cargo.toml` for `tempfile` in `[dev-dependencies]`. If missing, add it. Add `pub mod joblog_state;` to `mod.rs`.

- [ ] **Step 6: Run `just ci` and commit**

```bash
git add ia-cli/src/tui/joblog_state.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add JoblogState for log tab

Parses JSONL joblog into LogEntry structs with time/item/file/status.
Tail-reads new entries every 2s via file seek.
Supports filtering by status (uploaded/skipped/failed)."
```

---

### Task 5: Search and Filter Infrastructure

**Files:**
- Create: `ia-cli/src/tui/search.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for SearchState**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_inactive_by_default() {
        let search = SearchState::new();
        assert!(!search.is_active());
        assert!(search.query().is_empty());
    }

    #[test]
    fn test_activate_and_type() {
        let mut search = SearchState::new();
        search.activate();
        assert!(search.is_active());
        search.push('h');
        search.push('e');
        search.push('l');
        assert_eq!(search.query(), "hel");
    }

    #[test]
    fn test_backspace() {
        let mut search = SearchState::new();
        search.activate();
        search.push('a');
        search.push('b');
        search.backspace();
        assert_eq!(search.query(), "a");
    }

    #[test]
    fn test_confirm() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.confirm();
        assert!(!search.is_active());
        assert_eq!(search.query(), "x");  // filter stays active
    }

    #[test]
    fn test_cancel_clears() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.cancel();
        assert!(!search.is_active());
        assert!(search.query().is_empty());  // filter cleared
    }

    #[test]
    fn test_clear_filter() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.confirm();
        assert_eq!(search.query(), "x");
        search.clear();
        assert!(search.query().is_empty());
    }

    #[test]
    fn test_matches() {
        let search = SearchState::with_query("nasa");
        assert!(search.matches("nasa-photos-2024"));
        assert!(search.matches("NASA-data"));  // case-insensitive
        assert!(!search.matches("hubble-deep"));
    }

    #[test]
    fn test_filter_cycle() {
        let mut filter = FilterCycle::new(&["All", "Uploaded", "Skipped", "Failed"]);
        assert_eq!(filter.current(), "All");
        filter.next();
        assert_eq!(filter.current(), "Uploaded");
        filter.next();
        assert_eq!(filter.current(), "Skipped");
        filter.next();
        assert_eq!(filter.current(), "Failed");
        filter.next();
        assert_eq!(filter.current(), "All");  // wraps around
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::search -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement SearchState and FilterCycle**

```rust
/// Vim-style search input state.
pub struct SearchState {
    query: String,
    active: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            active: false,
        }
    }

    pub fn with_query(q: &str) -> Self {
        Self {
            query: q.to_string(),
            active: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn backspace(&mut self) {
        self.query.pop();
    }

    pub fn confirm(&mut self) {
        self.active = false;
        // query stays as active filter
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
    }

    pub fn clear(&mut self) {
        self.query.clear();
    }

    pub fn matches(&self, text: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        text.to_lowercase().contains(&self.query.to_lowercase())
    }
}

/// Cycles through a fixed set of filter labels.
pub struct FilterCycle {
    labels: Vec<&'static str>,
    index: usize,
}

impl FilterCycle {
    pub fn new(labels: &[&'static str]) -> Self {
        Self {
            labels: labels.to_vec(),
            index: 0,
        }
    }

    pub fn current(&self) -> &str {
        self.labels[self.index]
    }

    pub fn next(&mut self) {
        self.index = (self.index + 1) % self.labels.len();
    }

    pub fn index(&self) -> usize {
        self.index
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::search -- --nocapture`
Expected: All 8 tests pass.

- [ ] **Step 5: Add module to mod.rs, run `just ci`, commit**

```bash
git add ia-cli/src/tui/search.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add search and filter infrastructure

SearchState: vim-style text input with activate/confirm/cancel,
case-insensitive matching. FilterCycle: cycles through status
filter options (All/Uploaded/Skipped/Failed)."
```

---

### Task 6: Shared UI Widgets — Header, Tab Bar, Footer, S3 Panel

**Files:**
- Modify: `ia-cli/src/tui/widgets.rs`

**Context:** `widgets.rs` (lines 26-265) has `ThroughputTracker`, format helpers, and panel widgets. We need to add shared rendering functions for the decorative header, tab bar, S3 tasks panel, and key hints footer that all tabs share.

- [ ] **Step 1: Write tests for new widget helpers**

Add to `ia-cli/src/tui/widgets.rs` tests module:

```rust
#[test]
fn test_build_header_line() {
    let line = build_header_line("ia upload", "3/10 items", "4.2 GB", "12.5 MB/s", "ETA 28m");
    assert!(line.contains("ia upload"));
    assert!(line.contains("3/10 items"));
}

#[test]
fn test_build_s3_status_text_normal() {
    let text = build_s3_status_text(23, 4, 0, false);
    assert!(text.contains("23"));
    assert!(text.contains("4"));
    assert!(text.contains("Errors: 0"));
    assert!(!text.contains("rate-limited"));
}

#[test]
fn test_build_s3_status_text_rate_limited_no_errors() {
    let text = build_s3_status_text(23, 4, 0, true);
    assert!(text.contains("rate-limited"));
    assert!(!text.contains("Errors"));
}

#[test]
fn test_build_s3_status_text_rate_limited_with_errors() {
    let text = build_s3_status_text(23, 4, 2, true);
    assert!(text.contains("rate-limited"));
    assert!(text.contains("Errors: 2"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::widgets -- --nocapture`
Expected: Fails — functions not defined.

- [ ] **Step 3: Implement shared rendering helpers**

Add to `ia-cli/src/tui/widgets.rs`:

```rust
use super::tab::TabId;
use super::theme::Theme;

/// Build the decorative header line content.
pub fn build_header_line(
    command: &str,
    items: &str,
    bytes: &str,
    speed: &str,
    eta: &str,
) -> String {
    format!("━━━ {} ━━━ {} ━━━ {} ━━━ {} ━━━ {} ━━━", command, items, bytes, speed, eta)
}

/// Build the S3 status text according to the spec:
/// - Normal: "⧖ Queued: N   ↻ Running: N   ✗ Errors: N"
/// - Rate-limited, 0 errors: "⧖ Queued: N   ↻ Running: N   ⏸ rate-limited"
/// - Rate-limited with errors: "⧖ Queued: N   ↻ Running: N   ✗ Errors: N   ⏸ rate-limited"
pub fn build_s3_status_text(queued: u32, running: u32, errors: u32, rate_limited: bool) -> String {
    let mut parts = vec![
        format!("⧖ Queued: {}", queued),
        format!("↻ Running: {}", running),
    ];
    if rate_limited && errors == 0 {
        parts.push("⏸ rate-limited".to_string());
    } else {
        parts.push(format!("✗ Errors: {}", errors));
        if rate_limited {
            parts.push("⏸ rate-limited".to_string());
        }
    }
    parts.join("   ")
}

/// Render the decorative header bar (centered ━━━ line with stats).
pub fn draw_header(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    theme: &Theme,
    command: &str,
    items_done: usize,
    items_total: usize,
    bytes: &str,
    speed: &str,
    eta: &str,
) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use ratatui::style::Style;
    use ratatui::layout::Alignment;

    let items_str = format!("{}/{} items", items_done, items_total);
    let spans = vec![
        Span::styled("━━━ ", Style::default().fg(theme.maroon_bright)),
        Span::styled(command, Style::default().fg(theme.text).add_modifier(ratatui::style::Modifier::BOLD)),
        Span::styled(" ━━━ ", Style::default().fg(theme.maroon_bright)),
        Span::styled(&items_str, Style::default().fg(theme.green)),
        Span::styled(" ━━━ ", Style::default().fg(theme.maroon_bright)),
        Span::styled(bytes, Style::default().fg(theme.text_secondary)),
        Span::styled(" ━━━ ", Style::default().fg(theme.maroon_bright)),
        Span::styled(speed, Style::default().fg(theme.maroon_bright)),
        Span::styled(" ━━━ ", Style::default().fg(theme.maroon_bright)),
        Span::styled(eta, Style::default().fg(theme.text_secondary)),
        Span::styled(" ━━━", Style::default().fg(theme.maroon_bright)),
    ];
    let header = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
    frame.render_widget(header, area);
}

/// Render the tab bar with active tab highlighted.
pub fn draw_tab_bar(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    theme: &Theme,
    active: TabId,
) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use ratatui::style::{Style, Modifier};
    use ratatui::layout::Alignment;

    let mut spans = Vec::new();
    for (i, tab) in TabId::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        if *tab == active {
            spans.push(Span::styled(
                format!(" {} ", tab.label()),
                Style::default()
                    .fg(theme.text)
                    .bg(theme.maroon)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                tab.label(),
                Style::default().fg(theme.text_muted),
            ));
        }
    }
    let bar = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
    frame.render_widget(bar, area);
}

/// Render the S3 Tasks panel (used on Upload tab and Tasks tab).
pub fn draw_s3_panel(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    theme: &Theme,
    queued: u32,
    running: u32,
    errors: u32,
    global_count: u32,
    rate_limited: bool,
    seconds_ago: u64,
) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};
    use ratatui::style::Style;

    let title_line = Line::from(vec![
        Span::styled(" S3 Tasks ", Style::default().fg(theme.maroon_bright)),
    ]);
    let global_line = Line::from(vec![
        Span::styled(format!("Global: {} ", global_count), Style::default().fg(theme.text_muted)),
    ]);
    let bottom_line = Line::from(vec![
        Span::styled(format!(" polled {}s ago ", seconds_ago), Style::default().fg(theme.text_very_muted)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title_top(title_line.left_aligned())
        .title_top(global_line.right_aligned())
        .title_bottom(bottom_line.left_aligned());

    let mut spans = vec![
        Span::styled("⧖ Queued: ", Style::default().fg(theme.text_secondary)),
        Span::styled(format!("{}", queued), Style::default().fg(theme.gold).add_modifier(ratatui::style::Modifier::BOLD)),
        Span::raw("   "),
        Span::styled("↻ Running: ", Style::default().fg(theme.text_secondary)),
        Span::styled(format!("{}", running), Style::default().fg(theme.blue).add_modifier(ratatui::style::Modifier::BOLD)),
        Span::raw("   "),
    ];

    if rate_limited && errors == 0 {
        spans.push(Span::styled("⏸ rate-limited", Style::default().fg(theme.gold)));
    } else {
        spans.push(Span::styled("✗ Errors: ", Style::default().fg(theme.text_secondary)));
        spans.push(Span::styled(
            format!("{}", errors),
            Style::default().fg(if errors > 0 { theme.red } else { theme.text_secondary }),
        ));
        if rate_limited {
            spans.push(Span::raw("   "));
            spans.push(Span::styled("⏸ rate-limited", Style::default().fg(theme.gold)));
        }
    }

    let content = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(content, area);
}

/// Render context-sensitive key hints in the footer.
pub fn draw_footer(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    theme: &Theme,
    hints: &[(&str, &str)],
    elapsed: &str,
) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use ratatui::style::Style;
    use ratatui::layout::Alignment;

    let mut spans: Vec<Span> = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(format!("[{}]", key), Style::default().fg(theme.maroon_bright)));
        spans.push(Span::styled(format!(" {}", desc), Style::default().fg(theme.text_muted)));
    }

    // We'll render key hints left-aligned and elapsed right-aligned
    // by using two paragraphs in a horizontal split
    let keys = Paragraph::new(Line::from(spans));
    let time = Paragraph::new(Line::from(vec![
        Span::styled(elapsed, Style::default().fg(theme.text_muted)),
    ])).alignment(Alignment::Right);

    // Split area horizontally
    let chunks = ratatui::layout::Layout::horizontal([
        ratatui::layout::Constraint::Percentage(80),
        ratatui::layout::Constraint::Percentage(20),
    ]).split(area);

    frame.render_widget(keys, chunks[0]);
    frame.render_widget(time, chunks[1]);
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::widgets -- --nocapture`
Expected: All tests pass (existing + 4 new).

- [ ] **Step 5: Run `just ci` and commit**

```bash
git add ia-cli/src/tui/widgets.rs
git commit -m "feat(tui): add shared dashboard widgets

Header bar (centered ━━━ decorative line), tab bar with maroon
active pill, S3 tasks panel with rate-limit states, footer with
context-sensitive key hints. All themed with archive.org palette."
```

---

### Task 7: Upload Tab

**Files:**
- Create: `ia-cli/src/tui/upload_tab.rs`
- Modify: `ia-cli/src/tui/mod.rs`

**Context:** This migrates the existing `upload_ui.rs` panel rendering into a `TabView` implementation, adapting it to the new visual style (archive.org colors, new layout). The `UploadTuiState` struct and `update()` method in `upload_app.rs` remain unchanged.

- [ ] **Step 1: Write tests for UploadTab**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_state() -> Arc<Mutex<UploadTuiState>> {
        Arc::new(Mutex::new(UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
        ])))
    }

    fn make_s3_state() -> Arc<Mutex<S3TaskState>> {
        Arc::new(Mutex::new(S3TaskState::new("test@example.com".to_string())))
    }

    #[test]
    fn test_initial_focus() {
        let tab = UploadTab::new(make_state(), make_s3_state());
        assert_eq!(tab.focused_panel, FocusPanel::Items);
    }

    #[test]
    fn test_tab_cycles_focus() {
        let mut tab = UploadTab::new(make_state(), make_s3_state());
        assert_eq!(tab.focused_panel, FocusPanel::Items);
        tab.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(tab.focused_panel, FocusPanel::Transfers);
        tab.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(tab.focused_panel, FocusPanel::Items);
    }

    #[test]
    fn test_j_k_scrolls() {
        let mut tab = UploadTab::new(make_state(), make_s3_state());
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 1);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 0);
        // k at 0 stays at 0
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 0);
    }

    #[test]
    fn test_key_hints() {
        let tab = UploadTab::new(make_state(), make_s3_state());
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "j/k"));
        assert!(hints.iter().any(|(k, _)| *k == "Tab"));
        assert!(hints.iter().any(|(k, _)| *k == "Enter"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::upload_tab -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement UploadTab struct and TabView impl**

Create `ia-cli/src/tui/upload_tab.rs`. This adapts the rendering from `upload_ui.rs` to the new theme and layout. The implementation should:

1. Hold `Arc<Mutex<UploadTuiState>>` and `Arc<Mutex<S3TaskState>>` references
2. Track `focused_panel: FocusPanel` (Items or Transfers), `items_scroll: usize`, `transfers_scroll: usize`
3. `draw()`: Render S3 panel (via `widgets::draw_s3_panel`), then horizontal split for Items + Transfers, then Throughput sparkline. Use `Theme` colors for all styling.
4. `handle_key()`: `Tab` cycles focus, `j/k` scrolls focused panel, `Enter` opens browser for highlighted item
5. `tick()`: No-op (upload state is updated externally via `UploadTuiState::update`)
6. `key_hints()`: Return `[("j/k", "scroll"), ("Tab", "panel"), ("Enter", "history"), ("?", "help"), ("q", "quit")]`

Panel rendering functions (items list, transfers with progress bars, completed files, throughput sparkline) are adapted from `upload_ui.rs` but restyled:
- Use `theme.maroon_bright` for panel titles instead of cyan
- Use `theme.gold` for active item highlight with left-border
- Use `theme.border` for panel borders
- Progress bars: completed portion in `theme.green`, remaining in `theme.border`
- Completed files shown at bottom of Transfers panel (below a `──` divider), up to 10 entries
- Status icons unchanged: `✓` (green), `▸` (gold), `·` (muted), `⏸` (gold for rate-limited), `✗` (red)

Note: The full rendering code is substantial (~200-300 lines). Adapt directly from `upload_ui.rs` (lines 18-479), changing colors and layout structure. Key changes from existing code:
- Remove the old Header/Gauge (replaced by shared dashboard header)
- Remove the old status bar (replaced by shared dashboard footer)
- Remove S3 Tasks panel (now rendered by shared `draw_s3_panel`)
- Remove Rate Limit panel (rate-limited items shown inline in Transfers)
- Items and Transfers side-by-side (was vertical stack)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::upload_tab -- --nocapture`
Expected: All 4 tests pass.

- [ ] **Step 5: Add `open` crate to Cargo.toml**

In `ia-cli/Cargo.toml`, add under `[dependencies]`:
```toml
open = { version = "5", optional = true }
```

And update the `tui` feature to include it:
```toml
tui = ["dep:ratatui", "dep:crossterm", "dep:open"]
```

- [ ] **Step 6: Add module to mod.rs, run `just ci`, commit**

```bash
git add ia-cli/src/tui/upload_tab.rs ia-cli/src/tui/mod.rs ia-cli/Cargo.toml
git commit -m "feat(tui): add Upload tab with archive.org themed panels

Migrates upload_ui.rs rendering to TabView impl. Side-by-side
items/transfers layout, S3 panel via shared widget, gold active
item highlight, rate-limited inline in transfers. Enter opens
item history in browser via open crate."
```

---

### Task 8: Tasks Tab

**Files:**
- Create: `ia-cli/src/tui/tasks_tab.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for TasksTab**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_s3_state() -> Arc<Mutex<S3TaskState>> {
        let mut state = S3TaskState::new("test@example.com".to_string());
        state.update_tasks(vec![
            S3TaskEntry { identifier: "item-a".into(), cmd: "s3-put".into(), status: "running".into(), submittime: "14:01:23".into() },
            S3TaskEntry { identifier: "item-b".into(), cmd: "s3-put".into(), status: "queued".into(), submittime: "14:01:25".into() },
            S3TaskEntry { identifier: "item-c".into(), cmd: "s3-put".into(), status: "queued".into(), submittime: "14:01:30".into() },
        ]);
        state.update_summary(2, 1, 0);
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn test_scroll_clamps() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 2);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 2);  // clamped at len-1
    }

    #[test]
    fn test_toggle_view() {
        let s3 = make_s3_state();
        let mut tab = TasksTab::new(s3.clone());
        assert!(!s3.lock().unwrap().show_global);
        tab.handle_key(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(s3.lock().unwrap().show_global);
    }

    #[test]
    fn test_search_activation() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
        // Type a character
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(tab.search.query(), "a");
        // Escape cancels
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!tab.search.is_active());
        assert!(tab.search.query().is_empty());
    }

    #[test]
    fn test_key_hints() {
        let tab = TasksTab::new(make_s3_state());
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "u"));
        assert!(hints.iter().any(|(k, _)| *k == "Enter"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::tasks_tab -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement TasksTab**

Create `ia-cli/src/tui/tasks_tab.rs`:

1. Hold `Arc<Mutex<S3TaskState>>` and a `SearchState`
2. Track `cursor: usize` (highlighted row), `scroll_offset: usize`
3. `draw()`: Render S3 summary panel (with user/global indicator), then task table with columns SUBMITTED, IDENTIFIER, CMD, STATUS. Highlighted row gets gold left-border. Search input at bottom when active.
4. `handle_key()`: `j/k` scroll cursor, `u` toggles user/global, `/` activates search, `Enter` opens browser. When search is active, route chars to search input.
5. `tick()`: No-op (polling happens at dashboard level)
6. `key_hints()`: `[("j/k", "scroll"), ("/", "search"), ("u", "user/global"), ("Enter", "history"), ("?", "help"), ("q", "quit")]`

Status colors in the table: `running` in blue, `queued` in gold, `error` in red.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::tasks_tab -- --nocapture`
Expected: All 4 tests pass.

- [ ] **Step 5: Add module to mod.rs, run `just ci`, commit**

```bash
git add ia-cli/src/tui/tasks_tab.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add Tasks tab with scrollable task table

Browsable S3 task list with user/global toggle (u key), search (/),
cursor navigation (j/k), and Enter to open archive.org/history/<id>.
S3 summary panel at top with toggle indicator."
```

---

### Task 9: Log Tab

**Files:**
- Create: `ia-cli/src/tui/log_tab.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for LogTab**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_joblog_state() -> (Arc<Mutex<JoblogState>>, tempfile::TempPath) {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..20 {
            writeln!(f, r#"{{"ts":"2026-03-18T14:{:02}:00Z","op":"upload","item":"item-{}","file":"file-{}.txt","status":"ok"}}"#, i, i, i).unwrap();
        }
        f.flush().unwrap();
        let state = JoblogState::open(f.path()).unwrap();
        let path = f.into_temp_path();  // keeps file alive without leaking
        (Arc::new(Mutex::new(state)), path)
    }

    #[test]
    fn test_scroll_j_k() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        assert_eq!(tab.cursor, 0);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
    }

    #[test]
    fn test_jump_to_end() {
        let (state, _path) = make_joblog_state();
        let entry_count = state.lock().unwrap().entries.len();
        let mut tab = LogTab::new(state);
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, entry_count - 1);
    }

    #[test]
    fn test_gg_jump_to_top() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        // Move down first
        for _ in 0..5 {
            tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(tab.cursor, 5);
        // First g
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(tab.pending_g);
        // Second g
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
        assert!(!tab.pending_g);
    }

    #[test]
    fn test_filter_cycle() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        assert!(tab.status_filter.is_none());
        tab.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(tab.filter_cycle.current(), "Uploaded");
        tab.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(tab.filter_cycle.current(), "Skipped");
    }

    #[test]
    fn test_search_activation() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::log_tab -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement LogTab**

Create `ia-cli/src/tui/log_tab.rs`:

1. Hold `Arc<Mutex<JoblogState>>`, `SearchState`, `FilterCycle`, `pending_g: bool`, `pending_g_at: Instant`
2. Track `cursor: usize`, `scroll_offset: usize`, `status_filter: Option<LogStatus>`
3. `draw()`: Single panel with table columns TIME, ITEM, FILE, STATUS. Color-coded status. Filter indicator in panel title when active. Search input at bottom when active. Position indicator in bottom border.
4. `handle_key()`:
   - `j/k`: scroll cursor
   - `G` (shift+g): jump to end
   - `g`: set `pending_g = true`, second `g` within 500ms jumps to top
   - Any other key: clear `pending_g`
   - `/`: activate search
   - `f`: cycle filter (All → Uploaded → Skipped → Failed → All)
   - When search active: route to search input
5. `tick()`: Call `joblog_state.needs_tail()` → `joblog_state.tail()` if needed. Also clear `pending_g` if timeout exceeded:
   ```rust
   if self.pending_g && self.pending_g_at.elapsed() > Duration::from_millis(500) {
       self.pending_g = false;
   }
   ```
6. `key_hints()`: `[("j/k", "scroll"), ("G", "end"), ("gg", "top"), ("/", "search"), ("f", "filter"), ("?", "help"), ("q", "quit")]`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::log_tab -- --nocapture`
Expected: All 5 tests pass.

- [ ] **Step 5: Add module to mod.rs, run `just ci`, commit**

```bash
git add ia-cli/src/tui/log_tab.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add Log tab with human-readable joblog viewer

Scrollable table of joblog entries with color-coded status.
Vi-style navigation: j/k scroll, G end, gg top.
Search (/) and status filter cycling (f).
Tails joblog file for live updates every 2s."
```

---

### Task 10: Errors Tab

**Files:**
- Create: `ia-cli/src/tui/errors_tab.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for ErrorsTab**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_state_with_errors() -> Arc<Mutex<UploadTuiState>> {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);
        state.failed_files.push(("item-a/file1.jpg".to_string(), "503 Service Unavailable".to_string()));
        state.failed_files.push(("item-a/file2.jpg".to_string(), "timeout after 30s".to_string()));
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn test_scroll() {
        let mut tab = ErrorsTab::new(make_state_with_errors(), Arc::new(Mutex::new(S3TaskState::new("t@t.com".into()))));
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
    }

    #[test]
    fn test_expand_collapse() {
        let mut tab = ErrorsTab::new(make_state_with_errors(), Arc::new(Mutex::new(S3TaskState::new("t@t.com".into()))));
        assert!(tab.expanded.is_none());
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(tab.expanded, Some(0));
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(tab.expanded.is_none());  // toggle off
    }

    #[test]
    fn test_escape_collapses() {
        let mut tab = ErrorsTab::new(make_state_with_errors(), Arc::new(Mutex::new(S3TaskState::new("t@t.com".into()))));
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(tab.expanded.is_some());
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(tab.expanded.is_none());
    }
}
```

- [ ] **Step 2: Run tests, implement, run tests, commit**

Follow same TDD pattern. The ErrorsTab:

1. Holds `Arc<Mutex<UploadTuiState>>` (for `failed_files`) and `Arc<Mutex<S3TaskState>>` (for task errors)
2. Tracks `cursor: usize`, `expanded: Option<usize>`
3. `draw()`: Upload Errors panel with table (ITEM, FILE, ERROR). Note: `UploadTuiState.failed_files` is `Vec<(String, String)>` (key, error) with no timestamp, so omit the TIME column (or add timestamps to `failed_files` in a preparatory step by changing it to `Vec<(Instant, String, String)>`). If `expanded == Some(i)`, row `i` expands to show full error detail. S3 Task Errors panel only rendered if `s3_state.errors > 0`.
4. `handle_key()`: `j/k` scroll, `Enter` toggle expand, `Esc` collapse
5. `key_hints()`: `[("j/k", "scroll"), ("Enter", "expand"), ("?", "help"), ("q", "quit")]`

```bash
git add ia-cli/src/tui/errors_tab.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add Errors tab with inline error expansion

Upload errors table with Enter to expand full detail inline.
S3 task errors panel hidden unless errors exist. Esc to collapse."
```

---

### Task 11: Help Overlay

**Files:**
- Create: `ia-cli/src/tui/help.rs`
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Write tests for HelpOverlay**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::tab::TabId;

    #[test]
    fn test_global_hints_always_present() {
        let hints = help_content(TabId::Upload);
        assert!(hints.contains("1-4"));
        assert!(hints.contains("Tab"));
        assert!(hints.contains("q"));
    }

    #[test]
    fn test_upload_tab_hints() {
        let hints = help_content(TabId::Upload);
        assert!(hints.contains("j/k"));
        assert!(hints.contains("Enter"));
    }

    #[test]
    fn test_tasks_tab_hints() {
        let hints = help_content(TabId::Tasks);
        assert!(hints.contains("u"));
        assert!(hints.contains("user/global"));
    }

    #[test]
    fn test_log_tab_hints() {
        let hints = help_content(TabId::Log);
        assert!(hints.contains("gg"));
        assert!(hints.contains("f"));
    }
}
```

- [ ] **Step 2: Implement and test**

The `help.rs` module provides:
- `help_content(tab: TabId) -> String` — returns the formatted help text for a given tab
- `draw_help_overlay(frame, area, theme, tab)` — renders the help popup centered on screen using `Clear` + `Block`

```bash
git add ia-cli/src/tui/help.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add context-sensitive help overlay

Popup on ? key showing keybindings for current tab.
Global keys always shown. Dismissed on any key."
```

---

### Task 12: MultiTabDashboard — Wire Everything Together

**Files:**
- Create: `ia-cli/src/tui/dashboard.rs`
- Modify: `ia-cli/src/tui/mod.rs`

**Context:** This is the central coordinator. It implements the existing `Dashboard` trait from `framework.rs`, holds all four tabs, routes input, renders the shared header/tab bar/footer, and manages the help overlay.

- [ ] **Step 1: Write tests for MultiTabDashboard**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_dashboard() -> MultiTabDashboard {
        let upload_state = Arc::new(Mutex::new(UploadTuiState::new(&["test".to_string()])));
        let s3_state = Arc::new(Mutex::new(S3TaskState::new("t@t.com".into())));
        let joblog_state = Arc::new(Mutex::new(JoblogState::empty()));
        MultiTabDashboard::new(upload_state, s3_state, joblog_state)
    }

    #[test]
    fn test_initial_tab() {
        let d = make_dashboard();
        assert_eq!(d.active_tab, TabId::Upload);
    }

    #[test]
    fn test_tab_switching() {
        let mut d = make_dashboard();
        d.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Tasks);
        d.handle_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Log);
        d.handle_key(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Errors);
        d.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Upload);
    }

    #[test]
    fn test_quit() {
        let mut d = make_dashboard();
        assert!(!d.quit_requested());
        d.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(d.quit_requested());
    }

    #[test]
    fn test_help_toggle() {
        let mut d = make_dashboard();
        assert!(!d.show_help);
        d.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(d.show_help);
        // Any key dismisses
        d.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!d.show_help);
    }

    #[test]
    fn test_keys_route_to_active_tab() {
        let mut d = make_dashboard();
        // j on Upload tab should be consumed by upload tab
        let consumed = d.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(consumed);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib tui::dashboard -- --nocapture`
Expected: Compilation failure.

- [ ] **Step 3: Implement MultiTabDashboard**

Create `ia-cli/src/tui/dashboard.rs`:

```rust
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use super::framework::Dashboard;
use super::tab::TabId;
use super::theme::Theme;
use super::upload_tab::UploadTab;
use super::tasks_tab::TasksTab;
use super::log_tab::LogTab;
use super::errors_tab::ErrorsTab;
use super::upload_app::UploadTuiState;
use super::s3_state::S3TaskState;
use super::joblog_state::JoblogState;
use super::widgets;
use super::help;

pub struct MultiTabDashboard {
    pub active_tab: TabId,
    pub show_help: bool,
    quit: bool,
    theme: Theme,

    // Shared state
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,

    // Tabs
    upload_tab: UploadTab,
    tasks_tab: TasksTab,
    log_tab: LogTab,
    errors_tab: ErrorsTab,
}

impl MultiTabDashboard {
    pub fn new(
        upload_state: Arc<Mutex<UploadTuiState>>,
        s3_state: Arc<Mutex<S3TaskState>>,
        joblog_state: Arc<Mutex<JoblogState>>,
    ) -> Self {
        let theme = Theme::detect();
        Self {
            active_tab: TabId::Upload,
            show_help: false,
            quit: false,
            theme,
            upload_state: upload_state.clone(),
            s3_state: s3_state.clone(),
            upload_tab: UploadTab::new(upload_state.clone(), s3_state.clone()),
            tasks_tab: TasksTab::new(s3_state.clone()),
            log_tab: LogTab::new(joblog_state),
            errors_tab: ErrorsTab::new(upload_state, s3_state),
        }
    }
}

impl Dashboard for MultiTabDashboard {
    fn draw(&self, frame: &mut Frame) {
        // Tick all tabs (not just active) so background work happens
        // Note: draw() takes &self, so tick() calls happen via interior
        // mutability on the Arc<Mutex<>> state. Alternatively, add a
        // tick_all() method called from run_dashboard_sync before draw().
        // The cleanest approach: call tick() in a separate method invoked
        // from the event loop. See Task 13 integration for the actual wiring.

        let area = frame.area();

        // Layout: header(1) + tab_bar(1) + blank(1) + content(fill) + footer(1)
        let chunks = Layout::vertical([
            Constraint::Length(1),  // header
            Constraint::Length(1),  // tab bar
            Constraint::Length(1),  // spacer
            Constraint::Min(0),    // content
            Constraint::Length(1),  // footer
        ]).split(area);

        // Propagate rate-limit status from upload items to S3TaskState
        {
            let state = self.upload_state.lock().unwrap();
            let any_rate_limited = state.items.iter().any(|i| matches!(i.status, super::upload_app::UploadItemStatus::RateLimited));
            self.s3_state.lock().unwrap().set_rate_limited(any_rate_limited);
        }

        // Draw header
        let state = self.upload_state.lock().unwrap();
        let items_done = state.items.iter().filter(|i| matches!(i.status, super::upload_app::UploadItemStatus::Complete)).count();
        let items_total = state.items.len();
        let bytes = widgets::format_bytes(state.bytes_uploaded);
        let speed = format!("{}/s", widgets::format_bytes(state.throughput.throughput() as u64));
        let eta = widgets::format_eta(
            state.bytes_total.saturating_sub(state.bytes_uploaded),
            state.throughput.throughput(),  // f64, not cast to u64
        );
        drop(state);

        widgets::draw_header(frame, chunks[0], &self.theme, "ia upload", items_done, items_total, &bytes, &speed, &eta);
        widgets::draw_tab_bar(frame, chunks[1], &self.theme, self.active_tab);

        // Draw active tab content
        match self.active_tab {
            TabId::Upload => self.upload_tab.draw(frame, chunks[3], &self.theme),
            TabId::Tasks => self.tasks_tab.draw(frame, chunks[3], &self.theme),
            TabId::Log => self.log_tab.draw(frame, chunks[3], &self.theme),
            TabId::Errors => self.errors_tab.draw(frame, chunks[3], &self.theme),
        }

        // Draw footer with context-sensitive hints
        let hints = match self.active_tab {
            TabId::Upload => self.upload_tab.key_hints(),
            TabId::Tasks => self.tasks_tab.key_hints(),
            TabId::Log => self.log_tab.key_hints(),
            TabId::Errors => self.errors_tab.key_hints(),
        };
        let elapsed = widgets::format_elapsed(self.upload_state.lock().unwrap().throughput.elapsed());
        widgets::draw_footer(frame, chunks[4], &self.theme, &hints, &elapsed);

        // Help overlay on top
        if self.show_help {
            help::draw_help_overlay(frame, area, &self.theme, self.active_tab);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Help overlay intercepts all keys
        if self.show_help {
            self.show_help = false;
            return true;
        }

        // Ctrl-C always quits
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return true;
        }

        // When a tab is in search/input mode, route ALL keys to the tab first
        // so that typing '1', '2', 'q' etc. goes to the search input, not global keys
        if self.is_tab_consuming_input() {
            return match self.active_tab {
                TabId::Upload => self.upload_tab.handle_key(code, modifiers),
                TabId::Tasks => self.tasks_tab.handle_key(code, modifiers),
                TabId::Log => self.log_tab.handle_key(code, modifiers),
                TabId::Errors => self.errors_tab.handle_key(code, modifiers),
            };
        }

        // Global keys (only when no tab is consuming input)
        match code {
            KeyCode::Char('q') | KeyCode::Esc => { self.quit = true; return true; }
            KeyCode::Char('?') => { self.show_help = true; return true; }
            KeyCode::Char('1') => { self.active_tab = TabId::Upload; return true; }
            KeyCode::Char('2') => { self.active_tab = TabId::Tasks; return true; }
            KeyCode::Char('3') => { self.active_tab = TabId::Log; return true; }
            KeyCode::Char('4') => { self.active_tab = TabId::Errors; return true; }
            _ => {}
        }

        // Route to active tab
        match self.active_tab {
            TabId::Upload => self.upload_tab.handle_key(code, modifiers),
            TabId::Tasks => self.tasks_tab.handle_key(code, modifiers),
            TabId::Log => self.log_tab.handle_key(code, modifiers),
            TabId::Errors => self.errors_tab.handle_key(code, modifiers),
        }
    }

    fn is_done(&self) -> bool {
        let state = self.upload_state.lock().unwrap();
        state.done && state.active_files.is_empty()
    }

    fn quit_requested(&self) -> bool {
        self.quit
    }
}

impl MultiTabDashboard {
    /// Check if the active tab is in a mode that consumes all input (e.g., search).
    fn is_tab_consuming_input(&self) -> bool {
        match self.active_tab {
            TabId::Tasks => self.tasks_tab.search.is_active(),
            TabId::Log => self.log_tab.search.is_active(),
            _ => false,
        }
    }

    /// Tick all tabs for periodic work (joblog tailing, gg timeout, etc.).
    /// Called from the event loop before draw().
    pub fn tick_all(&mut self) {
        self.upload_tab.tick();
        self.tasks_tab.tick();
        self.log_tab.tick();
        self.errors_tab.tick();
    }
}
```

**Note:** Since `Dashboard::draw()` takes `&self`, we cannot call `tick_all()` from inside `draw()`. Instead, in Task 13 (integration), the event loop in `run_dashboard_and_summarize` must call `dashboard.tick_all()` before `dashboard.draw()` on each iteration. This requires either:
- Making `MultiTabDashboard` accessible via a mutable reference alongside the `Dashboard` trait, or
- Adding a `tick()` method to the `Dashboard` trait in `framework.rs` (preferred — add with empty default impl for backward compat)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib tui::dashboard -- --nocapture`
Expected: All 5 tests pass.

- [ ] **Step 5: Add module to mod.rs, run `just ci`, commit**

```bash
git add ia-cli/src/tui/dashboard.rs ia-cli/src/tui/mod.rs
git commit -m "feat(tui): add MultiTabDashboard implementing Dashboard trait

Wraps Upload/Tasks/Log/Errors tabs. Renders shared header, tab bar,
footer. Routes keys: 1-4 switch tabs, ? help overlay, q quit.
Delegates all other keys to active tab."
```

---

### Task 13: Integration — Wire Dashboard into Upload Pipeline

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`
- Modify: `ia-cli/src/tui/mod.rs`
- Modify: `ia-cli/src/commands/upload.rs`

**Context:** Replace the old `UploadDashboard` with `MultiTabDashboard`. Update the S3 polling to use `S3TaskState` with 15s interval + global count. Pass joblog path through to dashboard. Keep `UploadTuiState` and `run_dashboard_and_summarize` structure intact.

- [ ] **Step 1: Update `run_dashboard_and_summarize` to use MultiTabDashboard**

In `ia-cli/src/tui/upload_app.rs`:

1. Replace `UploadDashboard` struct and its `Dashboard` impl with a thin wrapper that creates `MultiTabDashboard`
2. Create `S3TaskState` (shared via `Arc<Mutex<>>`) alongside `UploadTuiState`
3. Create `JoblogState` from the joblog path
4. Update S3 polling loop:
   - Change interval from 60s to 15s
   - Write to `S3TaskState` instead of `UploadTuiState.tasks_*`
   - Add global count query: `get_tasks` with `TasksQuery { args: Some("*s3-put*"), limit: Some(0), summary: Some(true), ..Default::default() }` (no submitter filter). The `TasksSummary` has no `total` field, so compute: `global_count = summary.queued + summary.running + summary.error + summary.paused`
   - Convert `TaskEntry` results to `S3TaskEntry` for the task list
   - When toggling to global view (`u` key), re-query with `limit: Some(100)` and no submitter filter to cap result size
5. Update `UploadTuiState::update()` to set `s3_state.set_rate_limited()` when rate limit events occur
6. Pass `MultiTabDashboard` to `run_dashboard_sync` instead of `UploadDashboard`

- [ ] **Step 2: Update function signatures to accept joblog path**

In `upload_app.rs`, update `run_upload_tui` and `run_upload_batch_tui` to accept `joblog_path: Option<&Path>`.

In `commands/upload.rs`, pass `args.joblog.as_deref()` to the dashboard entry points.

- [ ] **Step 3: Remove old `UploadDashboard` struct and `upload_ui.rs` usage**

The old `UploadDashboard` (lines 354-400) and `upload_ui::draw` are no longer needed — all rendering is now in the tab modules. Mark `upload_ui.rs` functions as `#[allow(dead_code)]` initially, then remove in a follow-up cleanup once tests pass.

- [ ] **Step 4: Add `tick()` to `Dashboard` trait in `framework.rs`**

Add a default-impl `tick()` method to the `Dashboard` trait so `run_dashboard_sync` can call it before `draw()`:

```rust
// In framework.rs, add to Dashboard trait:
fn tick(&mut self) {}  // default no-op for backward compat
```

Update `run_dashboard_sync` to call `dashboard.tick()` before `dashboard.draw()` in the event loop. Then implement `tick()` on `MultiTabDashboard` to call `self.tick_all()`.

Also update the existing download `TuiState` `Dashboard` impl — no changes needed since the default impl is a no-op.

- [ ] **Step 5: Update mod.rs exports**

Update `ia-cli/src/tui/mod.rs` to export the new dashboard:
```rust
pub mod dashboard;
pub use upload_app::run_upload_tui;
pub use upload_app::run_upload_batch_tui;
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ia-cli -- --nocapture`
Expected: All tests pass. Existing upload_app tests should still pass since `UploadTuiState` is unchanged.

- [ ] **Step 6: Run `just ci`**

Run: `just ci`
Expected: All checks pass.

- [ ] **Step 7: Commit**

```bash
git add ia-cli/src/tui/upload_app.rs ia-cli/src/tui/mod.rs ia-cli/src/commands/upload.rs
git commit -m "feat(tui): wire MultiTabDashboard into upload pipeline

Replace UploadDashboard with MultiTabDashboard. S3 polling now
uses S3TaskState at 15s interval with global count query.
JoblogState created from --joblog path. Rate limit events
propagated to S3TaskState."
```

---

### Task 14: Cleanup and Final Polish

**Files:**
- Modify: `ia-cli/src/tui/upload_ui.rs` (remove or reduce)
- Modify: `ia-cli/src/tui/upload_app.rs` (remove old `UploadDashboard`)
- Modify: `ia-cli/src/tui/mod.rs`

- [ ] **Step 1: Remove old `upload_ui.rs` if fully superseded**

If all rendering has been migrated to `upload_tab.rs`, remove `upload_ui.rs` and its module declaration. If any shared functions remain, keep them.

- [ ] **Step 2: Remove old `UploadDashboard` struct**

Remove the old `UploadDashboard` struct and `impl Dashboard for UploadDashboard` from `upload_app.rs` if not already removed in Task 13.

- [ ] **Step 3: Remove `tasks_queued/running/error` from `UploadTuiState`**

These fields are now in `S3TaskState`. Remove them from `UploadTuiState` and update any references.

- [ ] **Step 4: Run full test suite**

Run: `cargo test -p ia-cli -- --nocapture`
Run: `cargo test -p ia-core -- --nocapture`
Expected: All tests pass.

- [ ] **Step 5: Run `just ci`**

Run: `just ci`
Expected: All checks pass.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "refactor(tui): remove old upload dashboard code

Remove UploadDashboard, upload_ui.rs, and tasks_* fields from
UploadTuiState. All rendering now handled by tab modules."
```

---

### Task 15: CLI Integration Tests

**Files:**
- Modify: `ia-cli/tests/` (add dashboard integration tests)

- [ ] **Step 1: Add integration test for `--dashboard` flag parsing**

Verify that `--dashboard` is accepted and mutually exclusive with `--json`.

- [ ] **Step 2: Add integration test for tab key routing**

Create a `MultiTabDashboard` with mock state, send key sequences, verify tab switching and key routing work correctly.

- [ ] **Step 3: Run `just ci` and commit**

```bash
git add ia-cli/tests/
git commit -m "test(tui): add integration tests for multi-tab dashboard

Tests for flag parsing, tab switching, key routing, and
help overlay toggling."
```

---

## Dependency Graph

```
Task 1 (Theme) ─────────────┐
Task 2 (TabView) ───────────┤
Task 3 (S3TaskState) ───────┤
Task 4 (JoblogState) ───────┼──→ Task 7 (Upload Tab) ──┐
Task 5 (Search) ────────────┤    Task 8 (Tasks Tab) ───┤
Task 6 (Shared Widgets) ────┘    Task 9 (Log Tab) ─────┼──→ Task 12 (Dashboard) ──→ Task 13 (Integration) ──→ Task 14 (Cleanup) ──→ Task 15 (Tests)
                                 Task 10 (Errors Tab) ─┤
                                 Task 11 (Help) ───────┘
```

**Parallelizable:**
- Tasks 1-6 can all run in parallel (no deps between them)
- Tasks 7-11 can all run in parallel (each depends only on Tasks 1-6)
- Tasks 12-15 are sequential
