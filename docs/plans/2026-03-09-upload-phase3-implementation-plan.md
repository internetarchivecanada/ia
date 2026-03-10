# Upload Phase 3 (Dashboard) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a ratatui TUI dashboard for `ia upload` and `ia upload import`, powered by a shared trait-based TUI framework that both download and upload dashboards use.

**Architecture:** Refactor the existing download TUI (`tui/app.rs` + `tui/ui.rs`) into a trait-based `Dashboard` framework with shared event loop, terminal lifecycle, and common widgets. Migrate the download dashboard onto it. Then build the upload dashboard as a second implementation. Add byte-level upload progress via an async body wrapper, and a minimal read-only Tasks API client for the S3 Tasks panel.

**Tech Stack:** ratatui 0.29, crossterm 0.28, tokio 1, reqwest 0.12, serde, wiremock (tests)

**Issues:** #217 (shared TUI infra), #218 (upload dashboard panels)

---

## Task 1: Shared TUI Framework — `Dashboard` Trait and Terminal Lifecycle

**Files:**
- Create: `ia-cli/src/tui/framework.rs`
- Modify: `ia-cli/src/tui/mod.rs:1-5`

This task extracts the reusable pieces from the download TUI into a framework module. The `Dashboard` trait defines the contract; the `run_dashboard()` function provides the event loop.

**Step 1: Create `framework.rs` with trait and terminal lifecycle**

```rust
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Terminal};

type Term = Terminal<CrosstermBackend<Stdout>>;

/// RAII guard — restores terminal on drop (including panics).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = io::stdout().execute(Show);
    }
}

/// Trait that download and upload dashboards implement.
pub trait Dashboard {
    /// Render all panels into the frame.
    fn draw(&self, frame: &mut ratatui::Frame);

    /// Handle a keyboard event. Return `true` if the dashboard should exit.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool;

    /// Return `true` when background work is done and the dashboard can exit
    /// (after the user presses q or all work completes).
    fn is_done(&self) -> bool;

    /// Whether a quit has been requested (q / Ctrl-C).
    fn quit_requested(&self) -> bool;
}

/// Run a dashboard until completion or user quit.
///
/// Sets up the terminal, enters the event loop at `tick_rate`, and cleans up
/// on exit. The caller is responsible for spawning async work that updates
/// the dashboard state before calling this.
pub fn run_dashboard_sync(
    terminal: &mut Term,
    dashboard: &mut dyn Dashboard,
    tick_rate: Duration,
) -> anyhow::Result<()> {
    let mut last_tick = Instant::now();
    let mut input_disabled = false;

    loop {
        // Draw
        terminal.draw(|f| dashboard.draw(f))?;

        // Exit check: quit requested, or all work done with no active transfers
        if dashboard.quit_requested() || dashboard.is_done() {
            break;
        }

        // Poll for input
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if !input_disabled {
                match event::read() {
                    Ok(Event::Key(key)) => {
                        if dashboard.handle_key(key.code, key.modifiers) {
                            break;
                        }
                    }
                    Err(_) => input_disabled = true,
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    Ok(())
}

/// Set up the terminal for TUI mode. Returns the terminal and a guard
/// that restores it on drop.
pub fn setup_terminal() -> anyhow::Result<(Term, TerminalGuard)> {
    if !atty::is(atty::Stream::Stdout) {
        anyhow::bail!("--dashboard requires an interactive terminal (stdout is not a TTY)");
    }
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok((terminal, TerminalGuard))
}
```

**Step 2: Update `tui/mod.rs` to export the framework**

```rust
pub mod ai;
mod app;
pub mod framework;
mod ui;

pub use app::run_tui;
```

**Step 3: Run `cargo check -p ia-cli`**

Expected: compiles. No tests yet — this is the skeleton.

**Step 4: Commit**

```
feat(tui): add Dashboard trait and shared terminal framework

Extract reusable TUI infrastructure into tui/framework.rs:
- Dashboard trait (draw, handle_key, is_done, quit_requested)
- run_dashboard_sync() event loop
- setup_terminal() with RAII TerminalGuard

Ref #217
```

---

## Task 2: Shared TUI Widgets — Throughput Tracker and Common Panels

**Files:**
- Create: `ia-cli/src/tui/widgets.rs`
- Modify: `ia-cli/src/tui/mod.rs`

Extract reusable rendering helpers that both dashboards will use.

**Step 1: Create `widgets.rs` with throughput tracker and panel helpers**

```rust
use std::time::{Duration, Instant};

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Sparkline},
    Frame,
};

// ── Throughput Tracker ──────────────────────────────────────────────

/// Tracks bytes-over-time and maintains a 60-sample history for sparklines.
pub struct ThroughputTracker {
    pub history: Vec<f64>,
    last_sample: Instant,
    total_bytes: u64,
    started_at: Instant,
}

impl ThroughputTracker {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            history: Vec::with_capacity(60),
            last_sample: now,
            total_bytes: 0,
            started_at: now,
        }
    }

    /// Set the current total bytes transferred. Call this on every progress update.
    pub fn set_bytes(&mut self, total: u64) {
        self.total_bytes = total;
    }

    /// Sample throughput if >= 1 second has elapsed. Call from the update path.
    pub fn maybe_sample(&mut self) {
        if self.last_sample.elapsed() >= Duration::from_secs(1) {
            self.history.push(self.throughput());
            if self.history.len() > 60 {
                self.history.remove(0);
            }
            self.last_sample = Instant::now();
        }
    }

    pub fn throughput(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.total_bytes as f64 / elapsed
        } else {
            0.0
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

// ── Format Helpers ──────────────────────────────────────────────────

/// Format bytes as human-readable (e.g. "1.2 GiB").
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;

    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1} TiB", b / TIB)
    } else if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Format a duration as "XhYYmZZs", "YmZZs", or "Zs".
pub fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Format ETA string from remaining bytes and speed.
pub fn format_eta(remaining_bytes: u64, bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 || remaining_bytes == 0 {
        return String::new();
    }
    let eta_secs = (remaining_bytes as f64 / bytes_per_sec) as u64;
    if eta_secs >= 3600 {
        format!("  ETA {}h{:02}m", eta_secs / 3600, (eta_secs % 3600) / 60)
    } else if eta_secs >= 60 {
        format!("  ETA {}m{:02}s", eta_secs / 60, eta_secs % 60)
    } else {
        format!("  ETA {eta_secs}s")
    }
}

// ── Common Panel Widgets ────────────────────────────────────────────

/// Draw a throughput sparkline panel.
pub fn draw_throughput_panel(frame: &mut Frame, area: Rect, history: &[f64]) {
    let data: Vec<u64> = history.iter().map(|v| *v as u64).collect();
    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Throughput (last 60s) "),
        )
        .data(&data)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(sparkline, area);
}

/// Draw an errors panel showing recent failures.
pub fn draw_errors_panel(frame: &mut Frame, area: Rect, errors: &[(String, String)]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(format!(" Errors ({}) ", errors.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_lines = inner.height as usize;
    let visible: Vec<Line> = errors
        .iter()
        .rev()
        .take(max_lines)
        .map(|(name, msg)| {
            Line::from(vec![
                Span::styled("✗ ", Style::default().fg(Color::Red)),
                Span::styled(name.clone(), Style::default().fg(Color::White)),
                Span::raw("  "),
                Span::styled(msg.clone(), Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

/// Draw a keyboard-help status line.
pub fn draw_key_hints(frame: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let spans: Vec<Span> = hints
        .iter()
        .flat_map(|(key, label)| {
            vec![
                Span::styled(
                    format!("[{key}]"),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(format!(" {label}  "), Style::default().fg(Color::DarkGray)),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

**Step 2: Update `tui/mod.rs`**

```rust
pub mod ai;
mod app;
pub mod framework;
mod ui;
pub mod widgets;

