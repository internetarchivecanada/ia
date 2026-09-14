# Progress Display Consolidation Implementation Plan

**Goal:** Unify upload and download progress output using shared style constants and helpers so they look and feel identical, preventing future drift.

**Architecture:** Add shared style constants/helpers to `output.rs`, add `Enumerated` variant to `UploadProgressStatus` in ia-core, then refactor all display structs to use shared helpers. Upload display gets rewritten to match download's aggregate bar pattern. BatchDisplay simplified to drop per-file bars.

**Tech Stack:** indicatif 0.17, console 0.15 (existing deps — no new crates)

---

## Task 1: Add `Enumerated` to `UploadProgressStatus`

**Files:**
- Modify: `ia-core/src/upload/types.rs:276-284`
- Modify: `ia-core/src/upload/item.rs:105-122`
- Test: `ia-core/src/upload/types.rs` (existing tests)

- [ ] **Step 1: Add `Enumerated` variant to `UploadProgressStatus`**

In `ia-core/src/upload/types.rs`, add a new variant to the enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadProgressStatus {
    /// Item files enumerated; reports total file count and bytes.
    Enumerated {
        files_count: usize,
        bytes_total: u64,
    },
    Verifying,
    Uploading,
    WaitingRateLimit,
    Complete,
    Skipped,
    Failed,
}
```

- [ ] **Step 2: Emit `Enumerated` from `upload_item`**

In `ia-core/src/upload/item.rs`, after step 8 (size hint computation, ~line 103) and before step 9 (upload loop), emit the enumerated event:

```rust
    // 8b. Emit Enumerated progress event
    if let Some(ref cb) = progress {
        let bytes_total = size_hint.unwrap_or_else(|| {
            expanded.iter()
                .filter_map(|f| std::fs::metadata(f).ok())
                .map(|m| m.len())
                .sum()
        });
        cb(UploadProgress {
            identifier: identifier.to_string(),
            key: String::new(),
            bytes_sent: 0,
            total_bytes: bytes_total,
            status: UploadProgressStatus::Enumerated {
                files_count: file_count,
                bytes_total,
            },
        });
    }
```

Note: when `no_size_hint` is false, `size_hint` already has the total. When `no_size_hint` is true, we compute it here specifically for the progress display (the hint header is separate from the progress event).

- [ ] **Step 3: Run tests to verify nothing breaks**

Run: `cargo test -p ia-core -- upload`
Expected: All existing upload tests pass (new variant doesn't break existing match arms since upload tests don't exhaustively match `UploadProgressStatus`).

- [ ] **Step 4: Fix any exhaustive match warnings**

Search for `match` on `UploadProgressStatus` in both `ia-core` and `ia-cli`. The existing `UploadDisplay::update()` in `output.rs` and the inline callbacks in `commands/upload.rs` match on specific variants — add `UploadProgressStatus::Enumerated { .. } => {}` arms to each.

Files to check:
- `ia-cli/src/output.rs:420-459` (`UploadDisplay::update`)
- `ia-cli/src/commands/upload.rs:607-643` (inline progress callback in `run_import`)
- `ia-cli/src/tui/upload_app.rs` (TUI upload state machine)

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings.

- [ ] **Step 7: Commit**

```bash
git add ia-core/src/upload/types.rs ia-core/src/upload/item.rs ia-cli/src/output.rs ia-cli/src/commands/upload.rs ia-cli/src/tui/upload_app.rs
git commit -m "feat(upload): add Enumerated variant to UploadProgressStatus

Emit total file count and bytes from upload_item after file expansion,
matching download's Enumerated event pattern. This enables aggregate
progress bars for upload."
```

---

## Task 2: Add shared style constants and helpers to `output.rs`

**Files:**
- Modify: `ia-cli/src/output.rs:1-10` (add constants and helpers at the top)

- [ ] **Step 1: Add shared constants**

Add at the top of `output.rs`, after imports:

```rust
// ─── Shared progress style ──────────────────────────────────────────────────

/// Progress bar characters: filled, head, empty.
const PROGRESS_CHARS: &str = "━╸─";

/// Progress bar width in terminal columns.
const BAR_WIDTH: usize = 40;

