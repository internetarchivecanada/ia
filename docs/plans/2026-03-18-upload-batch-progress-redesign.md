# Upload Batch Progress Redesign

## Problem

In batch upload mode, the aggregate progress bar tracks `bytes uploaded / bytes total`.
But `bytes_total` grows as each item is enumerated (file sizes aren't known until the
item's files are listed). This makes the progress bar denominator keep increasing —
progress appears to jump backward every time a new item starts, which is confusing.

## Design

Replace the batch-level byte progress bar with a **spinner + item counter + cumulative
byte counter**. Per-item progress bars (which have known totals) are unchanged.

### Before

```
  ━━━━━━━━━━━━━━━━╸───────────────────────── 1.2 GB/4.8 GB  12.5 MB/s
```

### After

```
⠙ Uploading  3/10 items  4.2 GB uploaded  12.5 MB/s
```

### Components

| Element | Source | Notes |
|---------|--------|-------|
| `⠙` | indicatif spinner | Ticks at 80ms, gives "alive" feel |
| `3/10 items` | `items_completed / items_total` | Known upfront from spreadsheet/batch input |
| `4.2 GB uploaded` | Cumulative `bytes_uploaded` | No denominator — just grows |
| `12.5 MB/s` | Aggregate throughput | Dimmed, computed from `bytes_uploaded / elapsed` |

On finish: spinner becomes `✓` (green) or `✗` (red if any failures), then
`print_batch_summary()` prints the final stats as it does today.

### What stays the same

- Per-item `▸ identifier` headers and `━╸─` byte progress bars (totals known per-item)
- `UploadProgress` event types in `ia-core` — no changes
- TUI dashboard — no changes (separate code path)
- `print_batch_summary()` — no changes
- Bottom sentinel pattern — no changes

## Implementation

### File: `ia-cli/src/output.rs`

**Struct changes:**

```rust
pub struct UploadBatchDisplay {
    multi: MultiProgress,
    batch_header: ProgressBar,          // now a spinner, not a bar
    bottom_sentinel: ProgressBar,
    active_items: Mutex<HashMap<String, UploadItemBars>>,
    items_total: usize,
    items_completed: AtomicUsize,       // NEW
    bytes_uploaded: AtomicU64,          // NEW
    started_at: Instant,               // NEW (for throughput calc)
}
```

**New imports (top of file):**

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
```

(`Instant` and `Duration` are already imported.)

**Constructor (`new`):**

- Replace `ProgressBar::new(0)` with `ProgressBar::new_spinner()`
- Use `retry_mode` to pick verb: `let verb = if retry_mode { "Retrying" } else { "Uploading" };`
- Template: `"{spinner:.cyan} {prefix}  {msg}"` with `batch_header.set_prefix(verb)`
- `enable_steady_tick(Duration::from_millis(80))`
- Initial message: `"0/{items_total} items  0 B uploaded"`
- Initialize `items_completed: AtomicUsize::new(0)`, `bytes_uploaded: AtomicU64::new(0)`,
  `started_at: Instant::now()`

**Event handling (`update`):**

- `Enumerated` — remove `batch_header.set_length()` call. Per-item bar creation unchanged.
- `Uploading` — after computing `delta` for per-item bar, also
  `self.bytes_uploaded.fetch_add(delta, Ordering::Relaxed)`. Call `self.refresh_header()`.
- `Complete` — compute final byte delta (last `Uploading` tick → `p.total_bytes`) and
  `self.bytes_uploaded.fetch_add(delta, Ordering::Relaxed)` before checking item completion.
  This accounts for the tail bytes between the last progress tick and actual file completion.
- `Skipped`/`Failed` — no byte accounting (skipped files weren't uploaded; failed files'
  partial bytes are already counted via `Uploading` events).
- In `maybe_finish_item()`, right after `items.remove()`,
  `self.items_completed.fetch_add(1, Ordering::Relaxed)` and `self.refresh_header()`.

**Note on skipped files:** Skipped files don't contribute to `bytes_uploaded`. This is
intentional — they weren't transferred. Consistent with `finish()` which only counts
`Uploaded` and `DryRun` bytes in the summary.

**New helper:**

```rust
fn refresh_header(&self) {
    let completed = self.items_completed.load(Ordering::Relaxed);
    let bytes = self.bytes_uploaded.load(Ordering::Relaxed);
    let elapsed = self.started_at.elapsed().as_secs_f64();
    let speed = if elapsed > 0.0 {
        format!("  {}/s", format_bytes((bytes as f64 / elapsed) as u64))
    } else {
        String::new()
    };
    self.batch_header.set_message(format!(
        "{}/{} items  {} uploaded{}",
        completed, self.items_total, format_bytes(bytes), speed
    ));
}
```

**Note on throughput:** This uses cumulative average (`total_bytes / total_elapsed`),
not a windowed/EWMA approach like indicatif's `{bytes_per_sec}`. The per-item bars
still use indicatif's EWMA for real-time speed. The batch-level speed is a coarser
signal — acceptable for v1, can be refined later if needed.

**Finish:**

- `batch_header.finish_and_clear()` — unchanged
- `print_batch_summary()` — unchanged

### No other files modified

- `ia-core` — no changes (events, batch logic untouched)
- `tui/upload_app.rs`, `tui/upload_ui.rs` — no changes (separate display path)
- `commands/upload.rs` — no changes (same `UploadBatchDisplay::new()` API)

## Verification

1. `cargo check` — compiles
2. `cargo test` — all 1,087+ tests pass
3. `cargo clippy -- -D warnings` — no warnings
4. `just ci` — full CI suite passes
5. Manual test: `ia upload import <spreadsheet>` with multiple items, confirm:
   - Spinner animates
   - Item counter increments as items complete
   - Byte counter grows monotonically
   - Per-item bars show byte progress as before
   - Speed is reasonable
   - Final summary prints correctly