pub use app::run_tui;
```

**Step 3: Run `cargo check -p ia-cli`**

Expected: compiles.

**Step 4: Commit**

```
feat(tui): add shared widget library for dashboard panels

ThroughputTracker, format helpers, and common panel widgets
(throughput sparkline, errors panel, key hints) extracted into
tui/widgets.rs for reuse by both download and upload dashboards.

Ref #217
```

---

## Task 3: Migrate Download Dashboard to Shared Framework

**Files:**
- Modify: `ia-cli/src/tui/app.rs:1-973`
- Modify: `ia-cli/src/tui/ui.rs:1-502`

Refactor the download dashboard to use the `Dashboard` trait, `run_dashboard_sync()`, `setup_terminal()`, `ThroughputTracker`, and shared widgets. The download TUI must remain fully functional — all existing tests must pass.

**Step 1: Refactor `TuiState` to use `ThroughputTracker`**

Replace the manual throughput sampling in `TuiState::update()` (lines 300-307 in `app.rs`) with `self.throughput.set_bytes(self.bytes_downloaded)` + `self.throughput.maybe_sample()`. Remove the `throughput_history`, `last_throughput_sample`, and `started_at` fields from `TuiState`, replacing them with a single `throughput: ThroughputTracker` field.

Update all methods that reference `self.throughput_history` → `self.throughput.history`, `self.started_at` → use `self.throughput.elapsed()`, `self.throughput()` → `self.throughput.throughput()`.

**Step 2: Implement `Dashboard` for download state**

Add a wrapper struct (or implement directly on a mutex-guarded state) that implements the `Dashboard` trait:

```rust
impl Dashboard for DownloadDashboard {
    fn draw(&self, frame: &mut ratatui::Frame) {
        let state = self.state.lock().unwrap();
        ui::draw(frame, &state);
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // existing j/k/q/Ctrl-C logic
    }

    fn is_done(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.done && state.active_files.is_empty()
    }

    fn quit_requested(&self) -> bool {
        self.state.lock().unwrap().quit_requested
    }
}
```

**Step 3: Refactor `run_tui()` to use `setup_terminal()` + `run_dashboard_sync()`**

Replace the manual terminal setup (lines 818-823), event loop (lines 878-933), and cleanup code with calls to the framework functions. Keep the task-spawning logic (lines 826-876) as-is — it's download-specific.

The function signature stays the same so `commands/download.rs` doesn't change.

**Step 4: Update `ui.rs` to use shared widgets**

Replace `draw_throughput_panel()` and `draw_errors_panel()` in `ui.rs` with calls to `widgets::draw_throughput_panel()` and `widgets::draw_errors_panel()`. Replace the inline elapsed-time formatting with `widgets::format_elapsed()`. Remove duplicated format helpers.

**Step 5: Run all tests**

Run: `cargo test -p ia-cli -p ia-core`

Expected: ALL existing tests pass. The download dashboard should be functionally identical.

**Step 6: Commit**

```
refactor(tui): migrate download dashboard to shared Dashboard framework

- TuiState now uses ThroughputTracker from widgets.rs
- DownloadDashboard implements Dashboard trait
- run_tui() uses setup_terminal() + run_dashboard_sync()
- ui.rs delegates to shared widget functions
- All existing tests pass unchanged

Ref #217
```

---

## Task 4: Byte-Level Upload Progress — Async Body Wrapper

**Files:**
- Create: `ia-core/src/upload/progress_body.rs`
- Modify: `ia-core/src/upload/single.rs:226-231`
- Modify: `ia-core/src/upload/mod.rs:1-11`

**Step 1: Write tests for the progress body wrapper**

Create tests in `progress_body.rs` that verify:
- Bytes are reported accurately as chunks are read
- The callback fires with cumulative byte counts
- Total bytes match the file size after full read

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_progress_body_reports_bytes() {
        let data = vec![0u8; 1024 * 100]; // 100 KiB
        let reported = Arc::new(AtomicU64::new(0));
        let reported_clone = Arc::clone(&reported);

        let stream = ProgressBody::wrap(
            tokio::io::BufReader::new(&data[..]),
            data.len() as u64,
            move |bytes_sent| {
                reported_clone.store(bytes_sent, Ordering::SeqCst);
            },
        );

        // Consume the entire stream
        use futures::StreamExt;
        let body = reqwest::Body::wrap_stream(stream);
        let mut collected = Vec::new();
        let mut stream = body.into_stream();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }

        assert_eq!(collected.len(), data.len());
        assert_eq!(reported.load(Ordering::SeqCst), data.len() as u64);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core progress_body`

Expected: FAIL — module doesn't exist yet.

**Step 3: Implement `ProgressBody`**

