# Upload Phase 3 Review Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Address all 13 findings from the PR #223 code review — 1 critical, 7 important, 5 suggestions.

**Architecture:** Targeted fixes across `ia-core` (API types, progress callback, stream body) and `ia-cli` (TUI state, widgets, polling, tests). No new modules — all changes are edits to existing files.

**Tech Stack:** Rust, ratatui, tokio, reqwest, bytes, Arc, VecDeque

---

## Dependency Graph

```
Task 1 (tasks types) ──────────────────────┐
Task 2 (ProgressBody Bytes)                 │
Task 3 (Arc progress callback)              │
Task 4 (format_bytes consolidation) ─┐      │
Task 5 (download widget migration) ──┘      │
Task 6 (VecDeque + scroll clamp) ──┐        │
Task 7 (concurrency semaphore)     │        │
Task 8 (exit→bail!)               │        │
Task 9 (extract shared helper) ────┘────────│
Task 10 (aggregate tasks polling) ──────────┘
Task 11 (missing test)
```

Tasks 1-3 are independent. Task 5 depends on 4. Task 9 depends on 6-8. Task 10 depends on 1.

---

### Task 1: Tasks API — derives, new query fields, success check

**Files:**
- Modify: `ia-core/src/tasks.rs`

**Step 1: Write failing tests for `submitter`, `args`, and `success: false`**

Add these tests to the existing `#[cfg(test)] mod tests` block in `tasks.rs`:

```rust
#[tokio::test]
async fn test_get_tasks_with_submitter_and_args() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("submitter", "user@example.com"))
        .and(query_param("args", "*s3-put*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "value": {
                "summary": {
                    "queued": 5,
                    "running": 2,
                    "error": 1,
                    "paused": 0
                }
            }
        })))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let query = TasksQuery {
        submitter: Some("user@example.com".to_string()),
        args: Some("*s3-put*".to_string()),
        ..Default::default()
    };

    let result = get_tasks(&client, &query).await.unwrap();
    assert_eq!(result.summary.queued, 5);
    assert_eq!(result.summary.running, 2);
    assert_eq!(result.summary.error, 1);
}

#[tokio::test]
async fn test_get_tasks_success_false() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": false,
            "value": {
                "summary": {
                    "queued": 0,
                    "running": 0,
                    "error": 0,
                    "paused": 0
                }
            }
        })))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let query = TasksQuery::default();

    let result = get_tasks(&client, &query).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        IaError::Http { status, message } => {
            assert_eq!(status, 200);
            assert!(message.contains("success: false"));
        }
        other => panic!("expected Http error, got: {other}"),
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core tasks::tests -- --nocapture`
Expected: compilation failures (missing `submitter`, `args` fields)

**Step 3: Implement the changes**

In `tasks.rs`:

1. Add `Clone, Copy` to `TasksSummary` derives (line 30):
```rust
#[derive(Debug, Clone, Copy, Deserialize)]
```

2. Add `Clone` to `TasksResponse`, `TasksValue`, `TaskEntry` derives:
```rust
#[derive(Debug, Clone, Deserialize)]  // TasksResponse (line 15)
#[derive(Debug, Clone, Deserialize)]  // TasksValue (line 22)
#[derive(Debug, Clone, Deserialize)]  // TaskEntry (line 43)
```

3. Add `submitter` and `args` fields to `TasksQuery` (after line 11):
```rust
pub struct TasksQuery {
    pub identifier: Option<String>,
    pub cmd: Option<String>,
    pub submitter: Option<String>,
    pub args: Option<String>,
    pub limit: Option<u32>,
}
```

4. Add query params to the request builder in `get_tasks` (after the `cmd` block, before the `limit` block):
```rust
if let Some(ref submitter) = query.submitter {
    req = req.query(&[("submitter", submitter.as_str())]);
}
if let Some(ref args) = query.args {
    req = req.query(&[("args", args.as_str())]);
}
```

5. Add success check after deserialization (replace lines 91-92):
```rust
let parsed: TasksResponse = serde_json::from_str(&body)?;
if !parsed.success {
    return Err(IaError::Http {
        status: status.as_u16(),
        message: "Tasks API returned success: false".into(),
    });
}
Ok(parsed.value)
```