/// Icons used across all progress displays.
pub(crate) const ICON_HEADER: &str = "▸";
pub(crate) const ICON_SUCCESS: &str = "✓";
pub(crate) const ICON_ERROR: &str = "✗";
pub(crate) const ICON_SKIPPED: &str = "–";
pub(crate) const ICON_DRY_RUN: &str = "⊘";
```

- [ ] **Step 2: Add `make_progress_bar` helper**

```rust
/// Create a progress bar with the shared style.
///
/// Template: `  ━━━━━╸──── 42.3 MiB/100.0 MiB 5.2 MiB/s  (3/15 files)`
fn make_progress_bar(total_bytes: u64) -> ProgressBar {
    let bar = ProgressBar::new(total_bytes);
    bar.set_style(
        ProgressStyle::with_template(&format!(
            "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes}}/{{total_bytes}} {{bytes_per_sec:.dim}}  ({{msg}})"
        ))
        .unwrap()
        .progress_chars(PROGRESS_CHARS),
    );
    bar.set_message("starting...");
    bar
}
```

- [ ] **Step 3: Add `print_item_header` helper**

```rust
/// Print `▸ identifier` header line to stderr.
fn print_item_header(identifier: &str) {
    eprintln!(
        "{} {}",
        style(ICON_HEADER).cyan(),
        style(identifier).bold(),
    );
}
```

- [ ] **Step 4: Add `format_speed` helper**

```rust
/// Format transfer speed as `· X.X MiB/s`, or empty string if elapsed is zero.
fn format_speed(bytes: u64, elapsed_secs: f64) -> String {
    if elapsed_secs > 0.0 {
        format!(
            " · {}/s",
            format_bytes((bytes as f64 / elapsed_secs) as u64)
        )
    } else {
        String::new()
    }
}
```

- [ ] **Step 5: Add `colored_count` helper**

```rust
/// Format a count: colored if non-zero, plain "0" otherwise.
fn colored_count(count: usize, color_fn: fn(console::StyledObject<String>) -> console::StyledObject<String>) -> String {
    if count > 0 {
        color_fn(style(count.to_string())).to_string()
    } else {
        "0".to_string()
    }
}
```

- [ ] **Step 6: Add `print_item_finish` helper**

```rust
/// Print the standard item completion block to stderr.
///
/// ```text
/// identifier  15 files (100.0 MiB) in 19.2s · 5.2 MiB/s
///   ✓ 15 uploaded · 0 skipped · 0 errors
/// ```
fn print_item_finish(
    identifier: &str,
    verb: &str,
    files_done: usize,
    files_skipped: usize,
    files_failed: usize,
    bytes_total: u64,
    elapsed_secs: f64,
) {
    let speed = format_speed(bytes_total, elapsed_secs);
    eprintln!(
        "{}  {} files ({}) in {:.1}s{}",
        style(identifier).bold(),
        files_done,
        format_bytes(bytes_total),
        elapsed_secs,
        style(&speed).dim(),
    );
    eprintln!(
        "  {} {} {} · {} skipped · {} errors",
        style(ICON_SUCCESS).green(),
        files_done,
        verb,
        colored_count(files_skipped, |s| s.yellow()),
        colored_count(files_failed, |s| s.red()),
    );
}
```

- [ ] **Step 7: Add `BatchSummary` and `print_batch_summary`**

```rust
/// Aggregate stats for a batch operation's summary block.
pub struct BatchSummary {
    pub items_total: usize,
    pub items_succeeded: usize,
    pub items_failed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_total: u64,
    pub elapsed_secs: f64,
}