```rust
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncRead;

/// Wraps an AsyncRead into a Stream<Item = Result<Bytes>> that reports
/// cumulative bytes read via a callback. Used to provide byte-level
/// upload progress for the TUI dashboard.
pub struct ProgressBody<R, F> {
    reader: R,
    callback: F,
    bytes_sent: u64,
    buf: Vec<u8>,
}

impl<R, F> ProgressBody<R, F>
where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    F: Fn(u64) + Send + Sync + 'static,
{
    /// Chunk size for reading — 64 KiB balances progress granularity and throughput.
    const CHUNK_SIZE: usize = 64 * 1024;

    pub fn wrap(reader: R, _total: u64, callback: F) -> impl Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + Sync {
        ProgressBody {
            reader,
            callback,
            bytes_sent: 0,
            buf: vec![0u8; Self::CHUNK_SIZE],
        }
    }
}

impl<R, F> Stream for ProgressBody<R, F>
where
    R: AsyncRead + Unpin + Send + Sync,
    F: Fn(u64) + Unpin + Send + Sync,
{
    type Item = Result<bytes::Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut read_buf = tokio::io::ReadBuf::new(&mut this.buf);
        match Pin::new(&mut this.reader).poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => {
                let n = read_buf.filled().len();
                if n == 0 {
                    return Poll::Ready(None); // EOF
                }
                this.bytes_sent += n as u64;
                (this.callback)(this.bytes_sent);
                Poll::Ready(Some(Ok(bytes::Bytes::copy_from_slice(&this.buf[..n]))))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}
```

**Step 4: Add to upload module**

In `ia-core/src/upload/mod.rs`, add:

```rust
pub(crate) mod progress_body;
```

**Step 5: Run tests**

Run: `cargo test -p ia-core progress_body`

Expected: PASS.

**Step 6: Wire into `single.rs`**

In `ia-core/src/upload/single.rs`, replace lines 229-231:

```rust
// Before:
let file_handle = tokio::fs::File::open(file).await?;
let stream_body = reqwest::Body::from(file_handle);
let response = request.body(stream_body).send().await;
```

With:

```rust
let file_handle = tokio::fs::File::open(file).await?;

let response = if let Some(cb) = progress {
    let id = identifier.to_string();
    let k = key.to_string();
    let fs = file_size;
    let stream = progress_body::ProgressBody::wrap(
        file_handle,
        file_size,
        move |bytes_sent| {
            cb(UploadProgress {
                identifier: id.clone(),
                key: k.clone(),
                bytes_sent,
                total_bytes: fs,
                status: UploadProgressStatus::Uploading,
            });
        },
    );
    request
        .header("Content-Length", file_size.to_string())
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
} else {
    let stream_body = reqwest::Body::from(file_handle);
    request.body(stream_body).send().await
};
```

Note: Content-Length is already set earlier (line 186), so the stream body works. IA S3 requires Content-Length (no chunked encoding), and `wrap_stream` uses chunked by default with reqwest — but since Content-Length is explicitly set, reqwest uses that instead. Verify this in testing.

**Step 7: Run all upload tests**

Run: `cargo test -p ia-core upload`

Expected: ALL existing upload tests pass.

**Step 8: Commit**

```
feat(upload): add byte-level progress via async body wrapper

ProgressBody wraps a tokio AsyncRead into a Stream that fires a
callback with cumulative bytes sent. Wired into upload_file() so
the dashboard can show animated progress bars during PUT transfers.

Ref #218
```

---

## Task 5: Minimal Tasks API Client

**Files:**
- Create: `ia-core/src/tasks.rs`
- Modify: `ia-core/src/lib.rs`