Note: `status` needs to be captured before the success check. Move the `let status = resp.status();` to remain at line 82, and use `status.as_u16()` in the success check.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core tasks::tests -- --nocapture`
Expected: all tasks tests pass

**Step 5: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "fix(tasks): add submitter/args query fields, success check, and derives

Add submitter and args fields to TasksQuery for aggregate task queries.
Check the success field in the API response — previously ignored, which
could silently return stale data on success: false with HTTP 200.
Add Clone to all Tasks types and Copy to TasksSummary for downstream
ergonomics.

Addresses review items #5, #12, #13 from PR #223."
```

---

### Task 2: ProgressBody — yield `Bytes` instead of `Vec<u8>`

**Files:**
- Modify: `ia-core/src/upload/progress_body.rs`

**Step 1: Update the Stream implementation**

1. Add import at top of file (after line 5):
```rust
use bytes::Bytes;
```

2. Change the `Item` type (line 48):
```rust
type Item = Result<Bytes, io::Error>;
```

3. Change the chunk yield (line 61):
```rust
Poll::Ready(Some(Ok(Bytes::copy_from_slice(&this.buf[..n]))))
```

**Step 2: Run existing tests**

Run: `cargo test -p ia-core upload::progress_body -- --nocapture`
Expected: all 4 tests pass (the test assertions work with `Bytes` since it derefs to `[u8]`)

Note: The tests use `chunk.len()` and `chunk.unwrap()` which work with `Bytes`. The `data_integrity_preserved` test uses `collected.extend_from_slice(&chunk.unwrap())` — `Bytes` derefs to `&[u8]`, so this still works.

**Step 3: Commit**

```
git add ia-core/src/upload/progress_body.rs
git commit -m "perf(upload): yield bytes::Bytes from ProgressBody instead of Vec<u8>

Eliminates an extra allocation per 64 KiB chunk. Previously each chunk
was copied into a Vec<u8>, then converted to Bytes by reqwest. Now we
produce Bytes directly via copy_from_slice. The bytes crate is already
a transitive dependency via reqwest.

Addresses review item #8 from PR #223."
```

---

### Task 3: Progress callback — `&dyn Fn` → `Arc<dyn Fn>`

This is the critical safety fix. Changes the `progress` parameter across the entire upload call chain from a borrowed trait object to an owned `Arc`, eliminating the `unsafe transmute` in `single.rs`.

**Files:**
- Modify: `ia-core/src/upload/single.rs`
- Modify: `ia-core/src/upload/item.rs`
- Modify: `ia-core/src/upload/multipart.rs`
- Modify: `ia-core/src/upload/batch.rs`
- Modify: `ia-core/src/upload/types.rs` (add type alias)
- Modify: `ia-core/src/upload/mod.rs` (re-export)
- Modify: `ia-cli/src/tui/upload_app.rs` (callers)
- Modify: `ia-cli/src/commands/upload.rs` (if it calls upload functions with progress)

**Step 1: Add a type alias in `types.rs`**

Add to `ia-core/src/upload/types.rs` (near the other upload types):
```rust
/// Progress callback type for upload operations.
///
/// Wrapping in `Arc` allows the callback to be shared across async tasks
/// and to satisfy the `'static` bound required by `reqwest::Body::wrap_stream()`
/// without resorting to `unsafe` lifetime transmutes.
pub type ProgressCallback = Arc<dyn Fn(UploadProgress) + Send + Sync>;
```

Add `use std::sync::Arc;` at the top of `types.rs` if not already imported.

**Step 2: Re-export from `mod.rs`**

In `ia-core/src/upload/mod.rs`, add `ProgressCallback` to the `pub use types::` line:
```rust
pub use types::{
    MultipartUploadInfo, PartInfo, ProgressCallback, UploadOpts, UploadOptsBuilder,
    UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};