/// Print the batch summary footer to stderr.
///
/// ```text
/// ────────────────────────────────────────────────────
/// 3/3 items (3 done · 0 skipped · 0 errors)
/// 150.0 MiB uploaded · 10.0 MiB/s
/// 13.0s elapsed
/// ```
pub fn print_batch_summary(
    summary: &BatchSummary,
    verb: &str,
    disk_statuses: Option<&[ia_core::disk_pool::DiskStatus]>,
) {
    let speed = format_speed(summary.bytes_total, summary.elapsed_secs);

    eprintln!(
        "{}",
        style("────────────────────────────────────────────────────").dim()
    );
    eprintln!(
        "{}/{} items ({} done · {} skipped · {} errors)",
        summary.items_succeeded,
        summary.items_total,
        style(summary.items_succeeded).green(),
        colored_count(summary.files_skipped, |s| s.yellow()),
        colored_count(summary.files_failed + summary.items_failed, |s| s.red()),
    );
    eprintln!(
        "{} {}{}",
        format_bytes(summary.bytes_total),
        verb,
        style(&speed).dim(),
    );

    if let Some(statuses) = disk_statuses {
        let disk_line: Vec<String> = statuses
            .iter()
            .map(|ds| {
                format!(
                    "{}: {} free",
                    style(ds.path.display()).dim(),
                    format_bytes(ds.free_bytes)
                )
            })
            .collect();
        eprintln!("{}", disk_line.join(" │ "));
    }

    eprintln!("{:.1}s elapsed", summary.elapsed_secs);
}
```

- [ ] **Step 8: Run clippy**

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: Zero warnings (new code is unused so far — that's fine, clippy won't warn on private functions).

- [ ] **Step 9: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "refactor(output): add shared progress style constants and helpers

Shared bar style, icons, and formatting functions that all display
structs will use. Prevents visual drift between upload/download output."
```

---

## Task 3: Refactor `DownloadDisplay` to use shared helpers

**Files:**
- Modify: `ia-cli/src/output.rs` (`DownloadDisplay` impl, ~lines 24-155)

- [ ] **Step 1: Refactor `DownloadDisplay::new` to use shared helpers**

Replace the inline header print and bar creation:

```rust
impl DownloadDisplay {
    pub fn new(identifier: &str) -> Self {
        print_item_header(identifier);
        let bar = make_progress_bar(0);

        Self {
            identifier: identifier.to_string(),
            bar,
            per_file_bytes: Mutex::new(HashMap::new()),
            files_done: Mutex::new(0),
            files_total: Mutex::new(0),
            errors: Mutex::new(Vec::new()),
        }
    }
```

- [ ] **Step 2: Refactor `DownloadDisplay::finish` to use shared helpers**

Replace the inline summary formatting:

```rust
    pub fn finish(&self, result: &ItemDownloadResult, destdir: &Path) {
        self.bar.finish_and_clear();

        // Print collected errors
        let errors = self.errors.lock().unwrap();
        for err in errors.iter() {
            eprintln!("{err}");
        }

        let elapsed = result.elapsed.as_secs_f64();
        print_item_finish(
            &self.identifier,
            "downloaded",
            result.files_downloaded,
            result.files_skipped,
            result.files_failed,
            result.bytes_total,
            elapsed,
        );

        if let Some(free) = disk_space_free(destdir) {
            eprintln!(
                "  {}: {} free",
                style(destdir.display()).dim(),
                format_bytes(free)
            );
        }
    }
```

`DownloadDisplay::update` stays unchanged — it's behavioral, not visual.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "refactor(output): DownloadDisplay uses shared style helpers

No behavioral change — same output, now using shared constants
and formatting functions."
```

---

## Task 4: Rewrite `UploadDisplay` to match `DownloadDisplay`

**Files:**
- Modify: `ia-cli/src/output.rs` (`UploadDisplay` struct + impl, ~lines 392-461)
- Modify: `ia-cli/src/commands/upload.rs` (wire up new display in `run_bare_upload`)

- [ ] **Step 1: Rewrite `UploadDisplay` struct**

Replace the current `UploadDisplay` with an aggregate-bar design matching `DownloadDisplay`:

```rust
/// Single-item upload progress: one aggregate bar, no per-file bars.
///
/// Matches `DownloadDisplay` pattern: header on creation, aggregate byte
/// bar during transfer, summary block on finish.
pub struct UploadDisplay {
    identifier: String,
    bar: ProgressBar,
    per_file_bytes: Mutex<HashMap<String, u64>>,
    files_done: Mutex<usize>,
    files_total: Mutex<usize>,
    bytes_total: Mutex<u64>,
    errors: Mutex<Vec<String>>,
    started_at: std::time::Instant,
}