Minimal read-only tasks API for the S3 Tasks panel. Just list tasks filtered by identifier/cmd.

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path, query_param};

    #[tokio::test]
    async fn test_get_tasks_by_identifier() {
        let mock_server = MockServer::start().await;
        let response_body = serde_json::json!({
            "success": true,
            "value": {
                "summary": {"queued": 2, "running": 1, "error": 0, "paused": 0},
                "catalog": [
                    {
                        "task_id": 12345,
                        "identifier": "test-item",
                        "cmd": "derive.php",
                        "submitter": "user@example.com",
                        "submittime": "2026-03-09 12:00:00",
                        "server": "ia123456",
                        "color": "green",
                        "priority": 0,
                        "args": {}
                    }
                ]
            }
        });

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("identifier", "test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        let client = crate::test_utils::mock_client(&mock_server).await;
        let result = get_tasks(&client, &TasksQuery {
            identifier: Some("test-item".to_string()),
            ..Default::default()
        }).await.unwrap();

        assert_eq!(result.summary.queued, 2);
        assert_eq!(result.summary.running, 1);
        assert_eq!(result.catalog.len(), 1);
        assert_eq!(result.catalog[0].identifier, "test-item");
    }

    #[tokio::test]
    async fn test_get_tasks_summary_only() {
        let mock_server = MockServer::start().await;
        let response_body = serde_json::json!({
            "success": true,
            "value": {
                "summary": {"queued": 5, "running": 3, "error": 1, "paused": 0},
                "catalog": []
            }
        });

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        let client = crate::test_utils::mock_client(&mock_server).await;
        let result = get_tasks(&client, &TasksQuery::default()).await.unwrap();

        assert_eq!(result.summary.queued, 5);
        assert_eq!(result.summary.running, 3);
        assert_eq!(result.summary.error, 1);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core tasks`

Expected: FAIL — module doesn't exist.

**Step 3: Implement minimal tasks client**

```rust
use serde::Deserialize;

use crate::{IaClient, Result};

/// Query parameters for the Tasks API.
#[derive(Debug, Default, Clone)]
pub struct TasksQuery {
    pub identifier: Option<String>,
    pub cmd: Option<String>,
    pub limit: Option<u32>,
}

/// Response from the Tasks API.
#[derive(Debug, Deserialize)]
pub struct TasksResponse {
    pub success: bool,
    pub value: TasksValue,
}

#[derive(Debug, Deserialize)]
pub struct TasksValue {
    pub summary: TasksSummary,
    #[serde(default)]
    pub catalog: Vec<TaskEntry>,
}

/// Aggregate task counts.
#[derive(Debug, Deserialize)]
pub struct TasksSummary {
    #[serde(default)]
    pub queued: u32,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub error: u32,
    #[serde(default)]
    pub paused: u32,
}

/// A single task entry from the catalog.
#[derive(Debug, Deserialize)]
pub struct TaskEntry {
    pub task_id: u64,
    pub identifier: String,
    pub cmd: String,
    #[serde(default)]
    pub submitter: String,
    #[serde(default)]
    pub submittime: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub priority: i32,
}

/// Fetch tasks from the archive.org Tasks API (read-only).
pub async fn get_tasks(client: &IaClient, query: &TasksQuery) -> Result<TasksValue> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let mut req = client
        .http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"));

    if let Some(ref id) = query.identifier {
        req = req.query(&[("identifier", id.as_str())]);
    }
    if let Some(ref cmd) = query.cmd {
        req = req.query(&[("cmd", cmd.as_str())]);
    }
    if let Some(limit) = query.limit {
        req = req.query(&[("limit", &limit.to_string())]);
    }

    let resp: TasksResponse = req.send().await?.json().await?;
    Ok(resp.value)
}
```

**Step 4: Add to `lib.rs`**

In `ia-core/src/lib.rs`, add:

```rust
pub mod tasks;
```

**Step 5: Run tests**

Run: `cargo test -p ia-core tasks`

Expected: PASS.

**Step 6: Commit**

```
feat(core): add minimal read-only Tasks API client

Supports GET /services/tasks.php with identifier/cmd/limit filters.
Returns task summary (queued/running/error/paused counts) and catalog
entries. Will power the S3 Tasks panel in the upload dashboard and
later expand into a full `ia tasks` CLI command.

Ref #218
```

---

## Task 6: Upload TUI State Machine

**Files:**
- Create: `ia-cli/src/tui/upload_app.rs`

This is the upload equivalent of `app.rs` — state management, progress callback handling, and `Dashboard` trait implementation.

**Step 1: Write tests for upload TUI state**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ia_core::upload::{UploadProgress, UploadProgressStatus};

    #[test]
    fn test_single_file_progress_tracking() {
        let mut state = UploadTuiState::new(&["test-item".to_string()]);

        // Verifying phase
        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            bytes_sent: 0,
            total_bytes: 1000,
            status: UploadProgressStatus::Verifying,
        });
        assert_eq!(state.active_files.len(), 1);

        // Uploading with byte progress
        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            bytes_sent: 500,
            total_bytes: 1000,
            status: UploadProgressStatus::Uploading,
        });
        assert_eq!(state.bytes_uploaded, 500);

        // Complete
        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            bytes_sent: 1000,
            total_bytes: 1000,
            status: UploadProgressStatus::Complete,
        });
        assert_eq!(state.bytes_uploaded, 1000);
        assert_eq!(state.files_completed, 1);
        assert!(state.active_files.is_empty());
    }

    #[test]
    fn test_batch_progress_multiple_items() {
        let mut state = UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
        ]);
        assert_eq!(state.items.len(), 2);

        // Item A starts uploading
        state.update(UploadProgress {
            identifier: "item-a".into(),
            key: "a.txt".into(),
            bytes_sent: 0,
            total_bytes: 100,
            status: UploadProgressStatus::Uploading,
        });

        let item_a = &state.items[*state.item_index.get("item-a").unwrap()];
        assert!(matches!(item_a.status, UploadItemStatus::Uploading));
    }

    #[test]
    fn test_rate_limit_tracking() {
        let mut state = UploadTuiState::new(&["test-item".to_string()]);

        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            bytes_sent: 0,
            total_bytes: 1000,
            status: UploadProgressStatus::WaitingRateLimit,
        });

        let item = &state.items[0];
        assert!(matches!(item.status, UploadItemStatus::RateLimited));
    }

    #[test]
    fn test_skipped_files() {
        let mut state = UploadTuiState::new(&["test-item".to_string()]);

        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "existing.pdf".into(),
            bytes_sent: 0,
            total_bytes: 500,
            status: UploadProgressStatus::Skipped,
        });

        assert_eq!(state.files_skipped, 1);
        assert!(state.active_files.is_empty());
    }

    #[test]
    fn test_failed_files_tracked_as_errors() {
        let mut state = UploadTuiState::new(&["test-item".to_string()]);

        state.update(UploadProgress {
            identifier: "test-item".into(),
            key: "bad.pdf".into(),
            bytes_sent: 0,
            total_bytes: 500,
            status: UploadProgressStatus::Failed,
        });

        assert_eq!(state.files_failed, 1);
        assert_eq!(state.failed_files.len(), 1);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli upload_app`

Expected: FAIL — module doesn't exist.

**Step 3: Implement `UploadTuiState`**

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use ia_core::upload::{UploadProgress, UploadProgressStatus};

use super::framework::Dashboard;
use super::widgets::ThroughputTracker;

// ── State ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadItemStatus {
    Pending,
    Verifying,
    Uploading,
    RateLimited,
    Complete,
    Failed(String),
}

pub struct UploadItemState {
    pub identifier: String,
    pub status: UploadItemStatus,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_uploaded: u64,
    pub started_at: Instant,
}

pub struct UploadFileProgress {
    pub name: String,
    pub identifier: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub status: UploadProgressStatus,
    pub started_at: Instant,
}

pub struct UploadTuiState {
    pub items: Vec<UploadItemState>,
    pub item_index: HashMap<String, usize>,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_uploaded: u64,
    pub bytes_total: u64,
    pub active_files: HashMap<String, UploadFileProgress>,
    pub completed_files: Vec<String>,
    pub failed_files: Vec<(String, String)>,
    pub throughput: ThroughputTracker,
    pub scroll_offset: usize,
    pub quit_requested: bool,
    pub done: bool,

    // S3 Tasks panel state (updated by polling task)
    pub tasks_queued: u32,
    pub tasks_running: u32,
    pub tasks_error: u32,
}

impl UploadTuiState {
    pub fn new(identifiers: &[String]) -> Self {
        let mut items = Vec::with_capacity(identifiers.len());
        let mut item_index = HashMap::with_capacity(identifiers.len());

        for (i, id) in identifiers.iter().enumerate() {
            item_index.insert(id.clone(), i);
            items.push(UploadItemState {
                identifier: id.clone(),
                status: UploadItemStatus::Pending,
                files_total: 0,
                files_completed: 0,
                files_skipped: 0,
                files_failed: 0,
                bytes_uploaded: 0,
                started_at: Instant::now(),
            });
        }

        Self {
            items,
            item_index,
            files_total: 0,
            files_completed: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_uploaded: 0,
            bytes_total: 0,
            active_files: HashMap::new(),
            completed_files: Vec::new(),
            failed_files: Vec::new(),
            throughput: ThroughputTracker::new(),
            scroll_offset: 0,
            quit_requested: false,
            done: false,
            tasks_queued: 0,
            tasks_running: 0,
            tasks_error: 0,
        }
    }