```

**Step 3: Update `upload_file` in `single.rs`**

Change the signature (line 45):
```rust
progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
```

Add `use std::sync::Arc;` at the top.

Remove the entire `unsafe transmute` block (lines 232-245). Replace the progress body construction (lines 231-267) with:
```rust
let response = if let Some(cb) = progress.clone() {
    // Wrap with ProgressBody for byte-level progress callbacks.
    let id = identifier.to_string();
    let k = key.to_string();
    let fs = file_size;
    let stream = super::progress_body::ProgressBody::new(
        file_handle,
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
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
} else {
    request.body(reqwest::Body::from(file_handle)).send().await
};
```

Also update other places in `single.rs` where `progress` is invoked (the Verifying, Complete, Skipped, Failed callbacks). These use `if let Some(cb) = progress { cb(...) }` — since `progress` is now `Option<Arc<...>>`, change to `if let Some(cb) = &progress { cb(...) }` or `if let Some(ref cb) = progress { cb(...) }`.

Also update the dispatch to multipart (line 48-55). Since `progress` is `Option<Arc<...>>` and `upload_file_multipart` will also take `Option<Arc<...>>`, pass `progress.clone()`.

**Step 4: Update `upload_file_multipart` in `multipart.rs`**

Change the signature (line 462):
```rust
progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
```

Add `use std::sync::Arc;` at the top.

Update all `if let Some(cb) = progress { cb(...) }` patterns to `if let Some(ref cb) = progress { cb(...) }`.

**Step 5: Update `upload_item` in `item.rs`**

Change the signature (line 33):
```rust
progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
```

Add `use std::sync::Arc;` at the top.

Update the call to `upload_file` (line 115-117) — pass `progress.clone()` since it's called in a loop:
```rust
let result = upload_file(
    client, identifier, file, key, &opts, is_first, is_last, hint, progress.clone(),
)
.await?;
```

**Step 6: Update `upload_batch` in `batch.rs`**

Change the signature (line 41):
```rust
progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
```

Add `use std::sync::Arc;` at the top.

Update the call to `upload_item` (line 68-69). Since `progress` is now `Arc`, clone it for each concurrent item:
```rust
let progress = progress.clone();
// inside the async block:
let result =
    upload_item(client, &group.identifier, &group.files, &item_opts, progress).await;
```

**Step 7: Update callers in `upload_app.rs`**

In `run_upload_tui` (line 401-407) and `run_upload_batch_tui` (line 619-626), the closure already creates a `progress_fn` and wraps it as `Option<&dyn Fn(...)>`. Change to wrap in `Arc`:

```rust
let progress_fn = Arc::new(move |p: UploadProgress| {
    if let Ok(mut s) = progress_state.lock() {
        s.update(p);
    }
});
let result = ia_core::upload::upload_item(
    &client, &id, &files, &item_opts, Some(progress_fn),
).await;
```

Remove the `let progress: Option<&(dyn Fn(...) + Send + Sync)> = Some(&progress_fn);` intermediate.

**Step 8: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: all 920+ tests pass, zero `unsafe` blocks in upload code

**Step 9: Commit**

```
git add ia-core/src/upload/types.rs ia-core/src/upload/mod.rs \
       ia-core/src/upload/single.rs ia-core/src/upload/multipart.rs \
       ia-core/src/upload/item.rs ia-core/src/upload/batch.rs \
       ia-cli/src/tui/upload_app.rs
git commit -m "fix(upload): replace unsafe transmute with Arc<dyn Fn> for progress callback

Change the progress callback parameter across the entire upload call
chain (upload_file, upload_item, upload_file_multipart, upload_batch)
from Option<&(dyn Fn(UploadProgress) + Send + Sync)> to
Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>. This eliminates the
unsafe lifetime transmute in single.rs that fabricated a 'static bound
for reqwest::Body::wrap_stream().

The Arc has negligible overhead (one allocation per upload) and is the
idiomatic Rust pattern for shared callbacks across async boundaries.
This is especially important since ia-core is a library consumed by
external projects (ia-gui).

Add ProgressCallback type alias in upload/types.rs for convenience.

Addresses critical review item #1 from PR #223."
```

---

### Task 4: Consolidate `format_bytes` on KiB labels

**Files:**
- Modify: `ia-cli/src/output.rs:476-486`
- Modify: `ia-cli/src/tui/widgets.rs:93-115`
- Modify: `ia-cli/src/tui/ui.rs:430-440`

The canonical copy lives in `output.rs` (not feature-gated). The `widgets.rs` copy re-exports it. The `ui.rs` copy is deleted.

**Step 1: Update `output.rs::format_bytes` to use KiB labels with TiB support**

Replace the function at `output.rs:476-486`:
```rust
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < MIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < GIB {
        format!("{:.1} MiB", b / MIB)
    } else if b < TIB {
        format!("{:.2} GiB", b / GIB)
    } else {
        format!("{:.2} TiB", b / TIB)
    }
}
```

**Step 2: Replace `widgets.rs::format_bytes` with a re-export**

Remove the `format_bytes` function definition (lines 93-115) and the `#[allow(dead_code)]` + `#[must_use]` + doc comment above it. Replace with:
```rust
// Re-export from output.rs — single source of truth for byte formatting.
pub use crate::output::format_bytes;
```