impl UploadDisplay {
    pub fn new(identifier: &str) -> Self {
        print_item_header(identifier);
        let bar = make_progress_bar(0);

        Self {
            identifier: identifier.to_string(),
            bar,
            per_file_bytes: Mutex::new(HashMap::new()),
            files_done: Mutex::new(0),
            files_total: Mutex::new(0),
            bytes_total: Mutex::new(0),
            errors: Mutex::new(Vec::new()),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn update(&self, p: UploadProgress) {
        match p.status {
            UploadProgressStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                self.bar.set_length(bytes_total);
                *self.files_total.lock().unwrap() = files_count;
                *self.bytes_total.lock().unwrap() = bytes_total;
                self.bar.set_message(format!("0/{files_count} files"));
            }
            UploadProgressStatus::Uploading => {
                let mut map = self.per_file_bytes.lock().unwrap();
                map.insert(p.key.clone(), p.bytes_sent);
                let total: u64 = map.values().sum();
                self.bar.set_position(total);
            }
            UploadProgressStatus::Verifying => {
                // Verifying doesn't change byte count — leave bar as-is
            }
            UploadProgressStatus::Complete => {
                // Snap this file's bytes to its total
                let mut map = self.per_file_bytes.lock().unwrap();
                map.insert(p.key.clone(), p.total_bytes);
                let total: u64 = map.values().sum();
                self.bar.set_position(total);
                drop(map);

                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{files_total} files"));
            }
            UploadProgressStatus::Skipped => {
                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{files_total} files"));
            }
            UploadProgressStatus::Failed => {
                self.errors.lock().unwrap().push(format!(
                    "  {} {} {}",
                    style(ICON_ERROR).red(),
                    style(&p.key).dim(),
                    style("— upload failed").red(),
                ));
                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{files_total} files"));
            }
            UploadProgressStatus::WaitingRateLimit => {
                self.bar.set_message("rate limited, waiting...");
            }
        }
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();

        // Print collected errors
        let errors = self.errors.lock().unwrap();
        for err in errors.iter() {
            eprintln!("{err}");
        }

        let elapsed = self.started_at.elapsed().as_secs_f64();
        let files_done = *self.files_done.lock().unwrap();
        let files_total = *self.files_total.lock().unwrap();
        let bytes_total = *self.bytes_total.lock().unwrap();
        // files_done includes skipped+failed; separate them from results
        let files_failed = errors.len();
        let files_uploaded = files_done.saturating_sub(files_failed);
        // For skipped count, we'd need to track separately — for now, infer
        // from total vs uploaded vs failed. This is approximate; the CLI's
        // output_results has the exact counts and can override.
        let files_skipped = files_total.saturating_sub(files_done);

        print_item_finish(
            &self.identifier,
            "uploaded",
            files_uploaded,
            files_skipped,
            files_failed,
            bytes_total,
            elapsed,
        );
    }
}
```

- [ ] **Step 2: Update `run_bare_upload` to use new `UploadDisplay`**

In `ia-cli/src/commands/upload.rs`, update `run_bare_upload` (~line 456-468):

```rust
    // Set up progress display
    let display = if !args.json && quiet == 0 {
        Some(std::sync::Arc::new(crate::output::UploadDisplay::new(identifier)))
    } else {
        None
    };

    let progress_ref: Option<std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>> =
        display.clone().map(|d| -> std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync> {
            std::sync::Arc::new(move |p: UploadProgress| {
                d.update(p);
            })
        });
```

After `upload_item` returns, call `finish()` and remove the per-file `output_results` call for quiet==0:

```rust
    let results = upload_item(client, identifier, &files, &opts, progress_ref)
        .await
        .context(format!("failed to upload to {identifier}"))?;

    // Finish display (prints summary)
    if let Some(d) = &display {
        d.finish();
    }

