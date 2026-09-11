# Download Output Redesign

**Issue:** [#27](https://github.com/internetarchivecanada/ia/issues/27)
**Date:** 2026-02-20

## Problem

The `--tui` flag is developer jargon. The download experience should be rich and informative by default. The full-screen dashboard should be available without rebuilding from source.

## Goals

1. Rich inline output by default (TTY-attached), no flag needed
2. Rename `--tui` to `--dashboard`, make it a default feature (always ships)
3. Show useful stats: speeds, totals, skip/error counts, disk space
4. Dashboard is the deep-dive experience, primarily for batch downloads

## Inline Output (Default)

The inline output is the unified default for single-item and batch downloads. No flags needed.

### Single Item

```
ia download nasa-images

nasa-images  Resolving...
────────────────────────────────────────────────────
  file1.zip  ━━━━━━━━╸───  125MB/512MB  8.1MB/s
  file2.iso  ━━━━━━━━━━━━  89MB/89MB    done
  meta.xml                               skipped (size match)
────────────────────────────────────────────────────
nasa-images  42 files (1.2GB) in 38s · 31.6MB/s
  ✓ 39 downloaded · 3 skipped · 0 errors
  /data: 450GB free
```

### Batch

```
ia download --itemlist items.txt

Downloading 5 items (8 workers)...
────────────────────────────────────────────────────
✓ nasa-images       42 files (1.2GB) 38s  31.6MB/s
  ─ 3 skipped (size+mtime match)
▸ old-newspapers    [17/89 files]
  page-045.jp2  ━━━━━━━━╸───  125MB/512MB  8.1MB/s
  page-046.jp2  ━━━╸────────   45MB/512MB  6.2MB/s
▸ gov-docs          [3/24 files]
  report.pdf    ━━━━━━╸─────   89MB/180MB  4.3MB/s
  2 items remaining...
────────────────────────────────────────────────────
3/5 items (3 done · 0 skipped · 0 errors)
2.1GB downloaded · 31.6MB/s
/data1: 450GB free │ /data2: 1.2TB free
1m 12s elapsed
```

### Colors

- Identifiers: bold
- Progress bars: cyan (`━╸─`)
- Done/success: green
- Skipped: yellow
- Errors: red
- Separator lines: dim dashes (uv-style)
- Speeds/metadata: dim

### Quiet Levels

- `-q`: per-item one-line summaries, no per-file bars
- `-qq`: final summary only
- Pipe/redirect: no progress bars, no colors, plain text status lines

## Dashboard (`--dashboard`)

Full-screen ratatui interface. Works for both single and batch, but primarily designed for batch workflows.

### Layout

```
┌─ ia download ─ 45% ─ 2.1GB ─ 31.6MB/s ─ ETA 2m 30s ──────────────┐
│ ██████████████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │
├─ Items ────────────────────────────────────────────────────────────┤
│ ✓ nasa-images       42 files  1.2GB   38s   31.6MB/s              │
│ ✓ archive-texts     12 files  340MB   15s   22.7MB/s              │
│ ▸ old-newspapers    17/89     450MB   1m    12.3MB/s              │
│ ▸ gov-docs           3/24      89MB   22s    8.1MB/s              │
│   pending-item      —         —       —      —                    │
├─ Workers (8) ─────────────────────────────────────────────────────┤
│ ▸ page-045.jp2      [████████░░░░] 40%   1.2MB/s                 │
│ ▸ page-046.jp2      [██░░░░░░░░░░]  8%   0.8MB/s                 │
│ ▸ report.pdf        [██████░░░░░░] 50%   4.3MB/s                 │
├─ Disks ───────────────────────────────────────────────────────────┤
│ /data1: 450GB free (3 items)  │  /data2: 1.2TB free (2 items)     │
├─ Errors ──────────────────────────────────────────────────────────┤
│ (none)                                                            │
├─ Throughput ──────────────────────────────────────────────────────┤
│ ▁▂▃▅▇█▇▅▆▇█▇▅▃▂▃▅▇█  (last 60s)                                 │
├───────────────────────────────────────────────────────────────────┤
│ 3/5 done · 0 skip · 0 err │ 2.1GB │ 1m 12s │ [j/k] [q]uit       │
└───────────────────────────────────────────────────────────────────┘
```

### Panels

1. **Header** — overall progress gauge, total bytes, speed, ETA
2. **Items** — scrollable table of all items with status, files, bytes, speed (hidden for single-item)
3. **Workers** — active file downloads with per-file progress bars and speeds
4. **Disks** — per-disk free space and item count (only shown if disk pool active)
5. **Errors** — scrollable log of failures with file name and error (only shown if errors exist)
6. **Throughput** — sparkline graph of speed over last 60 seconds
7. **Status bar** — aggregate counts, elapsed, keyboard shortcuts

### New State Tracking

- `ItemState` struct for per-item tracking in batch mode (status, files done/total, bytes, speed)
- Throughput history ring buffer (1 sample/sec, 60 entries) for sparkline
- ETA calculation: bytes remaining / rolling average speed (last 10s window)
- Per-disk stats from `DiskPool::status()`

## Implementation Changes

### ia-cli/Cargo.toml
- `default = ["tui"]` — dashboard always ships with release binaries

### ia-cli/src/commands/download.rs
- Rename `--tui` flag to `--dashboard`
- Remove `#[cfg(not(feature = "tui"))]` error path
- Improve inline output for single and batch modes
- Add disk space reporting to single-item summary
- Add overall speed to summaries

### ia-cli/src/output.rs
- Batch-aware `DownloadDisplay` that tracks multiple items
- Separator line rendering (dim dashes)
- Per-item summary formatting with speeds
- Disk space query (`statvfs` on destdir)
- Overall throughput tracking (bytes / elapsed)

### ia-cli/src/tui/app.rs
- `ItemState` struct for per-item tracking
- Throughput history ring buffer
- ETA calculation
- Accept batch downloads (vec of identifiers, not single)

### ia-cli/src/tui/ui.rs
- Items panel, Disks panel, Errors panel, Throughput sparkline
- Adaptive layout: single vs batch (hide Items panel for single)
- Conditional panels: Disks (only with pool), Errors (only when errors exist)

### ia-core
No changes needed. The callback-based progress API already provides everything required.
