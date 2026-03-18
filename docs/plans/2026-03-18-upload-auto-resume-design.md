# Upload Auto-Resume Design

**Date:** 2026-03-18
**Status:** Draft

## Problem

When a batch upload with `--joblog` is interrupted (ctrl-c, network failure, etc.) and rerun, all files are uploaded again from scratch. The `--retry-failed` flag exists but only retries items with errors — it doesn't skip already-succeeded files, and it requires explicit opt-in. Users expect interrupted batch uploads to resume automatically.

## Solution

Make joblog-based auto-resume the **default behavior** when `--joblog` is provided. On startup, scan the joblog for successful `(item, file)` entries and skip them. Add `--no-resume` to opt out.

## Design Decisions

1. **Auto-resume is default** when `--joblog <path>` is present and the file exists
2. **File-level granularity** — skip individual files with successful entries, not whole items
3. **`--no-resume`** global flag to disable auto-resume (still writes to joblog)
4. **Remove `--retry-failed` from upload** — auto-resume subsumes it entirely
5. **Keep `--retry-failed` for other commands** (download, tasks, metadata) until they migrate
6. **No new `UploadOpts` field** — skip set is operational state, passed as a separate parameter

## Architecture

### Resume Data Flow

```
CLI (--joblog path exists, no --no-resume)
  │
  ▼
joblog::successful_files(entries) → HashSet<(String, String)>
  │
  ▼
Arc<HashSet<...>> shared across concurrent item uploads
  │
  ▼
upload_batch(skip_set) → upload_item(skip_set) → per-file check
  │
  ▼
(identifier, key) in skip_set? → UploadStatus::Resumed, continue
```

### Component Changes

#### `ia-core/src/joblog.rs` — New `successful_files()`

```rust
pub fn successful_files(entries: &[JoblogEntry], op: &str) -> HashSet<(String, String)>
```

Scans all entries **filtered by `op`** (e.g., `"upload"`), keeps latest status per `(item, file)` pair, returns the set where latest status is `"ok"`. Mirrors `failed_files()` structure. The `op` filter prevents cross-operation contamination — a download success for `(item, file)` must not cause that file to be skipped during upload.

**Performance:** Linear scan, single HashMap pass, then filter. For 1M entries: ~1-2s parse, ~100-200MB for the HashSet. O(1) lookups during upload. Acceptable for batch operations that run for hours.

#### `ia-core/src/upload/types.rs` — New `Resumed` Variants

```rust
pub enum UploadStatus {
    Uploaded,
    Skipped,     // --skip-existing (MD5 match against server)
    Resumed,     // NEW: joblog says this file already succeeded
    Failed(String),
    DryRun,
}

pub enum UploadProgressStatus {
    // ... existing variants ...
    Resumed,     // NEW
}
```

`Resumed` is semantically distinct from `Skipped`:
- `Skipped` = remote MD5 matches local (requires network, checked per-file in `single.rs`)
- `Resumed` = joblog has a success entry (local-only, checked before `upload_file()`)

#### `ia-core/src/upload/item.rs` — Skip Logic

`upload_item()` gains `skip_set: Option<&HashSet<(String, String)>>`.

In the file loop, before calling `upload_file()`:

```rust
if let Some(skip) = skip_set {
    if skip.contains(&(identifier.to_string(), key.clone())) {
        // Emit Resumed progress, push Resumed result, continue
    }
}
```

**`is_first` / `is_last` correctness:**

`is_first`: Already handled — `first_file_succeeded` stays false when files are skipped via `continue`, so the next actually-uploaded file correctly gets `is_first = true` and carries metadata headers.

`is_last`: Pre-compute the index of the last non-resumed file before the loop:

```rust
let last_upload_idx = if let Some(ref skip) = skip_set {
    expanded.iter().zip(keys.iter()).enumerate()
        .rev()
        .find(|(_, (_, key))| !skip.contains(&(identifier.to_string(), key.to_string())))
        .map(|(i, _)| i)
} else {
    if file_count > 0 { Some(file_count - 1) } else { None }
};

// In the loop:
let is_last = Some(i) == last_upload_idx;
```

