# Metadata Export Progress & Summary Display

**Date:** 2026-04-03
**Status:** Approved
**Branch:** fix/export-joblog

## Problem

`ia metadata export` shows only a bare `\rFetched N/M items` counter during
operation and minimal summary stats afterward. For large exports (125K+ items),
this provides poor visibility into progress, errors, and throughput — and is
inconsistent with the polished output of `download`, `upload`, and other batch
commands.

## Design

### Progress Bar (during operation)

Use an `indicatif::ProgressBar` with the existing design vocabulary from
`output.rs`:

- **Bar style**: `PROGRESS_CHARS` (`"━╸─"`), `cyan/dim` colors
- **Template**: `{msg}\n  {bar:28.cyan/dim} {pos}/{len} {per_sec} ({elapsed} elapsed)`
- **Message line**: `"Exporting metadata..."` (or resume/retry variant)
- **Rate**: items/s (not bytes/s — items are the meaningful unit here)

The bar is created with `total = identifiers.len()` (full list including
already-exported). For resume, the bar position is immediately set to the
skip count so it shows overall progress.

### Header Messages by Scenario

| Scenario | Header message |
|----------|---------------|
| Normal | `Exporting metadata...` |
| Resume | `Exporting metadata... (resuming — {N} already exported)` |
| Retry | `Retrying {N} failed item(s) from joblog...` |

### Inline Error Display

Errors are printed as they occur, up to a maximum of **5**:

```
✗ identifier-name — error message
```

After 5 errors, subsequent errors are silently counted. At the end:

```
  ... {N} more errors (see joblog)
```

This uses `ICON_ERROR` (✗) in red, identifier in bold, error message in red —
matching existing error display patterns.

### Summary (after completion)

Printed after the progress bar finishes, using the existing separator pattern:

```
────────────────────────────────────────────────────
```

**Clean run (no errors):**
```
✓ 125,061 items exported
487.3 MB fetched · 42.1 items/s · 49m 28s elapsed
exported to x.jsonl
```

**With errors:**
```
124,999/125,061 items exported · 62 failed
487.3 MB fetched · 42.1 items/s · 49m 28s elapsed
exported to x.jsonl
warning: 62 item(s) failed — re-run with --retry-failed to retry
```

Color conventions (matching existing patterns):
- Success count: green
- Failed count: red (only shown when > 0)
- Bytes/speed/elapsed: dim
- `✓` icon: green (clean run only)
- `warning:` prefix: yellow bold
- Output filename: bold

**Stdout mode** omits the "exported to" line (data went to stdout).

### Quiet Levels

- `-q` (quiet=1): No progress bar, no inline errors. Summary still shown.
- `-qq` (quiet=2): No output at all.
- Default (quiet=0): Full progress bar + inline errors + summary.

### Tracking State

New fields tracked during the export loop:

- `succeeded: AtomicUsize` — items fetched successfully
- `failed: AtomicUsize` — items that errored
- `bytes_total: AtomicU64` — total bytes fetched (JSON response sizes)
- `errors_shown: AtomicUsize` — inline errors printed so far (cap at 5)
- `overflow_errors: AtomicUsize` — errors beyond the display cap
- `start: Instant` — for elapsed time and items/s calculation

### Implementation Scope

All changes are in `ia-cli/src/commands/metadata.rs` (`run_export()`) and
`ia-cli/src/output.rs` (make constants public). No changes to `ia-core`.

The export summary is simpler than download/upload (no per-file tracking, no
disk pool) so it doesn't warrant reuse of `BatchSummary`/`print_batch_summary`.
It uses the same icons and color conventions by importing the now-public
constants from `output.rs`: `ICON_SUCCESS`, `ICON_ERROR`, `PROGRESS_CHARS`,
`BAR_WIDTH`.

### What This Does NOT Include

- TUI dashboard mode (export is read-only metadata fetching, not worth it)
- Per-item success lines (noise at 125K scale)
- Retry logic within the export (handled by `--retry-failed` re-runs)