    fn file_key(identifier: &str, key: &str) -> String {
        format!("{identifier}\0{key}")
    }

    pub fn update(&mut self, p: UploadProgress) {
        let fk = Self::file_key(&p.identifier, &p.key);

        // Ensure item exists and update its status
        if let Some(&idx) = self.item_index.get(&p.identifier) {
            let item = &mut self.items[idx];

            match p.status {
                UploadProgressStatus::Verifying => {
                    if item.status == UploadItemStatus::Pending {
                        item.status = UploadItemStatus::Verifying;
                    }
                    self.active_files.entry(fk.clone()).or_insert_with(|| {
                        self.files_total += 1;
                        item.files_total += 1;
                        self.bytes_total += p.total_bytes;
                        UploadFileProgress {
                            name: p.key.clone(),
                            identifier: p.identifier.clone(),
                            bytes_sent: 0,
                            total_bytes: p.total_bytes,
                            status: UploadProgressStatus::Verifying,
                            started_at: Instant::now(),
                        }
                    });
                    if let Some(fp) = self.active_files.get_mut(&fk) {
                        fp.status = UploadProgressStatus::Verifying;
                    }
                }

                UploadProgressStatus::Uploading => {
                    item.status = UploadItemStatus::Uploading;

                    // Track byte-level progress
                    let prev_bytes = self
                        .active_files
                        .get(&fk)
                        .map(|fp| fp.bytes_sent)
                        .unwrap_or(0);
                    let delta = p.bytes_sent.saturating_sub(prev_bytes);
                    self.bytes_uploaded += delta;
                    item.bytes_uploaded += delta;

                    let fp = self.active_files.entry(fk).or_insert_with(|| {
                        self.files_total += 1;
                        item.files_total += 1;
                        self.bytes_total += p.total_bytes;
                        UploadFileProgress {
                            name: p.key.clone(),
                            identifier: p.identifier.clone(),
                            bytes_sent: 0,
                            total_bytes: p.total_bytes,
                            status: UploadProgressStatus::Uploading,
                            started_at: Instant::now(),
                        }
                    });
                    fp.bytes_sent = p.bytes_sent;
                    fp.status = UploadProgressStatus::Uploading;
                }

                UploadProgressStatus::WaitingRateLimit => {
                    item.status = UploadItemStatus::RateLimited;
                    if let Some(fp) = self.active_files.get_mut(&fk) {
                        fp.status = UploadProgressStatus::WaitingRateLimit;
                    }
                }

                UploadProgressStatus::Complete => {
                    // Final byte accounting
                    let prev_bytes = self
                        .active_files
                        .get(&fk)
                        .map(|fp| fp.bytes_sent)
                        .unwrap_or(0);
                    let delta = p.total_bytes.saturating_sub(prev_bytes);
                    self.bytes_uploaded += delta;
                    item.bytes_uploaded += delta;

                    self.active_files.remove(&fk);
                    self.files_completed += 1;
                    item.files_completed += 1;
                    self.completed_files.push(p.key.clone());

                    // Check if item is fully done
                    if item.files_completed + item.files_skipped + item.files_failed
                        == item.files_total
                        && item.files_total > 0
                    {
                        item.status = if item.files_failed > 0 {
                            UploadItemStatus::Failed("some files failed".into())
                        } else {
                            UploadItemStatus::Complete
                        };
                    }
                }

                UploadProgressStatus::Skipped => {
                    self.active_files.remove(&fk);
                    self.files_skipped += 1;
                    item.files_skipped += 1;
                }

                UploadProgressStatus::Failed => {
                    self.active_files.remove(&fk);
                    self.files_failed += 1;
                    item.files_failed += 1;
                    self.failed_files
                        .push((p.key.clone(), "upload failed".into()));
                }
            }
        }

        // Sample throughput
        self.throughput.set_bytes(self.bytes_uploaded);
        self.throughput.maybe_sample();
    }

    pub fn overall_progress(&self) -> f64 {
        if self.bytes_total == 0 {
            return 0.0;
        }
        (self.bytes_uploaded as f64 / self.bytes_total as f64).min(1.0)
    }
}

// ── Dashboard wrapper ───────────────────────────────────────────────

pub struct UploadDashboard {
    pub state: std::sync::Arc<Mutex<UploadTuiState>>,
}

impl Dashboard for UploadDashboard {
    fn draw(&self, frame: &mut ratatui::Frame) {
        let state = self.state.lock().unwrap();
        super::upload_ui::draw(frame, &state);
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut state = self.state.lock().unwrap();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                state.quit_requested = true;
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                state.quit_requested = true;
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                state.scroll_offset = state.scroll_offset.saturating_add(1);
                false
            }
            KeyCode::Char('k') | KeyCode::Up => {
                state.scroll_offset = state.scroll_offset.saturating_sub(1);
                false
            }
            _ => false,
        }
    }

    fn is_done(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.done && state.active_files.is_empty()
    }

    fn quit_requested(&self) -> bool {
        self.state.lock().unwrap().quit_requested
    }
}
```

**Step 4: Add to `tui/mod.rs`**

```rust
pub mod ai;
mod app;
pub mod framework;
mod ui;
pub mod upload_app;
pub mod upload_ui;
pub mod widgets;

pub use app::run_tui;
```

**Step 5: Run tests**

Run: `cargo test -p ia-cli upload_app`

Expected: PASS.

**Step 6: Commit**

```
feat(tui): add upload TUI state machine and Dashboard impl

UploadTuiState tracks per-item and per-file upload progress with
byte-level granularity, rate limit status, and throughput sampling.
UploadDashboard implements the Dashboard trait for the event loop.

Ref #218
```

---

## Task 7: Upload TUI Rendering — Panel Layout and Widgets

**Files:**
- Create: `ia-cli/src/tui/upload_ui.rs`

The upload dashboard's rendering function and all 6 panels.

**Step 1: Implement `draw()` and all panels**

```rust
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};

