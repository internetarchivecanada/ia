# Download Output Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Redesign download output — rich inline by default, rename `--tui` to `--dashboard`, add useful stats (speeds, disk space, error counts).

**Architecture:** The ia-core download module already provides callback-based progress events (`DownloadProgress` / `DownloadStatus`). All changes are in ia-cli: improve the `DownloadDisplay` in `output.rs`, rename + un-gate the TUI in `commands/download.rs` and `Cargo.toml`, and extend the ratatui dashboard in `tui/` with batch support and new panels.

**Tech Stack:** indicatif 0.17, console 0.15, ratatui 0.29, crossterm 0.28, libc 0.2 (already deps)

**Design doc:** `docs/plans/2026-02-20-download-output-redesign.md`

---

### Task 1: Make TUI a Default Feature and Rename `--tui` to `--dashboard`

**Files:**
- Modify: `ia-cli/Cargo.toml:27-28`
- Modify: `ia-cli/src/commands/download.rs:76-78` (the `--tui` arg)
- Modify: `ia-cli/src/commands/download.rs:205-214` (the cfg gates)
- Modify: `ia-cli/src/main.rs:7-8` (the `#[cfg(feature = "tui")]` on mod tui)
- Modify: `ia-cli/src/tui/mod.rs:4` (the pub use)
- Test: `ia-cli/tests/cli.rs`

**Step 1: Update Cargo.toml to add default feature**

In `ia-cli/Cargo.toml`, add a `default` line:

```toml
[features]
default = ["tui"]
tui = ["dep:ratatui", "dep:crossterm"]
```

**Step 2: Rename `--tui` to `--dashboard` in DownloadArgs**

In `ia-cli/src/commands/download.rs`, change:

```rust
    /// Full-screen dashboard mode
    #[arg(long)]
    pub dashboard: bool,
```

**Step 3: Update the cfg gates in download.rs**

Replace the dual `#[cfg(feature = "tui")]` / `#[cfg(not(feature = "tui"))]` blocks (lines 205-214) with:

```rust
    // Dashboard mode
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_tui(client, &identifiers, &opts, Arc::clone(&semaphore)).await;
    }

    #[cfg(not(feature = "tui"))]
    if args.dashboard {
        bail!("Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui");
    }
```

Note: we pass `&identifiers` (a Vec) now instead of `&identifiers[0]`. The TUI will be updated in a later task to accept this. For now, keep it as `&identifiers[0]` with the single-item restriction removed — we'll update `run_tui` signature in Task 6.

Actually, keep the current signature for now — just rename the flag:

```rust
    #[cfg(feature = "tui")]
    if args.dashboard && identifiers.len() == 1 {
        return crate::tui::run_tui(client, &identifiers[0], &opts, Arc::clone(&semaphore)).await;
    }

    #[cfg(not(feature = "tui"))]
    if args.dashboard {
        bail!("Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui");
    }
```

**Step 4: Write CLI test for --dashboard flag**

In `ia-cli/tests/cli.rs`, add:

```rust
#[test]
fn download_help_shows_dashboard_flag() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dashboard"));
}

#[test]
fn download_help_does_not_show_tui_flag() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tui").not());
}
```

**Step 5: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass. The new tests verify `--dashboard` appears and `--tui` does not.

**Step 6: Commit**

```bash
git add ia-cli/Cargo.toml ia-cli/src/commands/download.rs ia-cli/tests/cli.rs
git commit -m "feat: rename --tui to --dashboard, make tui a default feature (#27)"
```

---

### Task 2: Add Disk Space Query to output.rs

**Files:**
- Modify: `ia-cli/src/output.rs`

**Step 1: Add disk_space_free function**

Add a function to `ia-cli/src/output.rs` that queries free disk space for a given path. We can reuse the same `libc::statvfs` approach from `ia-core/src/disk_pool.rs`:

```rust
/// Get free disk space for a path (bytes).
pub fn disk_space_free(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let c_path = std::ffi::CString::new(path.to_str()?).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            Some(stat.f_bavail as u64 * stat.f_frsize as u64)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}
```

