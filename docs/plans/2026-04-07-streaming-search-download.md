# Streaming Search-to-Download Pipeline

**Date:** 2026-04-07
**Status:** Design

## Context

When `ia download --search <query>` is used on a large collection (e.g. 125k items), the CLI currently awaits the entire search stream into a `Vec<String>` before any downloads begin. This creates a 1-2 minute delay with no visible progress. The search API already returns a paginated stream — we should pipe identifiers directly into the download pipeline as they arrive.

## Goal

Start downloading items as search results arrive. Search pagination and downloads should overlap. The first download should begin within ~1 second of invocation.

## Design

### Approach: Stream-based batch download in the CLI layer

The key insight is that `ia_core::search::scrape()` already returns `Pin<Box<dyn Stream<Item = Result<SearchResult>>>>`. Currently, `collect_identifiers()` drains this stream into a Vec. Instead, we map the stream to extract identifiers and feed it directly into `buffer_unordered()` — the same pattern already used by `download_batch` and `download_batch_with_pool`.

All changes are in **ia-cli**. The ia-core library is untouched.

### Estimated total via `num_found()`

Before streaming begins, call `search::num_found()` (a cheap `total_only=true` request, ~200ms). This gives an estimated total for progress display. The estimate may drift slightly from the actual count (items added/removed during pagination), but it's good enough for the header and progress bar. The final batch summary uses actual counts.

### Architecture

```
num_found() ──→ estimated_total ──→ BatchDisplay::new(estimated_total, jobs)
                                         │
scrape() stream ──→ .map(|r| r.identifier) ──→ .map(|id| async { download_item(id) })
                                                    │
                                              buffer_unordered(items_concurrency)
                                                    │
                                              collect() → BatchDownloadResult
```

### What changes, what doesn't

| Component | Changes? | Notes |
|-----------|----------|-------|
| `collect_identifiers()` | No | Still used for `--itemlist`, stdin, positional args |
| `run()` | Yes | New early branch for `--search` path |
| `download_batch_streaming()` | New | Single-destdir streaming batch |
| `download_batch_with_pool_streaming()` | New | Multi-destdir streaming batch |
| `BatchDisplay` | No | Already works with estimated total |
| `download_batch()` (ia-core) | No | Unchanged |
| `scrape()` / `num_found()` (ia-core) | No | Already exist |
| TUI dashboard | No | See "Dashboard" below |
| Checksum/resume | No | Per-file, unaffected |
| DiskPool | No | `assign_item()` already incremental |
| Joblog | No | Written after batch completes |

## Implementation Plan

### Step 1: New streaming batch functions

Add two new functions to `ia-cli/src/commands/download.rs`:

**`download_batch_streaming()`** — mirrors the existing single-destdir batch path (lines 418-471) but takes a `Pin<Box<dyn Stream<Item = Result<String>> + Send>>` instead of `Vec<String>`. Uses `identifier_stream.map(|id| async { ... }).buffer_unordered(items_concurrency).collect()`. Passes `item_results.len()` as `items_total` to `collect_batch_results()` since the actual count isn't known until the stream is exhausted.

**`download_batch_with_pool_streaming()`** — mirrors `download_batch_with_pool()` (lines 539-725) but takes the same stream type. Same pattern: per-item metadata fetch → disk assignment → download, with disk-full failover.

Both functions accept `estimated_total: usize` for the progress display header.

### Step 2: Branch `run()` on `--search`

In `run()`, after validation and setup (destdir, filter, disk pool, etc.) but before the current `identifiers.len() == 1` check, add a branch:

```rust
if let Some(ref query) = args.search {
    // Fast estimated count for progress display
    let estimated_total = search::num_found(client, query, &[]).await.unwrap_or(0) as usize;

    // Build identifier stream from search results
    let opts = SearchOpts::default();
    let id_stream = search::scrape(client, query, &opts)
        .map(|r| r.map(|item| item.identifier));

    // Always use batch path for --search (never single-item path,
    // because we don't know the count without consuming the stream)
    let result = if let Some(pool) = disk_pool.take() {
        let (batch_result, returned_pool) = download_batch_with_pool_streaming(
            client, Box::pin(id_stream), make_opts, filter, pool,
            semaphore, batch_display, json_mode, estimated_total, items_concurrency,
        ).await?;
        disk_pool = Some(returned_pool);
        batch_result
    } else {
        download_batch_streaming(
            client, Box::pin(id_stream), opts, semaphore,
            batch_display, json_mode, estimated_total, items_concurrency,
        ).await
    };

    // ... same result handling as existing batch path (joblog, summary, exit code)
}
```