use super::upload_app::{UploadItemStatus, UploadTuiState};
use super::widgets;
use ia_core::upload::UploadProgressStatus;

pub fn draw(frame: &mut Frame, state: &UploadTuiState) {
    let area = frame.area().inner(Margin::new(1, 0));

    // Build dynamic layout constraints based on available panels
    let mut constraints = vec![Constraint::Length(3)]; // Header

    let show_items = state.items.len() > 1;
    if show_items {
        constraints.push(Constraint::Min(5)); // Items panel
    }

    constraints.push(Constraint::Min(5)); // Files (workers) panel

    let show_s3_tasks = state.tasks_queued > 0 || state.tasks_running > 0 || state.tasks_error > 0;
    if show_s3_tasks {
        constraints.push(Constraint::Length(3)); // S3 Tasks
    }

    let show_rate_limit = state
        .items
        .iter()
        .any(|i| i.status == UploadItemStatus::RateLimited);
    if show_rate_limit {
        constraints.push(Constraint::Length(3)); // Rate Limit
    }

    let show_errors = !state.failed_files.is_empty();
    if show_errors {
        constraints.push(Constraint::Length(5)); // Errors
    }

    let show_throughput = !state.throughput.history.is_empty();
    if show_throughput {
        constraints.push(Constraint::Length(4)); // Throughput
    }

    constraints.push(Constraint::Length(3)); // Status bar

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut idx = 0;

    // Header
    draw_header(frame, chunks[idx], state);
    idx += 1;

    // Items
    if show_items {
        draw_items_panel(frame, chunks[idx], state);
        idx += 1;
    }

    // Files (active transfers)
    draw_active_files(frame, chunks[idx], state);
    idx += 1;

    // S3 Tasks
    if show_s3_tasks {
        draw_s3_tasks_panel(frame, chunks[idx], state);
        idx += 1;
    }

    // Rate Limit
    if show_rate_limit {
        draw_rate_limit_panel(frame, chunks[idx], state);
        idx += 1;
    }

    // Errors
    if show_errors {
        widgets::draw_errors_panel(frame, chunks[idx], &state.failed_files);
        idx += 1;
    }

    // Throughput
    if show_throughput {
        widgets::draw_throughput_panel(frame, chunks[idx], &state.throughput.history);
        idx += 1;
    }

    // Status bar
    draw_status_bar(frame, chunks[idx], state);
}

fn draw_header(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let progress = state.overall_progress();
    let throughput = state.throughput.throughput();
    let remaining = state.bytes_total.saturating_sub(state.bytes_uploaded);
    let eta = widgets::format_eta(remaining, throughput);

    let label = format!(
        "{}  {:.0}% ({})  {}/s{}",
        if state.items.len() == 1 {
            state.items[0].identifier.clone()
        } else {
            format!("{} items", state.items.len())
        },
        progress * 100.0,
        widgets::format_bytes(state.bytes_uploaded),
        widgets::format_bytes(throughput as u64),
        eta,
    );

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" ia upload "),
        )
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .ratio(progress.min(1.0))
        .label(label);

    frame.render_widget(gauge, area);
}