**Step 2: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass (function is pure, no side effects to test beyond integration).

**Step 3: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "feat: add disk_space_free utility to output module (#27)"
```

---

### Task 3: Redesign DownloadDisplay for Rich Inline Output (Single Item)

**Files:**
- Modify: `ia-cli/src/output.rs` (rewrite `DownloadDisplay`)
- Modify: `ia-cli/src/commands/download.rs:228-265` (single-item output path)

**Step 1: Rewrite DownloadDisplay**

Replace the current `DownloadDisplay` in `ia-cli/src/output.rs` with a richer version that tracks throughput and shows separator lines:

```rust
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};

pub struct DownloadDisplay {
    identifier: String,
    multi: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    header: ProgressBar,
    separator: ProgressBar,
    started_at: Instant,
    bytes_downloaded: Mutex<u64>,
}

impl DownloadDisplay {
    pub fn new(identifier: &str, multi: &MultiProgress) -> Self {
        let header = multi.add(ProgressBar::new_spinner());
        header.set_message(format!(
            "{}  Resolving...",
            style(identifier).bold()
        ));
        header.enable_steady_tick(std::time::Duration::from_millis(100));

        let separator = multi.add(ProgressBar::new_spinner());
        separator.set_style(ProgressStyle::with_template("{msg}").unwrap());
        separator.set_message(
            style("────────────────────────────────────────────────────")
                .dim()
                .to_string(),
        );

        Self {
            identifier: identifier.to_string(),
            multi: multi.clone(),
            bars: Mutex::new(HashMap::new()),
            header,
            separator,
            started_at: Instant::now(),
            bytes_downloaded: Mutex::new(0),
        }
    }

    pub fn update(&self, progress: DownloadProgress) {
        let mut bars = self.bars.lock().unwrap();

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = bars.entry(progress.file_name.clone()).or_insert_with(|| {
                    let pb = self.multi.insert_before(
                        &self.separator,
                        ProgressBar::new(progress.total_bytes.unwrap_or(0)),
                    );
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  {prefix:.dim} {bar:20.cyan/dim} {bytes}/{total_bytes} {bytes_per_sec:.dim}")
                            .unwrap()
                            .progress_chars("━╸─"),
                    );
                    pb.set_prefix(progress.file_name.clone());
                    pb
                });
                bar.set_position(progress.bytes_downloaded);
                *self.bytes_downloaded.lock().unwrap() = progress.bytes_downloaded;
            }
            DownloadStatus::Complete => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style("done").green()
                    ));
                }
            }
            DownloadStatus::Skipped(reason) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("skipped ({reason})")).yellow()
                    ));
                }
            }
            DownloadStatus::Failed(err) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("FAILED: {err}")).red()
                    ));
                }
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn finish(&self, result: &ItemDownloadResult, destdir: &Path) {
        self.header.finish_and_clear();

        let elapsed = result.elapsed.as_secs_f64();
        let speed = if elapsed > 0.0 {
            format!(" · {}/s", format_bytes((result.bytes_total as f64 / elapsed) as u64))
        } else {
            String::new()
        };

        let summary = format!(
            "{}  {} files ({}) in {:.1}s{}",
            style(&self.identifier).bold(),
            result.files_downloaded,
            format_bytes(result.bytes_total),
            elapsed,
            style(&speed).dim(),
        );

        let stats = format!(
            "  {} {} downloaded · {} skipped · {} errors",
            style("✓").green(),
            result.files_downloaded,
            if result.files_skipped > 0 {
                style(result.files_skipped.to_string()).yellow().to_string()
            } else {
                "0".to_string()
            },
            if result.files_failed > 0 {
                style(result.files_failed.to_string()).red().to_string()
            } else {
                "0".to_string()
            },
        );

        eprintln!("{summary}");
        eprintln!("{stats}");

        // Disk space
        if let Some(free) = disk_space_free(destdir) {
            eprintln!("  {}: {} free", style(destdir.display()).dim(), format_bytes(free));
        }
    }
}
```

**Step 2: Update single-item path in download.rs**

In `ia-cli/src/commands/download.rs`, update the single-item block to pass `MultiProgress` and `destdir` to the new `DownloadDisplay`:

The `DownloadDisplay::new` now takes a `&MultiProgress`, so create one in the calling code:

```rust
    // Single item
    if identifiers.len() == 1 {
        let identifier = &identifiers[0];
        let item_opts = if let Some(ref mut pool) = disk_pool {
            let dest = pool.assign_item(identifier, 0)?;
            make_opts(dest.to_path_buf())
        } else {
            opts.clone()
        };

        let multi = indicatif::MultiProgress::new();
        let display = if quiet == 0 {
            Some(Arc::new(DownloadDisplay::new(identifier, &multi)))
        } else {
            None
        };

        let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
            display.clone().map(|d| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                Arc::new(move |p| d.update(p))
            });

        let result = ia_core::download::download_item(
            client, identifier, &item_opts, Arc::clone(&semaphore), progress,
        ).await.context(format!("failed to download {}", identifier))?;

        if let Some(d) = display {
            d.finish(&result, &item_opts.destdir);
        }
        // ... joblog and quiet handling unchanged
    }