    // Output results — only for JSON mode or quiet mode (display already printed summary)
    let had_failure = if args.json {
        output_results(&results, true, quiet, joblog.as_ref())?
    } else {
        // Write to joblog if applicable, check for failures
        let mut had_failure = false;
        for r in &results {
            if matches!(r.status, UploadStatus::Failed(_)) {
                had_failure = true;
            }
            if let Some(jl) = &joblog {
                write_upload_result(jl, r);
            }
        }
        had_failure
    };
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: Zero warnings.

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/output.rs ia-cli/src/commands/upload.rs
git commit -m "feat(upload): rewrite UploadDisplay with aggregate bar matching download

Single aggregate progress bar instead of per-file bars. Shows item
header, byte progress, file counter, and summary block — identical
visual pattern to DownloadDisplay."
```

---

## Task 5: Simplify `BatchDisplay` (download) — drop per-file bars

**Files:**
- Modify: `ia-cli/src/output.rs` (`BatchDisplay` + `ItemBars`, ~lines 157-390)

- [ ] **Step 1: Simplify `ItemBars` struct**

Replace the per-file tracking with an aggregate bar:

```rust
struct ItemBars {
    header: ProgressBar,
    bar: ProgressBar,
    per_file_bytes: HashMap<String, u64>,
    files_done: usize,
    files_total: usize,
}
```

- [ ] **Step 2: Rewrite `on_item_start`**

Create header + aggregate bar instead of per-file bars:

```rust
    pub fn on_item_start(&self, identifier: &str, _current: usize, _total: usize) {
        let item_header = self.multi.insert_before(
            &self.bottom_sentinel,
            ProgressBar::new_spinner(),
        );
        item_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        item_header.set_message(format!(
            "{} {}",
            style(ICON_HEADER).cyan(),
            style(identifier).bold(),
        ));

        let bar = self.multi.insert_before(
            &self.bottom_sentinel,
            ProgressBar::new(0),
        );
        bar.set_style(
            ProgressStyle::with_template(&format!(
                "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes}}/{{total_bytes}} {{bytes_per_sec:.dim}}  ({{msg}})"
            ))
            .unwrap()
            .progress_chars(PROGRESS_CHARS),
        );
        bar.set_message("starting...");

        let mut items = self.active_item_bars.lock().unwrap();
        items.insert(
            identifier.to_string(),
            ItemBars {
                header: item_header,
                bar,
                per_file_bytes: HashMap::new(),
                files_done: 0,
                files_total: 0,
            },
        );
    }
```

- [ ] **Step 3: Rewrite `on_progress` — aggregate only, no per-file bars**

```rust
    pub fn on_progress(&self, progress: DownloadProgress) {
        let identifier = &progress.identifier;
        let mut items = self.active_item_bars.lock().unwrap();
        let Some(item) = items.get_mut(identifier) else {
            return;
        };

        match &progress.status {
            DownloadStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                item.bar.set_length(*bytes_total);
                item.files_total = *files_count;
                item.bar.set_message(format!("0/{files_count} files"));
            }
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                item.per_file_bytes
                    .insert(progress.file_name.clone(), progress.bytes_downloaded);
                let total: u64 = item.per_file_bytes.values().sum();
                item.bar.set_position(total);
            }
            DownloadStatus::Complete => {
                if let Some(size) = progress.total_bytes {
                    item.per_file_bytes.insert(progress.file_name.clone(), size);
                    let total: u64 = item.per_file_bytes.values().sum();
                    item.bar.set_position(total);
                }
                item.files_done += 1;
                item.bar
                    .set_message(format!("{}/{} files", item.files_done, item.files_total));
            }
            DownloadStatus::Skipped(_) => {
                item.files_done += 1;
                item.bar
                    .set_message(format!("{}/{} files", item.files_done, item.files_total));
            }
            DownloadStatus::Failed(_) => {
                item.files_done += 1;
                item.bar
                    .set_message(format!("{}/{} files", item.files_done, item.files_total));
            }
            DownloadStatus::Verifying => {}
        }
    }
```

- [ ] **Step 4: Update `on_item_complete` to use shared formatting**

```rust
    pub fn on_item_complete(&self, result: &ia_core::download::ItemDownloadResult) {
        let identifier = &result.identifier;
        let mut items = self.active_item_bars.lock().unwrap();
        if let Some(item) = items.remove(identifier) {
            item.bar.finish_and_clear();

            let elapsed = result.elapsed.as_secs_f64();
            let speed = format_speed(result.bytes_total, elapsed);

            let skipped_info = if result.files_skipped > 0 {
                format!(
                    "\n  {} {} skipped",
                    style("─").dim(),
                    style(result.files_skipped).yellow()
                )
            } else {
                String::new()
            };

            item.header.set_message(format!(
                "{} {}       {} files ({}) {:.0}s{}{}",
                style(ICON_SUCCESS).green(),
                style(identifier).bold(),
                result.files_downloaded,
                format_bytes(result.bytes_total),
                elapsed,
                style(&speed).dim(),
                skipped_info,
            ));
            item.header.finish();
        }
    }