The existing `collect_identifiers()` path continues to handle `--itemlist`, stdin, and positional identifiers unchanged.

### Step 3: Handle `--retry-failed` interaction

`--retry-failed` reads a joblog and replaces identifiers. This doesn't interact with `--search` streaming — if `--retry-failed` is set, the code already replaces identifiers from the joblog, so the `--search` branch is never reached. No change needed.

### Step 4: Progress display with estimated total

`BatchDisplay::new(estimated_total, jobs)` already works — it shows "Downloading ~N items (M workers)" in the header. The `~` prefix signals the count is an estimate. If `num_found()` fails (unlikely), fall back to 0 and show "Downloading items (M workers)".

The `on_item_start()` callback receives `(identifier, current, total)` but only uses `identifier` for display. The counter is managed by an AtomicUsize in the streaming functions.

The final `BatchSummary` uses actual counts from `BatchDownloadResult`.

### Step 5: Dashboard (`--dashboard --search`)

The TUI dashboard pre-creates `ItemState` for every identifier in `TuiState::new(&identifiers)` and uses `items.len()` as the progress denominator. Making it support dynamic item addition would require significant refactoring.

For now, `--dashboard --search` keeps the collect-first behavior. This is acceptable because:
- Dashboard is a niche interactive feature
- Users who care about streaming are typically running headless batch jobs
- The TUI will gain streaming support in a future iteration if needed

Implementation: the `--dashboard` check already happens before the batch path. We add the `--search` streaming branch *after* the dashboard check, so dashboard still sees the collected Vec.

### Step 6: Search stream error handling

If `scrape()` yields an `Err` mid-stream, it becomes a failed item in the batch results. The stream adapter maps `Err` from search into the download pipeline where it's caught and recorded. This matches the current behavior where a search error is fatal.

Specifically: the `.map()` on the identifier stream propagates `Result<String>`. Inside the `buffer_unordered` closure, an `Err` identifier is converted to a failed item result `Err((query_context, error))`.

### Step 7: Tests

Add to `ia-cli/tests/download_search_list.rs`:

1. **`download_search_streams_to_downloads`** — Mock a multi-page scrape response (page 1 returns cursor, page 2 returns remaining items). Verify downloads complete. This is the core integration test.

2. **`download_search_num_found_failure_still_works`** — Mock `num_found` returning 500, verify downloads still proceed (graceful degradation).

3. **`download_search_with_disk_pool_streaming`** — Like the existing `download_batch_with_multiple_destdirs` test but verifies streaming path is used.

## Files to Modify

- `ia-cli/src/commands/download.rs` — New streaming functions, branch in `run()`
- `ia-cli/tests/download_search_list.rs` — New integration tests

## Files to Read (reference only, not modified)

- `ia-core/src/search.rs` — `scrape()`, `num_found()`, `SearchOpts`
- `ia-core/src/download.rs` — `download_item()`, `collect_batch_results()`, callback types
- `ia-cli/src/output.rs` — `BatchDisplay`, `print_batch_summary()`

## Verification

1. `cargo test -p ia-cli` — all existing + new tests pass
2. `just ci` — fmt, clippy, test, doc all green
3. Manual test: `ia download --search 'collection:some-large-collection' --dry-run` — should show items appearing immediately, not after a long pause
4. Manual test: `ia download --search 'collection:small-collection' --destdir /tmp/test` — verify files download correctly
5. Manual test with `--dashboard --search` — verify it still works (collect-first behavior)

## Performance Impact

- **Before:** 1-2 min delay for 125k-item search, then downloads begin
- **After:** ~200ms for `num_found()`, first download starts within ~1s (first scrape page), search pagination and downloads overlap completely