```

**Step 3: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-cli/src/output.rs ia-cli/src/commands/download.rs
git commit -m "feat: redesign single-item inline output with speeds and disk space (#27)"
```

---

### Task 4: Redesign Batch Inline Output

**Files:**
- Modify: `ia-cli/src/output.rs` (add `BatchDisplay`)
- Modify: `ia-cli/src/commands/download.rs:274-367` (batch output path)

**Step 1: Add BatchDisplay to output.rs**

Add a new `BatchDisplay` struct that manages batch download output with per-item summaries and active file progress bars:

```rust
use ia_core::download::BatchDownloadResult;
use ia_core::disk_pool::DiskStatus;

pub struct BatchDisplay {
    multi: MultiProgress,
    items_total: usize,
    jobs: usize,
    header: ProgressBar,
    active_item_bars: Mutex<HashMap<String, ItemBars>>,
    completed_count: Mutex<usize>,
    bytes_total: Mutex<u64>,
    started_at: Instant,
}

struct ItemBars {
    header: ProgressBar,
    file_bars: HashMap<String, ProgressBar>,
    separator: ProgressBar,
}

impl BatchDisplay {
    pub fn new(items_total: usize, jobs: usize) -> Self {
        let multi = MultiProgress::new();

        let header = multi.add(ProgressBar::new_spinner());
        header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        header.set_message(format!(
            "Downloading {} items ({} workers)...",
            style(items_total).bold(),
            jobs,
        ));

        // Top separator
        let sep = multi.add(ProgressBar::new_spinner());
        sep.set_style(ProgressStyle::with_template("{msg}").unwrap());
        sep.set_message(
            style("────────────────────────────────────────────────────")
                .dim()
                .to_string(),
        );

        Self {
            multi,
            items_total,
            jobs,
            header,
            active_item_bars: Mutex::new(HashMap::new()),
            completed_count: Mutex::new(0),
            bytes_total: Mutex::new(0),
            started_at: Instant::now(),
        }
    }

    pub fn on_item_start(&self, identifier: &str, current: usize, total: usize) {
        // Add item header bar
        let item_header = self.multi.add(ProgressBar::new_spinner());
        item_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        item_header.set_message(format!(
            "{} {}",
            style("▸").cyan(),
            style(identifier).bold(),
        ));

        let separator = self.multi.add(ProgressBar::new_spinner());
        separator.set_style(ProgressStyle::with_template("{msg}").unwrap());
        separator.set_message(""); // invisible until needed

        let mut items = self.active_item_bars.lock().unwrap();
        items.insert(identifier.to_string(), ItemBars {
            header: item_header,
            file_bars: HashMap::new(),
            separator,
        });
    }

    pub fn on_progress(&self, identifier: &str, progress: DownloadProgress) {
        let mut items = self.active_item_bars.lock().unwrap();
        let Some(item) = items.get_mut(identifier) else { return };

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = item.file_bars.entry(progress.file_name.clone()).or_insert_with(|| {
                    let pb = self.multi.insert_before(
                        &item.separator,
                        ProgressBar::new(progress.total_bytes.unwrap_or(0)),
                    );
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  {prefix:.dim} {bar:20.cyan/dim} {bytes}/{total_bytes} {bytes_per_sec:.dim}")
                            .unwrap()
                            .progress_chars("━╸─"),
                    );
                    pb.set_prefix(progress.file_name.clone());
                    pb
                });
                bar.set_position(progress.bytes_downloaded);
            }
            DownloadStatus::Complete => {
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_and_clear();
                }
            }
            DownloadStatus::Skipped(_) => {
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_and_clear();
                }
            }
            DownloadStatus::Failed(err) => {
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {} {}",
                        style("✗").red(),
                        progress.file_name,
                        style(err).red(),
                    ));
                }
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn on_item_complete(&self, identifier: &str, result: &ia_core::download::ItemDownloadResult) {
        let mut items = self.active_item_bars.lock().unwrap();
        if let Some(item) = items.remove(identifier) {
            // Clear all remaining file bars
            for (_, bar) in item.file_bars {
                bar.finish_and_clear();
            }
            item.separator.finish_and_clear();

            let elapsed = result.elapsed.as_secs_f64();
            let speed = if elapsed > 0.0 {
                format!("  {}/s", format_bytes((result.bytes_total as f64 / elapsed) as u64))
            } else {
                String::new()
            };

            let skipped_info = if result.files_skipped > 0 {
                format!("\n  {} {} skipped", style("─").dim(), style(result.files_skipped).yellow())
            } else {
                String::new()
            };

            item.header.set_message(format!(
                "{} {}       {} files ({}) {:.0}s{}{}",
                style("✓").green(),
                style(identifier).bold(),
                result.files_downloaded,
                format_bytes(result.bytes_total),
                elapsed,
                style(&speed).dim(),
                skipped_info,
            ));
            item.header.finish();
        }

        *self.completed_count.lock().unwrap() += 1;
        *self.bytes_total.lock().unwrap() += result.bytes_total;
    }

    pub fn finish(&self, result: &BatchDownloadResult, disk_statuses: Option<&[DiskStatus]>) {
        self.header.finish_and_clear();

        let elapsed = result.elapsed.as_secs_f64();
        let speed = if elapsed > 0.0 {
            format!(" · {}/s", format_bytes((result.bytes_total as f64 / elapsed) as u64))
        } else {
            String::new()
        };

        eprintln!(
            "{}",
            style("────────────────────────────────────────────────────").dim()
        );
        eprintln!(
            "{}/{} items ({} done · {} skipped · {} errors)",
            result.items_succeeded,
            result.items_total,
            style(result.items_succeeded).green(),
            if result.files_skipped > 0 {
                style(result.files_skipped.to_string()).yellow().to_string()
            } else {
                "0".to_string()
            },
            if result.files_failed > 0 || result.items_failed > 0 {
                style((result.files_failed + result.items_failed).to_string()).red().to_string()
            } else {
                "0".to_string()
            },
        );
        eprintln!(
            "{} downloaded{}",
            format_bytes(result.bytes_total),
            style(&speed).dim(),
        );

        // Disk space
        if let Some(statuses) = disk_statuses {
            let disk_line: Vec<String> = statuses.iter().map(|ds| {
                format!("{}: {} free", style(ds.path.display()).dim(), format_bytes(ds.free_bytes))
            }).collect();
            eprintln!("{}", disk_line.join(" │ "));
        }

        eprintln!("{:.1}s elapsed", elapsed);
    }
}
```

