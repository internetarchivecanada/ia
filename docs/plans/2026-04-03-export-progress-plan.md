# Export Progress Display — Implementation Plan

**Goal:** Replace the bare `\rFetched N/M items` counter in metadata export with an indicatif progress bar, capped inline error display, and color-coded summary stats — consistent with download/upload output.

**Architecture:** All changes in two files: make icon/bar constants public in `output.rs`, then rewrite the display logic in `run_export()` in `metadata.rs`. The progress bar tracks items (not bytes), errors are shown inline up to a cap of 5, and a summary block prints at the end using the existing separator/color conventions.

**Tech Stack:** `indicatif` (ProgressBar, ProgressStyle), `console` (style, Color), existing `output.rs` constants.

---

### Task 1: Make output.rs constants public

**Files:**
- Modify: `ia-cli/src/output.rs:16-27`

- [ ] **Step 1: Change const visibility**

In `ia-cli/src/output.rs`, change the icon and bar constants from `const` to `pub const`:

```rust
/// Progress bar characters: filled, head, empty.
pub const PROGRESS_CHARS: &str = "━╸─";

/// Progress bar width for per-item bars.
/// Total line: 2 (indent) + BAR + bytes + speed + msg ≈ 78 cols.
pub const BAR_WIDTH: usize = 28;

// Icons used across all progress displays.
pub const ICON_HEADER: &str = "▸";
pub const ICON_SUCCESS: &str = "✓";
pub const ICON_ERROR: &str = "✗";
pub const ICON_SKIPPED: &str = "–";
pub const ICON_DRY_RUN: &str = "⊘";
```

- [ ] **Step 2: Verify compilation**

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo check`
Expected: clean compilation, no errors.

- [ ] **Step 3: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "refactor: make output icon and bar constants public

Export progress will reuse these constants for consistent styling."
```

---

### Task 2: Add imports and constants to metadata.rs

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:1-5`

- [ ] **Step 1: Add new imports**

Add `AtomicU64` to the existing atomic import, add `indicatif`, and add the output constants. The imports block at the top of `metadata.rs` should change from:

```rust
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
```

to:

```rust
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
```

And after the existing `use console::style;` line, add:

```rust
use indicatif::{ProgressBar, ProgressStyle};
```

And after the `use ia_core::{IaClient, IaError};` line, add:

```rust
use crate::output::{format_bytes, BAR_WIDTH, ICON_ERROR, ICON_SUCCESS, PROGRESS_CHARS};
```

- [ ] **Step 2: Add the max-errors constant**

After the imports, before the `// ─── Filter enums` section comment, add:

```rust
/// Maximum number of errors to display inline during export.
const MAX_INLINE_ERRORS: usize = 5;
```

- [ ] **Step 3: Verify compilation**

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo check`
Expected: warnings about unused imports (that's fine — we'll use them in the next task).

- [ ] **Step 4: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "refactor: add imports for export progress display"
```

---

### Task 3: Rewrite run_export with progress bar and summary

This is the main task. Replace the `\rFetched N/M` counter with an indicatif progress bar, add capped inline error display, and print a color-coded summary.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:1094-1332` (the entire `run_export` function)

- [ ] **Step 1: Replace the progress setup section**

Replace the section from `let total = identifiers.len();` through the stream creation (lines 1162-1181) with:

```rust
    let total_with_skipped = total + skipped;
    let client = Arc::new(client.clone());
    let succeeded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let bytes_total = Arc::new(AtomicU64::new(0));
    let errors_shown = Arc::new(AtomicUsize::new(0));
    let start = std::time::Instant::now();

    // Progress bar: tracks items, shows items/s rate
    let bar = if quiet == 0 && total > 0 {
        let msg = if retry_failed {
            format!(
                "Retrying {} failed item(s) from joblog...",
                style(total).bold()
            )
        } else if skipped > 0 {
            format!(
                "Exporting metadata... {}",
                style(format!("(resuming — {skipped} already exported)")).dim()
            )
        } else {
            "Exporting metadata...".to_string()
        };

        let pb = ProgressBar::new(total_with_skipped as u64);
        pb.set_style(
            ProgressStyle::with_template(&format!(
                "{{msg}}\n  {{bar:{BAR_WIDTH}.cyan/dim}} {{pos}}/{{len}} {{per_sec:.dim}}  ({{elapsed}} elapsed)"
            ))
            .unwrap()
            .progress_chars(PROGRESS_CHARS),
        );
        pb.set_message(msg);
        // For resume: start at the skip offset so bar shows overall progress
        if skipped > 0 {
            pb.set_position(skipped as u64);
        }
        Some(pb)
    } else {
        None
    };

    // Fetch items concurrently, streaming results as they complete.
    // Only `jobs` futures are in-flight at a time (not all N at once).
    let mut stream = stream::iter(identifiers)
        .map(|id| {
            let client = client.clone();
            async move {
                let start = std::time::Instant::now();
                let result = client.get_item(&id).await;
                let elapsed_ms = start.elapsed().as_millis() as u64;
                (id, result, elapsed_ms)
            }
        })
        .buffer_unordered(jobs);
