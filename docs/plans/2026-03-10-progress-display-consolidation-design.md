# Progress Display Consolidation

## Problem

Upload and download have inconsistent progress output:

- **Download**: `━╸─` bar chars, `▸ identifier` header, aggregate byte bar, rich summary
- **Upload**: `=> ` bar chars (ASCII), spinner prefix, per-file bars, no header, no summary

This drift happened because they were built independently with no shared style. Future commands (metadata batch writes, etc.) will need similar output, compounding the problem.

## Design

### Approach: Shared Style Constants + Helpers (Approach A)

Keep `DownloadDisplay`, `UploadDisplay`, `BatchDisplay` as separate structs with different behavioral logic, but have them all pull visual styling from shared constants and helpers in `output.rs`. This prevents visual drift without over-abstracting the behavioral differences.

### Three output tiers (unchanged)

1. **Default** — aggregate bar per item, clean and quiet
2. **`--json`** — one JSONL line per file (machine-readable)
3. **`--dashboard`** — full TUI with per-file detail

### Shared style elements

All in `output.rs`:

- **Bar style**: `━╸─` progress chars, width 40, consistent template
- **Icons**: `▸` (header/cyan), `✓` (success/green), `✗` (error/red), `–` (skipped/dim), `⊘` (dry-run/dim)
- **`make_progress_bar(total_bytes)`** — returns ProgressBar with shared style
- **`print_item_header(identifier)`** — prints `▸ identifier`
- **`format_speed(bytes, elapsed)`** — `· 5.2 MiB/s` or empty
- **`colored_count(count, color_fn)`** — colored if non-zero, "0" otherwise
- **`print_item_summary(...)`** — summary line for a completed item
- **`BatchSummary`** struct + **`print_batch_summary(...)`** — end-of-batch summary block

### ia-core change: `Enumerated` variant for upload

Add `Enumerated { files_count: usize, bytes_total: u64 }` to `UploadProgressStatus`, matching `DownloadStatus::Enumerated`. Emit from `upload_item` after file expansion and size computation (which already happens at step 8).

### Display behavior (consistent across upload/download)

**Single-item:**
```
▸ my-item
  ━━━━━━━━━━━━━━━━╸──────────────────────── 42.3 MiB/100.0 MiB 5.2 MiB/s  (3/15 files)

my-item  15 files (100.0 MiB) in 19.2s · 5.2 MiB/s
  ✓ 15 downloaded · 0 skipped · 0 errors
```

**Batch (multiple items):**
```
Uploading 3 items (2 workers)...
▸ item-one
  ━━━━━━━━━━━━━━━━╸──────────────────────── 42.3 MiB/100.0 MiB  (1/2 files)
▸ item-two
  ━━━━━━━╸─────────────────────────────────── 8.1 MiB/50.0 MiB  (0/1 files)
✓ item-one       2 files (100.0 MiB) 3s  33.3 MiB/s
✓ item-two       1 file (50.0 MiB) 10s  5.0 MiB/s
────────────────────────────────────────────────────
3/3 items (3 done · 0 skipped · 0 errors)
150.0 MiB uploaded · 10.0 MiB/s
13.0s elapsed
```

### Structs

- **`DownloadDisplay`** — refactored to use shared helpers, no behavioral change
- **`UploadDisplay`** — rewritten: aggregate bar, header, summary (matches download)
- **`BatchDisplay`** — simplified: drops per-file bars, one aggregate bar per item, shared formatting
- **`UploadBatchDisplay`** — new: multi-item upload progress driven by `UploadProgress` events; creates bars on `Enumerated`, auto-detects item completion from file count

### What doesn't change

- ia-core progress types (shapes unchanged, just new `Enumerated` variant for upload)
- TUI dashboards
- `--json` output
- Command-level logic structure in `commands/download.rs` and `commands/upload.rs`