**Step 2: Update batch path in download.rs**

Replace the batch mode output logic with `BatchDisplay`. The key change is that `download_batch` needs per-item progress callbacks with the identifier. Currently, the progress callback doesn't include the item identifier — it only has `file_name`. We need to augment the callback or restructure the batch loop.

**Approach:** Instead of using `download_batch`, iterate items manually in the CLI so we can wire per-item callbacks. This gives us per-item start/complete hooks.

Restructure batch mode in `commands/download.rs`:

```rust
    // Batch mode
    let batch_display = if quiet == 0 {
        Some(BatchDisplay::new(identifiers.len(), jobs))
    } else {
        None
    };

    let start = std::time::Instant::now();
    let mut item_results = Vec::new();

    for (i, identifier) in identifiers.iter().enumerate() {
        if let Some(ref bd) = batch_display {
            bd.on_item_start(identifier, i + 1, identifiers.len());
        }

        let item_opts = if let Some(ref mut pool) = disk_pool {
            let dest = pool.assign_item(identifier, 0)?;
            make_opts(dest.to_path_buf())
        } else {
            opts.clone()
        };

        let bd_clone = batch_display.as_ref().map(Arc::new); // Need to handle this
        // ... wire progress callback per-item
    }
```

Actually, this is getting complex. Let's keep it simpler — keep using `download_batch` but add the identifier to `DownloadProgress`. That's a small ia-core change.