Move any `format_bytes` tests from `widgets.rs` to `output.rs`. If `output.rs` doesn't have a test module, add one.

**Step 3: Delete `format_bytes` from `ui.rs`**

Remove the private `format_bytes` function at `ui.rs:430-440`. All call sites in `ui.rs` now need to use `super::widgets::format_bytes` (or `crate::output::format_bytes`). Add import at top:
```rust
use super::widgets::format_bytes;
```

**Step 4: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass, all byte formatting uses KiB labels

**Step 5: Commit**

```
git add ia-cli/src/output.rs ia-cli/src/tui/widgets.rs ia-cli/src/tui/ui.rs
git commit -m "refactor(tui): consolidate format_bytes to single KiB-based implementation

Standardize on KiB/MiB/GiB/TiB labels (binary prefixes, technically
correct for 1024-based units) across all output — console and TUI.
The canonical implementation lives in output.rs. The copies in
widgets.rs (re-export) and ui.rs (deleted) now use the single source.

Adds TiB support that was missing from the output.rs version.

Addresses review items #3 and #9 (partial) from PR #223."
```

---

### Task 5: Migrate download dashboard to shared widgets

**Files:**
- Modify: `ia-cli/src/tui/ui.rs`

**Step 1: Replace inline ETA formatting with `widgets::format_eta`**

In `ui.rs::draw_header` (lines 78-90), the ETA is formatted inline. Replace with:
```rust
let eta_str = state
    .eta_seconds()
    .map(|s| format!("  ETA {}", super::widgets::format_eta(s as u64)))
    .unwrap_or_default();
```

This replaces the manual hour/minute/second formatting with the shared `format_eta` helper.

Note: Check if `format_eta` takes `u64` or `f64`. The download dashboard's `eta_seconds()` returns `Option<f64>`, so cast with `s as u64`.

**Step 2: Replace inline key hints with `widgets::draw_key_hints`**

In `ui.rs::draw_status_bar` (lines 406-412), replace the manual key hints:
```rust
let keys = Line::from(vec![
    Span::raw(" "),
    Span::styled("[j/k]", Style::default().fg(Color::Cyan)),
    Span::raw(" scroll  "),
    Span::styled("[q]", Style::default().fg(Color::Cyan)),
    Span::raw("uit"),
]);
```

With a call to the shared widget. Since `draw_key_hints` renders to a `Rect`, this may need slight adaptation. Check the signature of `draw_key_hints` in `widgets.rs` — if it takes `(f: &mut Frame, area: Rect, hints: &[(&str, &str)])`, then call:
```rust
super::widgets::draw_key_hints(f, layout[1], &[("j/k", "scroll"), ("q", "uit")]);
```

And remove the manual `Paragraph::new(keys)` rendering of that line.

**Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass

**Step 4: Commit**

```
git add ia-cli/src/tui/ui.rs
git commit -m "refactor(tui): migrate download dashboard to shared widgets

Replace inline ETA formatting and key hints rendering in the download
dashboard with shared helpers from widgets.rs (format_eta, draw_key_hints).
Both dashboards now use identical formatting logic.

Addresses review item #9 from PR #223."
```

---

### Task 6: Bounded `completed_files` and clamped `scroll_offset`

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`
- Modify: `ia-cli/src/tui/app.rs`

**Step 1: Write tests for the new bounds**

In `upload_app.rs` tests, add:
```rust
#[test]
fn completed_files_bounded() {
    let mut state = UploadTuiState::new(&["item-1".into()]);
    // Add 20 files — should only keep the last 10
    for i in 0..20 {
        state.update(progress("item-1", &format!("file-{i}.txt"), UploadProgressStatus::Verifying, 0, 100));
        state.update(progress("item-1", &format!("file-{i}.txt"), UploadProgressStatus::Complete, 100, 100));
    }
    assert!(state.completed_files.len() <= 10);
    // Most recent should be last
    assert_eq!(state.completed_files.back().unwrap(), "file-19.txt");
}