If `last_upload_idx` is `None`, all files are resumed — skip the loop entirely (no uploads, no derive triggered).

`size_hint`: Currently sent on `i == 0`. If file 0 is resumed, size_hint is not sent. This is acceptable — the item already exists from the previous run, so the size hint (used for bucket creation) is moot. For clarity, send it on the first non-resumed file:

```rust
let hint = if !first_file_succeeded && size_hint.is_some() { size_hint } else { None };
```

**`Enumerated` event:** Reports **full** file counts and bytes (including resumed files). This is necessary because `UploadBatchDisplay` uses `files_total` from `Enumerated` to detect item completion, and `Resumed` progress events increment `files_processed`. If `Enumerated` only reported active files, `files_processed` would exceed `files_total`. The progress display handles resumed files like skipped files — they count toward completion without contributing upload bytes.

#### `ia-core/src/upload/batch.rs` — Pass-Through

`upload_batch()` gains `skip_set: Option<Arc<HashSet<(String, String)>>>`. `Arc` because the set is shared across concurrent item uploads via `buffer_unordered`. Passed to each `upload_item()` as `skip_set.as_deref()`.

#### `ia-cli/src/main.rs` — `--no-resume` Global Flag

```rust
#[arg(long, global = true)]
no_resume: bool,
```

Passed to upload command alongside `joblog_path`.

#### `ia-cli/src/commands/upload.rs` — Wiring

**Build skip set** (in both `run()` and `run_import()`):
1. If `--no-resume` is set → `None`
2. If no `--joblog` → `None`
3. If joblog file doesn't exist → `None` (fresh upload)
4. Otherwise → read joblog, call `successful_files()`, wrap in `Arc`

**Startup message** (when resuming):
```
▸ Resuming: 42 files already uploaded (from upload.jsonl)
```

**`--retry-failed` deprecation for upload:**
```
Error: --retry-failed is no longer needed for uploads.
Resume is automatic when --joblog is provided.
```

**Resumed files are NOT written to the joblog** — they already have a success entry. This prevents unbounded log growth on repeated runs.

**Single-item uploads** (`ia upload <id> <files> --joblog`) also get auto-resume — same skip set passed to `upload_item()`.

#### `ia-cli/src/output.rs` — Display Updates

- `UploadDisplay` and `UploadBatchDisplay` handle `UploadProgressStatus::Resumed`
- Summary includes resumed count: `"42 uploaded, 7 resumed, 1 failed"`
- Batch display shows: `"⠙ Uploading 3/10 items (7 resumed)"`

## Known Limitations

**Cross-batch contamination:** If two different spreadsheets share the same `--joblog` file and have overlapping `(item, key)` pairs with different local content, auto-resume will skip the second upload because the joblog already has a success entry for that `(item, key)`. The local file content is not checked — only the `(item, key)` pair. Users can use `--no-resume` to force re-upload, or use separate joblog files per batch. This is acceptable for v1; a future enhancement could store MD5 in joblog entries and compare during resume.

## `--no-resume` Scope

`--no-resume` is a global flag despite currently only affecting upload. Rationale: download, tasks, and metadata will migrate to auto-resume in future work (see migration plan below). Making it global now avoids a breaking change later. Until those commands support auto-resume, `--no-resume` is a no-op for them.

## Exhaustive Match Sites for `Resumed`

Adding `Resumed` to `UploadStatus` will cause compile errors in all incomplete matches. Known sites that need updating:

| File | Function/Location | Change |
|------|-------------------|--------|
| `ia-cli/src/commands/upload.rs` | `write_upload_result()` | `Resumed => return` (don't write to joblog) |
| `ia-cli/src/commands/upload.rs` | `summarize_results()` | Count resumed files |
| `ia-cli/src/commands/upload.rs` | `print_result_line()` | Print resumed status line |
| `ia-cli/src/commands/upload.rs` | `output_results()` | Handle JSON output for resumed |
| `ia-cli/src/output.rs` | `UploadBatchDisplay::finish()` | Count resumed in summary |
| `ia-cli/src/output.rs` | `UploadDisplay` progress handler | Handle `Resumed` progress event |
| `ia-cli/src/tui/upload_app.rs` | TUI state update | Handle `Resumed` progress event |

The compiler will catch any missed sites since `UploadStatus` is not `#[non_exhaustive]`.

## `--skip-existing` Interaction

Auto-resume and `--skip-existing` are complementary:
- **Auto-resume**: local check (joblog), fast, no network
- **`--skip-existing`**: remote check (MD5 vs server metadata), requires network

A file passes resume (no joblog entry) but may still be skipped by `--skip-existing` (already on server via another method). Resume check happens first (item.rs loop), then `--skip-existing` runs inside `upload_file()` for non-resumed files. No conflict.

## `--retry-failed` Migration Plan

Upload removes `--retry-failed` support immediately. Other commands keep it for now:
- `ia download` — uses `--retry-failed` at item level
- `ia tasks submit` — uses `--retry-failed` at item level
- `ia metadata` — uses `--retry-failed` at item level

GitHub issues will track migrating each command to auto-resume and eventually removing `--retry-failed` globally.

## Test Strategy

### Unit Tests (`ia-core`)

**`joblog.rs`** (5 tests):
- `successful_files_empty` — empty input → empty set
- `successful_files_basic` — mixed statuses → only "ok" pairs
- `successful_files_latest_wins` — fail then succeed → included; succeed then fail → excluded
- `successful_files_skipped_not_included` — "skipped" status not in set
- `successful_files_filters_by_op` — download success entries ignored when `op="upload"`

**`upload/types.rs`** (1 test):
- Serde serialization of `Resumed` variant

**`upload/item.rs`** (4 tests, wiremock):
- `upload_item_skips_resumed_files` — files in skip set produce `Resumed` results
- `upload_item_is_first_after_resume` — metadata headers on first non-resumed file
- `upload_item_is_last_correct_with_resume` — derive on last non-resumed file
- `upload_item_all_resumed` — all files in skip set → no HTTP calls, no derive triggered
- `upload_item_different_key_not_resumed` — joblog has `(item, old.txt)`, upload has `(item, new.txt)` → uploads normally
- `upload_item_same_key_different_item` — joblog has `(item-1, file.txt)`, upload has `(item-2, file.txt)` → uploads normally

### Integration Tests (`ia-cli`)

- `upload_retry_failed_error` — `--retry-failed` with upload prints error
- `upload_no_resume_flag` — `--no-resume` accepted
- `upload_auto_resume_message` — startup message when joblog has successes

### Manual Verification

1. Batch upload 5 items (3 files each), interrupt after 8 files
2. Rerun → 8 resumed, 7 uploaded
3. Rerun with `--no-resume` → all 15 uploaded fresh
4. Verify joblog has no "resumed" entries
5. Check `--json` output

## Files Modified

| File | Change |
|------|--------|
| `ia-core/src/joblog.rs` | Add `successful_files()` + tests |
| `ia-core/src/upload/types.rs` | Add `Resumed` to both enums |
| `ia-core/src/upload/item.rs` | Add `skip_set` param, skip logic, is_first/is_last fix |
| `ia-core/src/upload/batch.rs` | Add `skip_set` param (Arc), pass through |
| `ia-cli/src/main.rs` | Add `--no-resume` global flag |
| `ia-cli/src/commands/upload.rs` | Wire skip set, remove retry-failed for upload |
| `ia-cli/src/output.rs` | Handle Resumed in displays |
| `ia-cli/src/tui/upload_app.rs` | Pass `None` for skip_set in TUI calls |
| `ia-cli/tests/upload.rs` | Integration tests |
| `docs/usage.md` | Document auto-resume |