**Alternative: Add identifier to DownloadProgress**

In `ia-core/src/download.rs`, add an `identifier` field to `DownloadProgress`:

```rust
pub struct DownloadProgress {
    pub identifier: String,
    pub file_name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub status: DownloadStatus,
}
```

Then update all places that create a `DownloadProgress` in `download_file()` to include the identifier (passed down from `download_item`). This is a small, clean change.

**This is the recommended approach.** It keeps the architecture clean and avoids restructuring the batch loop.

**Step 2a: Add identifier to DownloadProgress in ia-core**

In `ia-core/src/download.rs`:

1. Add `pub identifier: String` to `DownloadProgress` struct (line 51)
2. Update all `DownloadProgress { ... }` constructors in `download_file()` to include `identifier: identifier.to_string()`
3. Update `download_file()` signature to take `identifier: &str` (it doesn't currently — it's passed from `download_item`). Actually, looking at the code, `download_file` already takes `identifier: &str` (line 82). Good.

Update every `DownloadProgress` construction in `download_file()` to include `identifier: identifier.to_string()`.

**Step 2b: Update ia-cli code that creates DownloadProgress or reads it**

- `ia-cli/src/output.rs` — `DownloadDisplay::update()` now has `progress.identifier` available
- `ia-cli/src/tui/app.rs` — `TuiState::update()` now has `progress.identifier` available
- `ia-cli/src/commands/download.rs` — batch mode callback can use `progress.identifier`

**Step 3: Wire BatchDisplay into download.rs batch path**

Add an `on_item_start` callback using `download_batch`'s existing `on_item_start` parameter. Add an `on_item_complete` callback — this needs to be added to `download_batch` in ia-core.

**Actually**, looking at the code more carefully, `download_batch` spawns all items concurrently and only returns a `BatchDownloadResult` at the end. It has `on_item_start` but no `on_item_complete`. We need to add one.

In `ia-core/src/download.rs`, add an `on_item_complete` callback parameter to `download_batch`:

```rust
pub async fn download_batch(
    client: &IaClient,
    identifiers: Vec<String>,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
    on_item_start: Option<Arc<dyn Fn(&str, usize, usize) + Send + Sync>>,
    on_item_complete: Option<Arc<dyn Fn(&ItemDownloadResult) + Send + Sync>>,
) -> BatchDownloadResult {
```

And call it after each item completes successfully in the spawn loop.

**Step 4: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add ia-core/src/download.rs ia-cli/src/output.rs ia-cli/src/commands/download.rs
git commit -m "feat: redesign batch inline output with per-item progress and stats (#27)"
```

---

### Task 5: Add Disk Space to Batch Summary

**Files:**
- Modify: `ia-cli/src/commands/download.rs` (batch finish call)

**Step 1: Pass disk pool status to BatchDisplay::finish**

After `download_batch` returns, get disk pool status and pass to `finish`:

```rust
    let disk_statuses = disk_pool.as_ref().map(|p| p.status());
    if let Some(ref bd) = batch_display {
        bd.finish(&result, disk_statuses.as_deref());
    }