```

Note: the stream no longer carries a counter `n` — the progress bar handles position tracking.

- [ ] **Step 2: Replace the file-mode loop**

Replace the file-mode section (`if let Some(ref path) = args.output { ... }`) with:

```rust
    if let Some(ref path) = args.output {
        // File mode: collect all records for unified column computation.
        let mut records: Vec<ia_core::spreadsheet::SpreadsheetRecord> = Vec::new();

        while let Some((identifier, result, elapsed_ms)) = stream.next().await {
            match result {
                Ok(item) => {
                    let json_bytes = serde_json::to_string(&item)
                        .map(|s| s.len() as u64)
                        .unwrap_or(0);
                    bytes_total.fetch_add(json_bytes, Ordering::Relaxed);
                    succeeded.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "")
                                .ok(json_bytes, elapsed_ms),
                        );
                    }
                    if let Some(ref pb) = bar {
                        pb.inc(1);
                    }
                    let metadata_json = serde_json::to_value(&item.metadata)?;
                    let mut fields = HashMap::new();
                    if let serde_json::Value::Object(map) = metadata_json {
                        for (key, value) in map {
                            if key == "identifier" {
                                continue;
                            }
                            match &value {
                                serde_json::Value::String(s) => {
                                    if !s.is_empty() {
                                        fields.insert(key, s.clone());
                                    }
                                }
                                serde_json::Value::Array(arr) if arr.len() == 1 => {
                                    let s = match &arr[0] {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    };
                                    if !s.is_empty() {
                                        fields.insert(key, s);
                                    }
                                }
                                serde_json::Value::Array(arr) => {
                                    for (i, elem) in arr.iter().enumerate() {
                                        let s = match elem {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        if !s.is_empty() {
                                            fields.insert(format!("{key}[{i}]"), s);
                                        }
                                    }
                                }
                                serde_json::Value::Null => {}
                                other => {
                                    let s = other.to_string();
                                    if !s.is_empty() {
                                        fields.insert(key, s);
                                    }
                                }
                            }
                        }
                    }
                    records.push((identifier, fields));
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    failed.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "").error(&msg, 0),
                        );
                    }
                    if let Some(ref pb) = bar {
                        pb.inc(1);
                    }
                    // Show up to MAX_INLINE_ERRORS errors inline
                    let shown = errors_shown.fetch_add(1, Ordering::Relaxed);
                    if quiet == 0 && shown < MAX_INLINE_ERRORS {
                        if let Some(ref pb) = bar {
                            pb.suspend(|| {
                                eprintln!(
                                    "{} {} {}",
                                    style(ICON_ERROR).red(),
                                    style(&identifier).bold(),
                                    style(format!("— {msg}")).red(),
                                );
                            });
                        } else {
                            eprintln!(
                                "{} {} {}",
                                style(ICON_ERROR).red(),
                                style(&identifier).bold(),
                                style(format!("— {msg}")).red(),
                            );
                        }
                    }
                }
            }
        }

        // Finish progress bar
        if let Some(ref pb) = bar {
            pb.finish_and_clear();
        }

        ia_core::spreadsheet::write_spreadsheet(path, &records)
            .context(format!("failed to write export file: {}", path.display()))?;

        // Print summary
        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            Some(path),
            quiet,
        );
    }