fn draw_items_panel(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(" Items ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    for item in &state.items {
        let (icon, icon_color) = match &item.status {
            UploadItemStatus::Pending => ("  ", Color::DarkGray),
            UploadItemStatus::Verifying => ("◇", Color::Yellow),
            UploadItemStatus::Uploading => ("▸", Color::Cyan),
            UploadItemStatus::RateLimited => ("⏸", Color::Yellow),
            UploadItemStatus::Complete => ("✓", Color::Green),
            UploadItemStatus::Failed(_) => ("✗", Color::Red),
        };

        let id_display = if item.identifier.len() > 25 {
            format!("…{}", &item.identifier[item.identifier.len() - 24..])
        } else {
            item.identifier.clone()
        };

        let files_str = format!("{}/{}", item.files_completed, item.files_total);

        lines.push(Line::from(vec![
            Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
            Span::styled(
                format!("{id_display:<25}"),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("  {files_str:>7}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  {:>10}", widgets::format_bytes(item.bytes_uploaded)),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        if lines.len() >= inner.height as usize {
            break;
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_active_files(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let count = state.active_files.len();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(format!(" Workers ({count}) "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.active_files.is_empty() {
        let msg = if state.done {
            Span::styled(
                "All uploads complete.",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "Waiting for uploads to start...",
                Style::default().fg(Color::DarkGray),
            )
        };
        frame.render_widget(Paragraph::new(Line::from(msg)), inner);
        return;
    }

    let mut files: Vec<_> = state.active_files.values().collect();
    files.sort_by_key(|f| &f.name);

    let mut lines = Vec::new();
    for fp in files.iter().skip(state.scroll_offset) {
        let (progress_bar, pct) = if fp.total_bytes > 0 {
            let ratio = fp.bytes_sent as f64 / fp.total_bytes as f64;
            let filled = (ratio * 20.0) as usize;
            let empty = 20 - filled;
            (
                format!("{}{}", "█".repeat(filled), "░".repeat(empty)),
                format!("{:5.1}%", ratio * 100.0),
            )
        } else {
            ("░".repeat(20), "  0.0%".into())
        };

        let name = if fp.name.len() > 30 {
            format!("…{}", &fp.name[fp.name.len() - 29..])
        } else {
            fp.name.clone()
        };

        let status_icon = match fp.status {
            UploadProgressStatus::Verifying => ("◇", Color::Yellow),
            UploadProgressStatus::Uploading => ("↑", Color::Cyan),
            UploadProgressStatus::WaitingRateLimit => ("⏸", Color::Yellow),
            _ => (" ", Color::DarkGray),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", status_icon.0),
                Style::default().fg(status_icon.1),
            ),
            Span::styled(progress_bar, Style::default().fg(Color::Cyan)),
            Span::styled(format!(" {pct}"), Style::default().fg(Color::White)),
            Span::styled(format!("  {name:<30}"), Style::default().fg(Color::White)),
        ]));

        if lines.len() >= inner.height as usize {
            break;
        }
    }

    // Show recent completions if space remains
    let remaining_lines = (inner.height as usize).saturating_sub(lines.len());
    if remaining_lines > 0 && !state.completed_files.is_empty() {
        let recent: Vec<_> = state
            .completed_files
            .iter()
            .rev()
            .take(remaining_lines.min(3))
            .collect();
        for name in recent {
            lines.push(Line::from(vec![
                Span::styled(" ✓ ", Style::default().fg(Color::Green)),
                Span::styled(name.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_s3_tasks_panel(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" S3 Tasks ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("  Queued: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state.tasks_queued.to_string(),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("  Running: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state.tasks_running.to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("  Errors: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state.tasks_error.to_string(),
            Style::default().fg(if state.tasks_error > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ),
    ]);

    frame.render_widget(Paragraph::new(line), inner);
}

fn draw_rate_limit_panel(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Rate Limited ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let limited: Vec<_> = state
        .items
        .iter()
        .filter(|i| i.status == UploadItemStatus::RateLimited)
        .collect();

    let mut lines = Vec::new();
    for item in limited {
        lines.push(Line::from(vec![
            Span::styled(" ⏸ ", Style::default().fg(Color::Yellow)),
            Span::styled(&item.identifier, Style::default().fg(Color::White)),
            Span::styled(
                "  polling check_limit...",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        if lines.len() >= inner.height as usize {
            break;
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_status_bar(frame: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    // Status line
    let elapsed = widgets::format_elapsed(state.throughput.elapsed());
    let is_batch = state.items.len() > 1;

    let status = if is_batch {
        let items_done = state
            .items
            .iter()
            .filter(|i| matches!(i.status, UploadItemStatus::Complete))
            .count();
        let items_failed = state
            .items
            .iter()
            .filter(|i| matches!(i.status, UploadItemStatus::Failed(_)))
            .count();

        let mut spans = vec![
            Span::styled("  Items: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}/{} done", items_done, state.items.len()),
                Style::default().fg(if items_done == state.items.len() {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ];

        if items_failed > 0 {
            spans.push(Span::styled(
                format!(", {items_failed} failed"),
                Style::default().fg(Color::Red),
            ));
        }

        spans.extend([
            Span::styled("  Files: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(
                    "{} done, {} skipped, {} failed",
                    state.files_completed, state.files_skipped, state.files_failed,
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("  {elapsed}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        Line::from(spans)
    } else {
        let remaining = state.files_total.saturating_sub(
            state.files_completed + state.files_skipped + state.files_failed,
        );
        Line::from(vec![
            Span::styled("  Queue: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{remaining} remaining"),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!(
                    "  Done: {}  Skipped: {}  Failed: {}",
                    state.files_completed, state.files_skipped, state.files_failed,
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  {elapsed}"),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    frame.render_widget(Paragraph::new(status), rows[0]);

    // Key hints
    widgets::draw_key_hints(frame, rows[1], &[("j/k", "scroll"), ("q", "uit")]);
}
```

**Step 2: Run `cargo check -p ia-cli`**

Expected: compiles.

**Step 3: Commit**

```
feat(tui): add upload dashboard panel rendering

Six panels: header gauge, items list, active files with progress bars,
S3 tasks summary, rate limit status, and status bar. Shared widgets
(throughput sparkline, errors, key hints) from widgets.rs.

Ref #218
```

---

## Task 8: Wire Upload Dashboard into CLI

**Files:**
- Modify: `ia-cli/src/commands/upload.rs:333-352`
- Modify: `ia-cli/src/commands/upload.rs:135-137`

Connect the `--dashboard` flag to the new TUI.

**Step 1: Create `run_upload_tui()` entry point**

Add to the bottom of `ia-cli/src/tui/upload_app.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use ia_core::upload::{upload_batch, upload_item, UploadOpts, UploadProgress};
use ia_core::IaClient;

/// Entry point for `ia upload --dashboard`.
pub async fn run_upload_tui(
    client: &IaClient,
    identifiers: Vec<String>,
    files_per_item: Vec<Vec<std::path::PathBuf>>,
    opts: &UploadOpts,
    concurrency: usize,
) -> anyhow::Result<()> {
    let (mut terminal, _guard) = super::framework::setup_terminal()?;

    let state = Arc::new(Mutex::new(UploadTuiState::new(&identifiers)));

    // Spawn upload tasks
    let mut handles = Vec::new();
    for (i, (id, files)) in identifiers.iter().zip(files_per_item.iter()).enumerate() {
        let client = client.clone();
        let id = id.clone();
        let files = files.clone();
        let opts = opts.clone();
        let progress_state = Arc::clone(&state);

        handles.push(tokio::spawn(async move {
            let progress_fn = move |p: UploadProgress| {
                if let Ok(mut s) = progress_state.lock() {
                    s.update(p);
                }
            };
            let progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)> = Some(&progress_fn);
            let result = upload_item(&client, &id, &files, &opts, progress).await;

            // Mark item complete/failed in state
            if let Ok(mut s) = progress_state.lock() {
                // Clean up any remaining active files for this item
                let prefix = format!("{id}\0");
                s.active_files.retain(|k, _| !k.starts_with(&prefix));
            }

            result
        }));
    }

    // Spawn S3 tasks polling loop
    let tasks_state = Arc::clone(&state);
    let tasks_client = client.clone();
    let tasks_ids = identifiers.clone();
    tokio::spawn(async move {
        loop {
            for id in &tasks_ids {
                if let Ok(value) = ia_core::tasks::get_tasks(
                    &tasks_client,
                    &ia_core::tasks::TasksQuery {
                        identifier: Some(id.clone()),
                        ..Default::default()
                    },
                )
                .await
                {
                    if let Ok(mut s) = tasks_state.lock() {
                        s.tasks_queued = value.summary.queued;
                        s.tasks_running = value.summary.running;
                        s.tasks_error = value.summary.error;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // Run dashboard event loop
    let mut dashboard = UploadDashboard {
        state: Arc::clone(&state),
    };
    super::framework::run_dashboard_sync(
        &mut terminal,
        &mut dashboard,
        Duration::from_millis(100),
    )?;

    // Collect results
    let mut all_results = Vec::new();
    let mut had_failure = false;
    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => {
                for r in &results {
                    if matches!(r.status, ia_core::upload::UploadStatus::Failed(_)) {
                        had_failure = true;
                    }
                }
                all_results.extend(results);
            }
            Ok(Err(e)) => {
                had_failure = true;
                eprintln!("Upload error: {e}");
            }
            Err(e) => {
                had_failure = true;
                eprintln!("Task panicked: {e}");
            }
        }
    }

    // Print summary
    let uploaded = all_results
        .iter()
        .filter(|r| matches!(r.status, ia_core::upload::UploadStatus::Uploaded))
        .count();
    let skipped = all_results
        .iter()
        .filter(|r| matches!(r.status, ia_core::upload::UploadStatus::Skipped))
        .count();
    let failed = all_results
        .iter()
        .filter(|r| matches!(r.status, ia_core::upload::UploadStatus::Failed(_)))
        .count();

    eprintln!(
        "\n{} uploaded, {} skipped, {} failed",
        uploaded, skipped, failed,
    );

    if had_failure {
        std::process::exit(1);
    }

    Ok(())
}
```

**Step 2: Update the dashboard bail in `upload.rs`**

Replace lines 343-345 in `ia-cli/src/commands/upload.rs`:

```rust
// Before:
if args.dashboard {
    bail!("upload dashboard is not yet implemented (Phase 3)");
}
```

With:

```rust
#[cfg(feature = "tui")]
if args.dashboard {
    // Dashboard handles its own output — dispatch here before subcommand routing
    // (bare upload and import are the only modes that support dashboard)
    match &args.command {
        None => {
            // Will be handled in run_bare_upload with dashboard flag
        }
        Some(UploadCommand::Import(_)) => {
            // Will be handled in run_import with dashboard flag
        }
        _ => bail!("--dashboard is only supported for bare upload and import"),
    }
}

#[cfg(not(feature = "tui"))]
if args.dashboard {
    bail!("Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui");
}
```

Then in `run_bare_upload()`, add the dashboard dispatch before the existing progress setup (around line 427):

```rust
#[cfg(feature = "tui")]
if args.dashboard {
    return crate::tui::upload_app::run_upload_tui(
        client,
        vec![identifier.to_string()],
        vec![files],
        &opts,
        1, // single item
    )
    .await;
}
```

**Step 3: Update help text**

In the `--dashboard` flag definition (line 135-137), change:

```rust
// Before:
/// Full-screen TUI dashboard (not yet implemented)

// After:
/// Full-screen TUI dashboard for monitoring upload progress
```

**Step 4: Run `cargo check -p ia-cli`**

Expected: compiles.

**Step 5: Run all tests**

Run: `cargo test -p ia-cli -p ia-core`

Expected: ALL tests pass.

**Step 6: Commit**

```
feat(upload): wire --dashboard flag to upload TUI

Remove "not yet implemented" bail, dispatch to run_upload_tui()
for bare upload and import modes. Spawns per-item upload tasks
with progress callbacks, S3 tasks polling loop, and shared
Dashboard event loop.

Ref #218
```

---

## Task 9: Import (Batch) Dashboard Support

**Files:**
- Modify: `ia-cli/src/commands/upload.rs` (the `run_import()` function)

Wire `--dashboard` through the import subcommand path as well.

**Step 1: Add dashboard flag to ImportArgs**

Check if `ImportArgs` already inherits the `--dashboard` flag from the parent `UploadArgs`. If not, it needs to be added. (It's a parent-level flag, so it should already be available via the `UploadArgs` struct.)

Look at how `run_import()` is called (line 348) — it receives `ImportSubArgs`, not the full `UploadArgs`. The `dashboard` flag needs to be threaded through.

**Step 2: Thread dashboard flag into `run_import()`**

Modify the `run()` function to pass `args.dashboard` to `run_import()`:

```rust
Some(UploadCommand::Import(sub)) => run_import(client, sub, quiet, jobs, joblog_path, args.dashboard).await,
```

In `run_import()`, add a dashboard dispatch before the `upload_batch()` call, similar to the bare upload path. Extract identifiers and file lists from the spreadsheet records, then call `run_upload_tui()`.

**Step 3: Run all tests**

Run: `cargo test -p ia-cli -p ia-core`

Expected: ALL tests pass.

**Step 4: Commit**

```
feat(upload): add dashboard support for import (batch) mode

Thread --dashboard flag through to run_import(), dispatch to
run_upload_tui() with identifiers and files extracted from
spreadsheet records.

Ref #218
```

---

## Task 10: Integration Tests

**Files:**
- Create tests in `ia-cli/tests/` or extend existing upload test file

Write integration tests that verify the dashboard can be invoked (though actual TUI rendering can't be tested in CI without a TTY). Focus on:

1. `--dashboard` and `--json` mutual exclusivity error
2. `--dashboard` on template/cleanup subcommands errors
3. Tasks API wiremock tests (already in Task 5)
4. `UploadTuiState` unit tests (already in Task 6)
5. Progress body wrapper tests (already in Task 4)

**Step 1: Write CLI integration tests**

```rust
#[test]
fn dashboard_and_json_mutually_exclusive() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["upload", "--dashboard", "--json", "test-item", "file.txt"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("mutually exclusive"));
}

#[test]
fn dashboard_on_template_errors() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["upload", "--dashboard", "template", "/tmp"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("only supported for"));
}
```

**Step 2: Run all tests**

Run: `cargo test -p ia-cli -p ia-core`

Expected: ALL tests pass.

**Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`

Expected: zero warnings.

**Step 4: Commit**

```
test(upload): add integration tests for dashboard flag handling

Verify --dashboard/--json mutual exclusivity and dashboard-only-on-
supported-subcommands error handling.

Closes #217, Closes #218
```

---

## Task 11: Documentation and Cleanup

**Files:**
- Modify: `docs/plans/2026-03-05-upload-design.md` (mark Phase 3 as implemented)
- Modify: MEMORY.md (update status)

**Step 1: Update MEMORY.md**

Change Phase 3 status from "NOT STARTED" to "COMPLETE" with module/file details.

**Step 2: Update design doc**

Add implementation notes to the Phase 3 section noting what was built.

**Step 3: Final test + clippy pass**

Run: `cargo test -p ia-cli -p ia-core && cargo clippy -p ia-core -p ia-cli -- -D warnings`

Expected: all pass, zero warnings.

**Step 4: Commit**

```
docs: update MEMORY.md and design doc for upload Phase 3 completion

Mark upload dashboard (Phase 3) as complete. Document new modules:
tui/framework.rs, tui/widgets.rs, tui/upload_app.rs, tui/upload_ui.rs,
upload/progress_body.rs, tasks.rs.
```