```

For single-disk (no pool), query free space directly:

```rust
    // If no pool, still report disk space for the single destdir
    if disk_pool.is_none() {
        if let Some(ref bd) = batch_display {
            if let Some(free) = disk_space_free(&base_destdir) {
                let single_status = vec![ia_core::disk_pool::DiskStatus {
                    path: base_destdir.clone(),
                    free_bytes: free,
                    total_bytes: 0,
                    items_count: result.items_total,
                }];
                bd.finish(&result, Some(&single_status));
            } else {
                bd.finish(&result, None);
            }
        }
    }
```

**Step 2: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 3: Commit**

```bash
git add ia-cli/src/commands/download.rs
git commit -m "feat: add disk space reporting to batch download summary (#27)"
```

---

### Task 6: Extend TUI Dashboard for Batch Downloads

**Files:**
- Modify: `ia-cli/src/tui/app.rs` (add ItemState, batch support, throughput history, ETA)
- Modify: `ia-cli/src/tui/ui.rs` (add Items panel, Disks panel, Errors panel, Throughput sparkline)
- Modify: `ia-cli/src/tui/mod.rs` (update run_tui signature)
- Modify: `ia-cli/src/commands/download.rs` (pass identifiers vec to run_tui)

**Step 1: Add ItemState and batch tracking to TuiState**

In `ia-cli/src/tui/app.rs`, add:

```rust
#[derive(Debug, Clone)]
pub struct ItemState {
    pub identifier: String,
    pub status: ItemStatus,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub started_at: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemStatus {
    Pending,
    Downloading,
    Complete,
    Failed(String),
}

impl ItemState {
    pub fn throughput(&self) -> f64 {
        let secs = self.started_at.elapsed().as_secs_f64();
        if secs > 0.0 { self.bytes_downloaded as f64 / secs } else { 0.0 }
    }
}
```

Update `TuiState` to include:

```rust
pub struct TuiState {
    // ... existing fields ...
    pub items: Vec<ItemState>,          // Per-item tracking
    pub throughput_history: Vec<f64>,   // Ring buffer, 1 sample/sec, max 60
    pub last_throughput_sample: Instant,
    pub disk_statuses: Vec<(String, u64)>,  // (path_display, free_bytes)
}
```

Add throughput sampling in `update()` — sample once per second.

Add ETA calculation:

```rust
pub fn eta_seconds(&self) -> Option<f64> {
    let speed = self.throughput();
    if speed > 0.0 && self.bytes_total > self.bytes_downloaded {
        Some((self.bytes_total - self.bytes_downloaded) as f64 / speed)
    } else {
        None
    }
}
```

**Step 2: Update run_tui to accept Vec of identifiers**

Change signature from `identifier: &str` to `identifiers: &[String]`. For batch, spawn multiple `download_item` tasks. This mirrors what `download_batch` does but with TUI state updates.

```rust
pub async fn run_tui(
    client: &IaClient,
    identifiers: &[String],
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
) -> anyhow::Result<()> {
```

**Step 3: Update download.rs to pass full identifiers vec**

```rust
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_tui(client, &identifiers, &opts, Arc::clone(&semaphore)).await;
    }
```

Remove the `identifiers.len() == 1` restriction.

**Step 4: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add ia-cli/src/tui/ ia-cli/src/commands/download.rs
git commit -m "feat: extend dashboard for batch downloads with item tracking (#27)"
```

---

### Task 7: Add Dashboard Panels (Items, Disks, Errors, Throughput)

**Files:**
- Modify: `ia-cli/src/tui/ui.rs` (add new panels)

**Step 1: Add Items panel**

Add `draw_items_panel()` function that renders a scrollable list of items with status, files, bytes, speed. Only shown if >1 item.

**Step 2: Add Disks panel**

Add `draw_disks_panel()` function. Only shown if disk pool is active (disk_statuses is non-empty). Shows per-disk path, free space, and item count.

**Step 3: Add Errors panel**

Add `draw_errors_panel()` function. Only shown if `failed_files` is non-empty. Scrollable list of failed files with error messages.

**Step 4: Add Throughput sparkline**

Add `draw_throughput_panel()` using ratatui's `Sparkline` widget with the throughput history ring buffer.

**Step 5: Update layout in draw()**

Update the main `draw()` function to use dynamic layout constraints based on which panels are visible:

```rust
pub fn draw(f: &mut Frame, state: &TuiState) {
    let mut constraints = vec![Constraint::Length(3)]; // Header always

    let show_items = state.items.len() > 1;
    if show_items {
        constraints.push(Constraint::Min(5)); // Items panel
    }

    constraints.push(Constraint::Min(5)); // Workers panel (always)

    let show_disks = !state.disk_statuses.is_empty();
    if show_disks {
        constraints.push(Constraint::Length(3)); // Disks panel
    }

    let show_errors = !state.failed_files.is_empty();
    if show_errors {
        constraints.push(Constraint::Length(5)); // Errors panel
    }

    constraints.push(Constraint::Length(4)); // Throughput sparkline
    constraints.push(Constraint::Length(3)); // Status bar

    // ... split and render
}
```

**Step 6: Update header to show ETA**

```rust
fn draw_header(f: &mut Frame, area: Rect, state: &TuiState) {
    let eta_str = state.eta_seconds()
        .map(|s| format!("  ETA {}", format_duration(s as u64)))
        .unwrap_or_default();

    let label = format!(
        " {} — {:.0}% ({})  {}/s{} ",
        if state.items.len() > 1 { format!("{} items", state.items.len()) } else { state.identifier.clone() },
        progress * 100.0,
        format_bytes(state.bytes_downloaded),
        format_bytes(throughput as u64),
        eta_str,
    );
    // ...
}
```

**Step 7: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 8: Commit**

```bash
git add ia-cli/src/tui/ui.rs
git commit -m "feat: add Items, Disks, Errors, and Throughput panels to dashboard (#27)"
```

---

### Task 8: Update CLI Tests and Final Cleanup

**Files:**
- Modify: `ia-cli/tests/cli.rs`
- Modify: `ia-cli/src/commands/download.rs` (remove old batch output code)

**Step 1: Verify all CLI integration tests pass**

Run: `cargo test --workspace`

**Step 2: Add test for --dashboard flag in download help**

Already added in Task 1. Verify it's there.

**Step 3: Clean up any dead code**

- Remove old `format_bytes` from `commands/download.rs` (use the one from `output.rs` or make it `pub`)
- Remove duplicate `format_bytes` definitions — consolidate to one place (`output.rs` as `pub`)
- Remove any leftover `--tui` references in comments

**Step 4: Run clippy**

Run: `cargo clippy --workspace --all-features -- -D warnings`
Expected: No warnings.

**Step 5: Run full test suite**

Run: `cargo test --workspace --all-features`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add -A
git commit -m "chore: clean up dead code, consolidate format_bytes, final tests (#27)"
```

---

### Task 9: Update Documentation

**Files:**
- Modify: `CLAUDE.md` (update --tui references to --dashboard)
- Modify: MEMORY.md (update CLI info)

**Step 1: Update CLAUDE.md**

- Change any `--tui` references to `--dashboard`
- Note that `tui` is now a default feature

**Step 2: Update MEMORY.md**

- Update CLI files section: `--tui` → `--dashboard`
- Update Key CLI Files: note `tui/` is now always compiled
- Note the new `BatchDisplay` in `output.rs`

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update documentation for --dashboard rename (#27)"
```

---

### Task 10: Create PR

**Step 1: Push branch and create PR**

```bash
git push -u origin feat/download-output-redesign
gh pr create --title "feat: redesign download output (#27)" --body "..."
```

Link to issue #27 in the PR body.