```

- [ ] **Step 3: Replace the stdout-mode loop**

Replace the `else` branch (stdout mode) with:

```rust
    else {
        // Stdout mode: stream results as they complete (no buffering).
        while let Some((identifier, result, elapsed_ms)) = stream.next().await {
            match result {
                Ok(item) => {
                    let output = if args.pretty {
                        serde_json::to_string_pretty(&item)?
                    } else {
                        serde_json::to_string(&item)?
                    };
                    let json_bytes = output.len() as u64;
                    bytes_total.fetch_add(json_bytes, Ordering::Relaxed);
                    succeeded.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "")
                                .ok(json_bytes, elapsed_ms),
                        );
                    }
                    if let Some(ref pb) = bar {
                        pb.suspend(|| println!("{output}"));
                        pb.inc(1);
                    } else {
                        println!("{output}");
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    failed.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "").error(&msg, 0),
                        );
                    }
                    if let Some(ref pb) = bar {
                        pb.inc(1);
                    }
                    let shown = errors_shown.fetch_add(1, Ordering::Relaxed);
                    if quiet == 0 && shown < MAX_INLINE_ERRORS {
                        if let Some(ref pb) = bar {
                            pb.suspend(|| {
                                eprintln!(
                                    "{} {} {}",
                                    style(ICON_ERROR).red(),
                                    style(&identifier).bold(),
                                    style(format!("— {msg}")).red(),
                                );
                            });
                        } else {
                            eprintln!(
                                "{} {} {}",
                                style(ICON_ERROR).red(),
                                style(&identifier).bold(),
                                style(format!("— {msg}")).red(),
                            );
                        }
                    }
                }
            }
        }

        // Finish progress bar
        if let Some(ref pb) = bar {
            pb.finish_and_clear();
        }

        // Print summary (no output path for stdout mode)
        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            None,
            quiet,
        );
    }
```

- [ ] **Step 4: Replace the old summary block at the end of run_export**

Delete the old summary block (the `let errors = error_count.load(...)` through the end of the function) and replace with just:

```rust
    Ok(())
}
```

The summary is now printed by `print_export_summary` inside each branch.

- [ ] **Step 5: Add the print_export_summary helper function**

Add this function right before `run_export` (after the `collect_identifiers_from_export` function, before the `// ─── Export` section or right after the `run_export` closing brace — whichever reads better). Place it between `collect_identifiers_from_export` and `run_export`:

```rust
/// Print the export summary footer to stderr.
fn print_export_summary(
    succeeded: usize,
    failed: usize,
    bytes: u64,
    elapsed_secs: f64,
    errors_shown: usize,
    output_path: Option<&Path>,
    quiet: u8,
) {
    if quiet >= 2 {
        return;
    }

    let total = succeeded + failed;

    // Overflow error count (errors beyond the inline display cap)
    let overflow = if errors_shown > MAX_INLINE_ERRORS {
        errors_shown - MAX_INLINE_ERRORS
    } else {
        0
    };
    if overflow > 0 && quiet == 0 {
        eprintln!(
            "  {} {overflow} more error(s) (see joblog)",
            style("...").dim(),
        );
    }

    // Separator
    eprintln!(
        "{}",
        style("────────────────────────────────────────────────────").dim()
    );

    // Items line
    if failed == 0 {
        eprintln!(
            "{} {} items exported",
            style(ICON_SUCCESS).green(),
            style(succeeded).green(),
        );
    } else {
        eprintln!(
            "{}/{} items exported · {} failed",
            style(succeeded).green(),
            total,
            style(failed).red(),
        );
    }

    // Bytes / speed / elapsed line
    let speed = if elapsed_secs > 0.0 {
        format!("{:.1} items/s", total as f64 / elapsed_secs)
    } else {
        String::new()
    };
    eprintln!(
        "{}",
        style(format!(
            "{} fetched · {} · {:.1}s elapsed",
            format_bytes(bytes),
            speed,
            elapsed_secs,
        ))
        .dim(),
    );

    // Output file line (file mode only)
    if let Some(path) = output_path {
        eprintln!("exported to {}", style(path.display()).bold());
    }

    // Warning for failures
    if failed > 0 {
        eprintln!(
            "{} {} item(s) failed — re-run with --retry-failed to retry",
            style("warning:").yellow().bold(),
            failed,
        );
    }
}
```

- [ ] **Step 6: Add `use std::path::Path;` if not already imported**

Check if `Path` is already in scope. It's used via `PathBuf` but `print_export_summary` takes `Option<&Path>`. Since `PathBuf` derefs to `Path`, and `Path` is in `std::path` which is already imported via `use std::path::PathBuf;`, we just need to change the import:

```rust
use std::path::{Path, PathBuf};
```

- [ ] **Step 7: Remove the now-unused `\rFetched` resume message**

The old resume message at lines 1132-1136 (`"Resuming: skipping {} already-exported item(s) from joblog"`) is no longer needed — it's been replaced by the progress bar's message line. Remove it. The `skip_set` code should still compute `skipped` but no longer print its own message.

Change:

```rust
                if !done.is_empty() && quiet == 0 {
                    eprintln!(
                        "Resuming: skipping {} already-exported item(s) from joblog",
                        done.len()
                    );
                }
```

to:

```rust
                // Resume count is displayed in the progress bar header message
```

- [ ] **Step 8: Also remove the retry message that's now in the progress bar**

The old message at line 1114 (`"Retrying {} failed item(s) from joblog"`) is now handled by the progress bar header. Remove it:

Change:

```rust
            eprintln!("Retrying {} failed item(s) from joblog", failed.len());
```

to just nothing (delete the line). The progress bar message handles this now.

- [ ] **Step 9: Verify compilation**

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo check`
Expected: clean compilation.

- [ ] **Step 10: Run cargo fmt**

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo fmt --all`

- [ ] **Step 11: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "feat: add progress bar and summary stats to metadata export

Replace bare 'Fetched N/M items' counter with:
- indicatif progress bar with items/s rate and elapsed time
- Capped inline error display (max 5, then 'N more errors' note)
- Color-coded summary: separator, success/fail counts, bytes, speed
- Scenario-aware header: normal, resume (with offset), retry-failed
- Clean run shows checkmark, error run shows counts + warning"
```

---

### Task 4: Update existing tests and add new ones

**Files:**
- Modify: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Update the existing `metadata_export_writes_joblog` test**

The test currently checks that the joblog is non-empty and contains `"op":"export"`. This still works — no changes needed. Verify:

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo test -p ia-cli --test cli -- metadata_export_writes_joblog -v`
Expected: PASS

- [ ] **Step 2: Add a test for progress bar output on stderr**

After the existing `metadata_export_writes_joblog` test, add:

```rust
#[test]
fn metadata_export_progress_bar_shown() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // Progress output includes "Exporting metadata..." and the summary separator
    ia().args(["metadata", "export", "--itemlist", ids.to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("Exporting metadata..."))
        .stderr(predicate::str::contains("──────────"));
}
```

- [ ] **Step 3: Add a test for summary stats**

```rust
#[test]
fn metadata_export_shows_summary_stats() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // Summary should show "items exported" and "fetched"
    ia().args(["metadata", "export", "--itemlist", ids.to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("items exported"))
        .stderr(predicate::str::contains("fetched"));
}
```

- [ ] **Step 4: Add a test for quiet mode suppressing progress**

```rust
#[test]
fn metadata_export_quiet_suppresses_progress() {
    let dir = tempfile::tempdir().unwrap();
    let ids = dir.path().join("ids.txt");
    std::fs::write(&ids, "test-nonexistent-id\n").unwrap();

    // -q should suppress the progress bar but still show summary
    ia().args([
        "metadata",
        "export",
        "--itemlist",
        ids.to_str().unwrap(),
        "-q",
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("Exporting metadata...").not())
    .stderr(predicate::str::contains("items exported"));
}
```

- [ ] **Step 5: Run the new tests**

Run: `cd ~/github/jjjake/worktrees/export-joblog && cargo test -p ia-cli --test cli -- metadata_export_ -v`
Expected: all `metadata_export_*` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add ia-cli/tests/cli.rs
git commit -m "test: add integration tests for export progress display

Tests: progress bar header shown, summary stats present, quiet mode
suppresses progress bar but preserves summary."
```

---

### Task 5: Run full CI and push

- [ ] **Step 1: Run full CI**

Run: `cd ~/github/jjjake/worktrees/export-joblog && just ci`
Expected: fmt-check, clippy, all tests, docs all pass.

- [ ] **Step 2: Fix any issues**

If clippy or fmt complains, fix and re-run.

- [ ] **Step 3: Push and update PR**

```bash
cd ~/github/jjjake/worktrees/export-joblog && git push
```

The PR at #301 (development tracker, not published) will be updated with the new commits.