```

- [ ] **Step 5: Update `finish` to use `print_batch_summary`**

```rust
    pub fn finish(
        &self,
        result: &ia_core::download::BatchDownloadResult,
        disk_statuses: Option<&[ia_core::disk_pool::DiskStatus]>,
    ) {
        self.batch_header.finish_and_clear();
        self.bottom_sentinel.finish_and_clear();

        let summary = BatchSummary {
            items_total: result.items_total,
            items_succeeded: result.items_succeeded,
            items_failed: result.items_failed,
            files_skipped: result.files_skipped,
            files_failed: result.files_failed,
            bytes_total: result.bytes_total,
            elapsed_secs: result.elapsed.as_secs_f64(),
        };
        print_batch_summary(&summary, "downloaded", disk_statuses);
    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ia-cli`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "refactor(output): simplify BatchDisplay to aggregate bars

Drop per-file progress bars — one aggregate bar per active item.
Per-file detail is available via --dashboard. Uses shared style
constants and formatting helpers."
```

---

## Task 6: Add `UploadBatchDisplay`

**Files:**
- Modify: `ia-cli/src/output.rs` (add new struct after `BatchDisplay`)
- Modify: `ia-cli/src/commands/upload.rs` (`run_import` to use new display)

- [ ] **Step 1: Add `UploadBatchDisplay` struct**

```rust
/// Multi-item upload progress display.
///
/// Driven entirely by `UploadProgress` events: creates aggregate bars on
/// `Enumerated`, tracks bytes on `Uploading`, auto-detects item completion
/// when the file counter reaches the expected count.
pub struct UploadBatchDisplay {
    multi: MultiProgress,
    batch_header: ProgressBar,
    bottom_sentinel: ProgressBar,
    active_items: Mutex<HashMap<String, UploadItemBars>>,
    completed_items: Mutex<usize>,
    items_total: usize,
}

struct UploadItemBars {
    header: ProgressBar,
    bar: ProgressBar,
    per_file_bytes: HashMap<String, u64>,
    files_done: usize,
    files_total: usize,
    bytes_total: u64,
    errors: Vec<String>,
    started_at: std::time::Instant,
}

impl UploadBatchDisplay {
    pub fn new(items_total: usize, jobs: usize) -> Self {
        let multi = MultiProgress::new();

        let batch_header = multi.add(ProgressBar::new_spinner());
        batch_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        batch_header.set_message(format!(
            "Uploading {} items ({} workers)...",
            style(items_total).bold(),
            jobs,
        ));

        let bottom_sentinel = multi.add(ProgressBar::new_spinner());
        bottom_sentinel.set_style(ProgressStyle::with_template("{msg}").unwrap());
        bottom_sentinel.set_message("");
        bottom_sentinel.finish();

        Self {
            multi,
            batch_header,
            bottom_sentinel,
            active_items: Mutex::new(HashMap::new()),
            completed_items: Mutex::new(0),
            items_total,
        }
    }

    pub fn update(&self, p: UploadProgress) {
        let mut items = self.active_items.lock().unwrap();

        match p.status {
            UploadProgressStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                // New item starting — create header + aggregate bar
                if !items.contains_key(&p.identifier) {
                    let header = self.multi.insert_before(
                        &self.bottom_sentinel,
                        ProgressBar::new_spinner(),
                    );
                    header.set_style(ProgressStyle::with_template("{msg}").unwrap());
                    header.set_message(format!(
                        "{} {}",
                        style(ICON_HEADER).cyan(),
                        style(&p.identifier).bold(),
                    ));

                    let bar = self.multi.insert_before(
                        &self.bottom_sentinel,
                        ProgressBar::new(bytes_total),
                    );
                    bar.set_style(
                        ProgressStyle::with_template(&format!(
                            "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes}}/{{total_bytes}} {{bytes_per_sec:.dim}}  ({{msg}})"
                        ))
                        .unwrap()
                        .progress_chars(PROGRESS_CHARS),
                    );
                    bar.set_message(format!("0/{files_count} files"));

                    items.insert(p.identifier.clone(), UploadItemBars {
                        header,
                        bar,
                        per_file_bytes: HashMap::new(),
                        files_done: 0,
                        files_total: files_count,
                        bytes_total,
                        errors: Vec::new(),
                        started_at: std::time::Instant::now(),
                    });
                }
            }
            UploadProgressStatus::Uploading => {
                if let Some(item) = items.get_mut(&p.identifier) {
                    item.per_file_bytes.insert(p.key.clone(), p.bytes_sent);
                    let total: u64 = item.per_file_bytes.values().sum();
                    item.bar.set_position(total);
                }
            }
            UploadProgressStatus::Complete => {
                if let Some(item) = items.get_mut(&p.identifier) {
                    item.per_file_bytes.insert(p.key.clone(), p.total_bytes);
                    let total: u64 = item.per_file_bytes.values().sum();
                    item.bar.set_position(total);

                    item.files_done += 1;
                    item.bar.set_message(format!(
                        "{}/{} files",
                        item.files_done, item.files_total
                    ));

                    self.maybe_finish_item(&p.identifier, &mut items);
                }
            }
            UploadProgressStatus::Skipped => {
                if let Some(item) = items.get_mut(&p.identifier) {
                    item.files_done += 1;
                    item.bar.set_message(format!(
                        "{}/{} files",
                        item.files_done, item.files_total
                    ));
                    self.maybe_finish_item(&p.identifier, &mut items);
                }
            }
            UploadProgressStatus::Failed => {
                if let Some(item) = items.get_mut(&p.identifier) {
                    item.errors.push(format!(
                        "  {} {} {}",
                        style(ICON_ERROR).red(),
                        style(&p.key).dim(),
                        style("— upload failed").red(),
                    ));
                    item.files_done += 1;
                    item.bar.set_message(format!(
                        "{}/{} files",
                        item.files_done, item.files_total
                    ));
                    self.maybe_finish_item(&p.identifier, &mut items);
                }
            }
            UploadProgressStatus::WaitingRateLimit => {
                if let Some(item) = items.get_mut(&p.identifier) {
                    item.bar.set_message("rate limited, waiting...");
                }
            }
            UploadProgressStatus::Verifying => {}
        }
    }

    /// Check if an item is fully done and finalize its display.
    fn maybe_finish_item(
        &self,
        identifier: &str,
        items: &mut HashMap<String, UploadItemBars>,
    ) {
        let done = items.get(identifier).map(|i| i.files_done >= i.files_total).unwrap_or(false);
        if !done {
            return;
        }

        if let Some(item) = items.remove(identifier) {
            item.bar.finish_and_clear();

            // Print errors
            for err in &item.errors {
                eprintln!("{err}");
            }

            let elapsed = item.started_at.elapsed().as_secs_f64();
            let speed = format_speed(item.bytes_total, elapsed);
            let files_uploaded = item.files_done.saturating_sub(item.errors.len());

            let skipped_info = String::new(); // Upload doesn't have a "skipped" count in Enumerated

            item.header.set_message(format!(
                "{} {}       {} files ({}) {:.0}s{}{}",
                style(ICON_SUCCESS).green(),
                style(identifier).bold(),
                files_uploaded,
                format_bytes(item.bytes_total),
                elapsed,
                style(&speed).dim(),
                skipped_info,
            ));
            item.header.finish();

            *self.completed_items.lock().unwrap() += 1;
        }
    }

    pub fn finish(&self, results: &[UploadResult], elapsed: std::time::Duration) {
        self.batch_header.finish_and_clear();
        self.bottom_sentinel.finish_and_clear();

        let uploaded = results.iter().filter(|r| matches!(r.status, UploadStatus::Uploaded)).count();
        let skipped = results.iter().filter(|r| matches!(r.status, UploadStatus::Skipped)).count();
        let failed = results.iter().filter(|r| matches!(r.status, UploadStatus::Failed(_))).count();
        let bytes: u64 = results.iter()
            .filter(|r| matches!(r.status, UploadStatus::Uploaded))
            .map(|r| r.bytes)
            .sum();

        let summary = BatchSummary {
            items_total: self.items_total,
            items_succeeded: *self.completed_items.lock().unwrap(),
            items_failed: self.items_total - *self.completed_items.lock().unwrap(),
            files_skipped: skipped,
            files_failed: failed,
            bytes_total: bytes,
            elapsed_secs: elapsed.as_secs_f64(),
        };
        print_batch_summary(&summary, "uploaded", None);
    }
}
```

- [ ] **Step 2: Update `run_import` to use `UploadBatchDisplay`**

In `ia-cli/src/commands/upload.rs`, replace the inline progress callback in `run_import` with `UploadBatchDisplay`:

```rust
    // Determine number of unique items for the batch display header
    let item_count = {
        let mut ids = std::collections::HashSet::new();
        for (id, _) in &records {
            ids.insert(id.clone());
        }
        ids.len()
    };

    let batch_display = if !json_mode && quiet == 0 {
        Some(std::sync::Arc::new(crate::output::UploadBatchDisplay::new(item_count, jobs)))
    } else {
        None
    };

    let progress_ref: Option<std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>> =
        batch_display.clone().map(|bd| -> std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync> {
            std::sync::Arc::new(move |p: UploadProgress| {
                bd.update(p);
            })
        });
```

After `upload_batch` returns, call `finish()` and update result handling:

```rust
    let start = std::time::Instant::now();
    let results = upload_batch(client, records, &opts, jobs, progress_ref)
        .await
        .context("batch upload failed")?;
    let elapsed = start.elapsed();

    // Finish batch display
    if let Some(bd) = &batch_display {
        bd.finish(&results, elapsed);
    }

    // Write to joblog + check for failures
    let mut had_failure = false;
    for r in &results {
        if matches!(r.status, UploadStatus::Failed(_)) {
            had_failure = true;
        }
        if json_mode {
            match &r.status {
                UploadStatus::Failed(msg) => {
                    let err_json = serde_json::json!({
                        "error": {
                            "code": "upload_failed",
                            "message": msg,
                            "identifier": r.identifier,
                            "key": r.key,
                        }
                    });
                    eprintln!("{}", serde_json::to_string(&err_json).unwrap_or_default());
                }
                _ => {
                    let json = serde_json::to_string(r).context("failed to serialize upload result")?;
                    println!("{json}");
                }
            }
        }
        if let Some(jl) = &joblog {
            write_upload_result(jl, r);
        }
    }
```

Remove the old `if !json_mode && quiet == 0` header print and inline progress callback entirely.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: Zero warnings.

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/output.rs ia-cli/src/commands/upload.rs
git commit -m "feat(upload): add UploadBatchDisplay for batch upload progress

Multi-item upload progress with aggregate bars matching download's
BatchDisplay. Auto-detects item completion from Enumerated file count.
Replaces inline checkmark callbacks in run_import."
```

---

## Task 7: Clean up dead code and run full verification

**Files:**
- Modify: `ia-cli/src/output.rs` (remove unused code)
- Modify: `ia-cli/src/commands/upload.rs` (remove unused helpers)

- [ ] **Step 1: Remove old `print_result_line` calls from quiet==0 path**

The `output_results` function in `commands/upload.rs` is still used for JSON mode and joblog. Check that the `quiet == 0` branch no longer calls `print_result_line` directly (it's handled by the display). If `print_result_line` is now unused, keep it anyway — it's used by `output_results` for fallback.

- [ ] **Step 2: Remove the old batch header print from `run_import`**

The `eprintln!("▸ Uploading {} records from {}...", ...)` line is now handled by `UploadBatchDisplay::new`. Remove it.

- [ ] **Step 3: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings.

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/output.rs ia-cli/src/commands/upload.rs
git commit -m "refactor: clean up dead code after progress display consolidation"
```

---

## Task 8: Update upload CLI tests

**Files:**
- Modify: `ia-cli/tests/upload.rs` (if integration tests reference old output)
- Modify: `ia-cli/src/commands/upload.rs` (unit tests)

- [ ] **Step 1: Check integration tests for output assertions**

Run: `grep -r "spinner\|=>\|\\[=\|bytes_per_sec" ia-cli/tests/`

If any tests assert on the old upload progress format (spinner, `[==>    ]`), update them to expect the new format or remove output format assertions (progress output goes to stderr and is hard to test deterministically).

- [ ] **Step 2: Run full test suite one more time**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings.

- [ ] **Step 4: Commit if any test changes**

```bash
git add ia-cli/tests/ ia-cli/src/commands/upload.rs
git commit -m "test: update upload tests for new progress display format"
```

---

## Pre-PR Checklist

- [ ] `git status` — no uncommitted files that belong in the PR
- [ ] `cargo test -p ia-core -p ia-cli` — all tests pass
- [ ] `cargo clippy -p ia-core -p ia-cli -- -D warnings` — zero warnings
- [ ] Design doc committed: `docs/plans/2026-03-10-progress-display-consolidation-design.md`
- [ ] Plan doc committed: `docs/plans/2026-03-10-progress-display-consolidation-plan.md`
- [ ] MEMORY.md updated