#[test]
fn scroll_offset_clamped() {
    let mut state = UploadTuiState::new(&["item-1".into()]);
    // Add 2 active files
    state.update(progress("item-1", "a.txt", UploadProgressStatus::Verifying, 0, 100));
    state.update(progress("item-1", "b.txt", UploadProgressStatus::Verifying, 0, 100));
    // Scroll way past the end
    state.scroll_offset = 100;
    state.clamp_scroll();
    assert!(state.scroll_offset <= 1); // max is active_files.len() - 1
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli upload_app::tests -- --nocapture`
Expected: compilation failures

**Step 3: Implement in `upload_app.rs`**

1. Add `use std::collections::VecDeque;` at top.

2. Change `completed_files` field type (line 103):
```rust
pub completed_files: VecDeque<String>,
```

3. In `UploadTuiState::new` (line 149), change initialization:
```rust
completed_files: VecDeque::new(),
```

4. In `UploadTuiState::update`, where `completed_files.push(p.key)` (line 286), change to:
```rust
self.completed_files.push_back(p.key);
if self.completed_files.len() > 10 {
    self.completed_files.pop_front();
}
```

5. Add a `clamp_scroll` method to `UploadTuiState`:
```rust
/// Clamp scroll_offset so it doesn't scroll past the active file list.
pub fn clamp_scroll(&mut self) {
    let max = self.active_files.len().saturating_sub(1);
    self.scroll_offset = self.scroll_offset.min(max);
}
```

6. In `UploadDashboard::handle_key` (lines 343-348), after incrementing scroll, call clamp:
```rust
KeyCode::Char('j') | KeyCode::Down => {
    s.scroll_offset = s.scroll_offset.saturating_add(1);
    s.clamp_scroll();
    true
}
```

**Step 4: Implement the same changes in `app.rs` (download dashboard)**

1. Add `use std::collections::VecDeque;` at top.

2. Change `completed_files: Vec<String>` to `VecDeque<String>` (line 65).

3. In `TuiState::new` or wherever initialized, use `VecDeque::new()`.

4. Wherever `completed_files.push(...)` is called, use `push_back(...)` + cap at 10.

5. Add `clamp_scroll` and use it in `DownloadDashboard::handle_key`.

6. Check rendering code — if it calls `.iter()` or `.rev().take(3)`, `VecDeque` supports both so no changes needed there.

**Step 5: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass

**Step 6: Commit**

```
git add ia-cli/src/tui/upload_app.rs ia-cli/src/tui/app.rs
git commit -m "fix(tui): bound completed_files with VecDeque and clamp scroll_offset

Cap completed_files at 10 entries using VecDeque (only the last 3 are
ever displayed). Prevents unbounded memory growth during large batch
uploads with thousands of files.

Clamp scroll_offset to active_files.len() - 1 in both upload and
download dashboards, preventing empty display when scrolling past the
active file list.

Addresses review items #6 and #7 from PR #223."
```

---

### Task 7: Add concurrency semaphore to `run_upload_tui`

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`

**Step 1: Replace `_concurrency` with a semaphore**

In `run_upload_tui` (line 381), rename the parameter:
```rust
concurrency: usize,
```

Add semaphore creation after terminal setup (after line 388):
```rust
let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
```

In the spawn loop (lines 400-454), add permit acquisition at the start of each async block:
```rust
let sem = Arc::clone(&semaphore);
handles.push(tokio::spawn(async move {
    let _permit = sem.acquire().await.expect("semaphore closed");
    // ... existing progress_fn and upload_item call ...
```

**Step 2: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass

**Step 3: Commit**

```
git add ia-cli/src/tui/upload_app.rs
git commit -m "fix(tui): add concurrency semaphore to run_upload_tui

The concurrency parameter was accepted but ignored, meaning all items
would be uploaded concurrently without limit. Now uses a tokio Semaphore
matching the pattern in run_upload_batch_tui.

Addresses review item #2 from PR #223."
```

---

### Task 8: Replace `std::process::exit(1)` with `bail!`

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`
- Modify: `ia-cli/src/tui/app.rs`

**Step 1: In `upload_app.rs`**

Replace `std::process::exit(1)` at lines 558-559 and 771-772 with:
```rust
if total_failed > 0 {
    anyhow::bail!("{total_failed} file(s) failed to upload");
}
```

**Step 2: In `app.rs`**

Find the equivalent `std::process::exit(1)` (around line 927) and replace with:
```rust
if total_failed > 0 {
    anyhow::bail!("{total_failed} file(s) failed to download");
}
```

**Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass

**Step 4: Commit**

```
git add ia-cli/src/tui/upload_app.rs ia-cli/src/tui/app.rs
git commit -m "fix(tui): replace std::process::exit(1) with anyhow::bail!

process::exit bypasses Drop destructors and prevents cleanup. Replace
with bail! which propagates through the normal error handling chain.
The caller (commands/upload.rs, commands/download.rs) already converts
errors to exit code 1 via main.rs.

Addresses review item #11 from PR #223."
```

---

### Task 9: Extract shared helper from upload TUI entry points

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`

This task extracts the duplicated tail of `run_upload_tui` and `run_upload_batch_tui` into a shared helper.

**Step 1: Identify the shared code**

The duplicated sections are:
1. Tasks polling loop (lines 457-492 / 672-706)
2. Dashboard event loop spawn (lines 496-504 / 711-717)
3. Cleanup (lines 506-508 / 719-721)
4. Result collection + summary (lines 510-563 / 723-776)

**Step 2: Extract the shared helper**

Add a private function before the two entry points:

```rust
/// Shared dashboard lifecycle: poll S3 tasks, run the event loop, collect
/// results, and print a summary. Used by both `run_upload_tui` and
/// `run_upload_batch_tui`.
async fn run_dashboard_and_summarize(
    client: &ia_core::IaClient,
    state: Arc<Mutex<UploadTuiState>>,
    mut terminal: super::framework::Term,
    _guard: super::framework::TerminalGuard,
    handles: Vec<tokio::task::JoinHandle<std::result::Result<Vec<ia_core::upload::UploadResult>, ia_core::error::IaError>>>,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Spawn S3 tasks polling loop (every 60s, single aggregate query).
    let tasks_state = Arc::clone(&state);
    let tasks_client = client.clone();
    // Try to get submitter email from config (no network call).
    let submitter = client.config().cookies.get("logged-in-user").cloned();
    let tasks_handle = tokio::spawn(async move {
        loop {
            if let Ok(value) = ia_core::tasks::get_tasks(
                &tasks_client,
                &ia_core::tasks::TasksQuery {
                    args: Some("*s3-put*".to_string()),
                    submitter: submitter.clone(),
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

            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // 2. Run the dashboard event loop on a blocking thread.
    let dashboard_state = Arc::clone(&state);
    let tick_rate = Duration::from_millis(100);
    tokio::task::spawn_blocking(move || {
        let mut dashboard = UploadDashboard {
            state: dashboard_state,
        };
        super::framework::run_dashboard_sync(&mut terminal, &mut dashboard, tick_rate)
    })
    .await??;

    // 3. Clean up
    tasks_handle.abort();
    drop(_guard);

    // 4. Collect results and print summary
    let mut total_uploaded = 0usize;
    let mut total_skipped = 0usize;
    let mut total_failed = 0usize;
    let mut total_bytes = 0u64;

    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => {
                for r in &results {
                    match &r.status {
                        ia_core::upload::UploadStatus::Uploaded => {
                            total_uploaded += 1;
                            total_bytes += r.bytes;
                        }
                        ia_core::upload::UploadStatus::Skipped => {
                            total_skipped += 1;
                        }
                        ia_core::upload::UploadStatus::Failed(_) => {
                            total_failed += 1;
                        }
                        ia_core::upload::UploadStatus::DryRun => {
                            total_skipped += 1;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("Error: {e}");
                total_failed += 1;
            }
            Err(e) => {
                eprintln!("Task panicked: {e}");
                total_failed += 1;
            }
        }
    }

    let elapsed = state.lock().unwrap().throughput.elapsed();
    eprintln!(
        "Uploaded {} files, {} skipped, {} failed ({}) in {:.1}s",
        total_uploaded,
        total_skipped,
        total_failed,
        crate::output::format_bytes(total_bytes),
        elapsed.as_secs_f64(),
    );

    if total_failed > 0 {
        anyhow::bail!("{total_failed} file(s) failed to upload");
    }

    Ok(())
}
```

**Step 3: Simplify `run_upload_tui`**

Replace everything from the tasks polling loop to the end (lines 457-562) with:
```rust
run_dashboard_and_summarize(client, state, terminal, _guard, handles).await
```

**Step 4: Simplify `run_upload_batch_tui`**

Replace everything from the tasks polling loop to the end (lines 672-775) with:
```rust
run_dashboard_and_summarize(client, state, terminal, _guard, handles).await
```

**Step 5: Run tests**

Run: `cargo test -p ia-cli`
Expected: all tests pass

**Step 6: Commit**

```
git add ia-cli/src/tui/upload_app.rs
git commit -m "refactor(tui): extract run_dashboard_and_summarize shared helper

Deduplicate ~150 lines of identical logic between run_upload_tui and
run_upload_batch_tui: S3 tasks polling, dashboard event loop, cleanup,
result collection, and summary printing.

This also integrates the aggregate tasks query (single API call with
args=*s3-put* instead of per-identifier polling) and the bail! fix
from previous commits.

Addresses review items #4 and #12 from PR #223."
```

---

### Task 10: Aggregate S3 tasks polling

**Note:** This is already integrated into Task 9's `run_dashboard_and_summarize` helper. The per-identifier polling loop is replaced by a single call with `args=*s3-put*&submitter={email}`.

If Task 9 is not yet implemented when this task runs, make the change directly in both `run_upload_tui` and `run_upload_batch_tui`:

Replace the per-identifier loop (lines 468-482 / 682-696):
```rust
for id in &tasks_ids {
    if let Ok(value) = ia_core::tasks::get_tasks(...) { ... }
}
```

With a single aggregate call:
```rust
if let Ok(value) = ia_core::tasks::get_tasks(
    &tasks_client,
    &ia_core::tasks::TasksQuery {
        args: Some("*s3-put*".to_string()),
        submitter: submitter.clone(),
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
```

Where `submitter` is resolved once before the loop:
```rust
let submitter = client.config().cookies.get("logged-in-user").cloned();
```

If this is folded into Task 9, no separate commit is needed.

---

### Task 11: Add missing integration test for `--dashboard import`

**Files:**
- Modify: `ia-cli/tests/upload.rs`

**Step 1: Add the positive-path test**

Add in the dashboard test section (after line 249):
```rust
#[test]
fn upload_dashboard_accepted_on_import() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "upload",
            "--dashboard",
            "import",
            "/tmp/nonexistent-test-file.csv",
        ])
        .assert()
        .failure()
        // Should fail for a reason OTHER than dashboard restriction —
        // verifies import is recognized as a valid dashboard target.
        .stderr(predicate::str::contains("only supported for").not());
}
```

**Step 2: Run the test**

Run: `cargo test -p ia-cli --test upload upload_dashboard_accepted_on_import -- --nocapture`
Expected: PASS (the test fails because the file doesn't exist, but NOT because of dashboard restriction)

**Step 3: Commit**

```
git add ia-cli/tests/upload.rs
git commit -m "test(upload): add positive-path test for --dashboard on import

Verify that --dashboard is accepted (not rejected) on the import
subcommand. The test asserts the error is NOT the 'only supported for'
restriction — it should fail for another reason (nonexistent file).

Addresses review item #10 from PR #223."
```

---

## Execution Order

Tasks 1, 2, 3 can be parallelized. Task 5 depends on 4. Task 9 depends on 6+7+8. Task 10 is folded into 9. Task 11 is independent.

Recommended serial order:
1. Task 1 (tasks types)
2. Task 2 (ProgressBody Bytes)
3. Task 3 (Arc progress callback) — critical fix
4. Task 4 (format_bytes)
5. Task 5 (download widget migration)
6. Task 6 (VecDeque + scroll clamp)
7. Task 7 (concurrency semaphore)
8. Task 8 (exit→bail!)
9. Task 9 (extract helper + aggregate polling — combines #4, #12)
10. Task 11 (missing test)

Final verification: `cargo test -p ia-core -p ia-cli && cargo clippy -p ia-core -p ia-cli -- -D warnings`
