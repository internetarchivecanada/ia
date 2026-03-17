# Download Command Interface Analysis

**Analyst**: download-analyst
**Date**: 2026-03-16
**Task**: #4 — Audit download command batch interface

---

## 1. Positional Arguments

| Arg | Type | Required | Notes |
|-----|------|----------|-------|
| `identifier` | `String` | Optional | Optional when using `--itemlist`, `--search`, or stdin |
| `files` | `Vec<String>` | Optional | File names to download from single item; incompatible with batch input |

**Code references (ia-cli/src/commands/download.rs):**
- `identifier` definition: line 41
- `files` definition: line 44
- Validation (files require identifier): lines 149-151
- Validation (incompatible with `--itemlist`): lines 154-155
- Validation (incompatible with `--search`): lines 168-169

**Key constraint**: File names (positional `<file...>`) only work with a **single identifier**. They cannot be combined with `--itemlist`, `--search`, or piped stdin.

---

## 2. Named Flags/Options (Complete Reference)

### File Selection

| Short | Long | Type | Default | Required | Notes |
|-------|------|------|---------|----------|-------|
| `-g` | `--glob` | `String` | - | No | Glob pattern for file filtering (pipe-separated: `"*.mp4\|*.webm"`) |
| `-e` | `--exclude` | `String` | - | No | Exclude pattern (glob) |
| `-f` | `--format` | `Vec<String>` | Empty | No | Filter by file format (repeatable: `-f mp4 -f webm`) |
| - | `--source` | `FileSource` | - | No | Filter by source: `original`, `derivative`, or `metadata` |
| - | `--exclude-source` | `FileSource` | - | No | Exclude by source type |

**Parser for source types** (lines 111-120):
- Converts strings to `FileSource` enum via `parse_source()`
- Valid values: `original`, `derivative`, `metadata` (case-insensitive)

### Batch Input

| Short | Long | Type | Default | Required | Notes |
|-------|------|------|---------|----------|-------|
| - | `--itemlist` | `PathBuf` | - | No | File containing identifiers (one per line, supports plain text or JSONL) |
| `-s` | `--search` | `String` | - | No | Download items matching search query (scrapes all results) |

**Identifier parsing** (lines 127-143):
- Plain identifiers: `my-item-id`
- JSONL format: extracts `identifier` field from JSON objects
- Empty lines and lines starting with `#` are ignored
- Falls back to raw line if JSON lacks `identifier` field

### Download Options

| Short | Long | Type | Default | Required | Notes |
|-------|------|------|---------|----------|-------|
| - | `--destdir` | `Vec<PathBuf>` | `.` | No | Destination directory (repeatable for disk pool) |
| - | `--no-directories` | Bool | false | No | Don't create item subdirectory |
| `-C` | `--checksum` | Bool | false | No | Verify checksums (slower, reads every local file) |
| `-R` | `--retries` | `usize` | `5` | No | Max retries per file |
| - | `--no-timestamps` | Bool | false | No | Don't set file modification times |
| - | `--dry-run` | Bool | false | No | Show what would be downloaded without downloading |

### Batch/Performance

| Short | Long | Type | Default | Required | Notes |
|-------|------|------|---------|----------|-------|
| - | `--items` | `usize` | `2` | No | Concurrent items for batch/search (use `-j/--jobs` for concurrent files) |

**Comment (line 98):**
```
/// Concurrent items for batch/search (use -j/--jobs for concurrent files)
```

### Output/Display

| Short | Long | Type | Default | Required | Notes |
|-------|------|------|---------|----------|-------|
| - | `--dashboard` | Bool | false | No | Full-screen dashboard mode (feature-gated: requires `tui` feature) |
| - | `--json` | Bool | false | No | Output results as JSON (one object per line) |

**Mutual exclusivity** (lines 206-208):
- `--json` and `--dashboard` are mutually exclusive
- Error if both provided

---

## 3. Global Options (Passed from main.rs)

These are applied to **all** commands and handled by the `run()` function:

| Flag | Type | Default | Notes |
|------|------|---------|-------|
| `-q, --quiet` | `u8` (counter) | 0 | `-q` suppresses summary; `-qq` suppresses all output |
| `-j, --jobs` | `usize` | 2 | **File download** concurrency (default 2) |
| `--joblog` | `PathBuf` | - | Path to append-only job log |
| `--retry-failed` | Bool | false | Re-download failed items from previous joblog |

**Function signature** (lines 198-205):
```rust
pub async fn run(
    client: &IaClient,
    args: DownloadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()>
```

---

## 4. Stdin Behavior

### Detection Logic

Stdin is **automatically detected and consumed** when **ALL** of these conditions are true:

1. No `--identifier` argument provided
2. No `--itemlist` file provided
3. No `--search` query provided
4. stdin is NOT a terminal (`!is_terminal()`)

**Code** (lines 179-193):
```rust
if ids.is_empty()
    && args.itemlist.is_none()
    && args.search.is_none()
    && !std::io::stdin().is_terminal()
{
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read from stdin")?;
        if let Some(id) = parse_identifier_line(&line) {
            ids.push(id);
        }
    }
}
```

### Input Format Support

The `parse_identifier_line()` function (lines 127-143) supports:

1. **Plain text identifiers**: One per line
   ```
   nasa
   cubanc_000418
   ```

2. **JSONL from `ia search --json`**: Extracts `identifier` field
   ```json
   {"identifier": "nasa", "title": "NASA Images", ...}
   ```

3. **Empty lines and comments**: Ignored
   ```
   # This is a comment

   my-item-id
   ```

4. **Fallback behavior**: If JSON object lacks `identifier`, use raw line as-is
   ```json
   {"title": "something"}  # Treated as literal identifier: "{\"title\": \"something\"}"
   ```

---

## 5. Batch Input Patterns

Download supports **four ways** to specify multiple identifiers:

### 1. CLI Positional (Single Item)
```bash
ia download identifier [file1] [file2] ...
```
- Only one item per invocation
- Can specify individual files
- Not true batch mode

### 2. --itemlist File
```bash
ia download --itemlist ids.txt
```
- One identifier per line
- Supports plain text or JSONL
- Cannot combine with positional file names (error at line 154-155)
- Cannot combine with `--search` (conflict checked implicitly)

### 3. --search Query
```bash
ia download --search "collection:nasa AND mediatype:movies"
```
- Dynamically scrapes search results
- Uses `ia_core::search::scrape()` (line 172)
- Cannot combine with file names (error at line 168-169)

### 4. Stdin Piping
```bash
ia search -q collection:nasa --json | ia download
```
- Auto-detected (no flag required)
- Parses both plain text and JSONL identifiers
- Only triggered if no other source provided and stdin is not a tty

### Source Priority
Order of evaluation (lines 147-196):

1. Positional `--identifier` argument
2. `--itemlist` file
3. `--search` query
4. Stdin (if not a tty and no other source)

**Key constraint**: File names (positional `<file...>`) are **only valid with a single identifier**. They cause errors with `--itemlist` or `--search`.

---

## 6. Output Formats

### Display Modes

| Mode | Output | Condition | Summary |
|------|--------|-----------|---------|
| **Default (human)** | Animated progress bars | `--json` absent, `quiet == 0` | Shows per-file progress + final summary |
| **Quiet L1** | Silent summary | `--quiet=1` or `-q` | Shows progress bars only; suppresses final summary |
| **Quiet L2** | Silent | `--quiet=2` or `-qq` | Suppresses all output except errors |
| **JSON** | Newline-delimited JSON | `--json` flag | One JSON object per file (single-item) or per item (batch) |
| **Dashboard** | Full-screen TUI | `--dashboard` flag | Animated dashboard (requires `tui` feature) |

### Single-Item Output

**Lines 319-368:**

Without `--json` and `quiet == 0`:
- Creates `DownloadDisplay` object
- Updates with progress callback for each file
- Calls `d.finish()` at end with results and destination

With `--json`:
- Prints one JSON object per file result (line 351-353)
- Uses `print_json_file_result()` (lines 531-535)

With `quiet == 1`:
- Single-line summary: `<identifier> <count> files (<size>) in <time>s` (lines 355-361)

### Batch Output

**Lines 372-479:**

Without `--json` and `quiet == 0`:
- Creates `BatchDisplay` object
- Callbacks: `on_item_start`, `on_progress`, `on_item_complete`

With `--json`:
- Prints per-item JSON on completion (lines 396-408)
- Prints failed item errors separately (lines 430-436)
- Each object has: `item`, `status`, `files_ok`, `files_skipped`, `files_failed`, `bytes`, `elapsed_ms`

With `quiet == 1` or summary display:
- Prints summary (lines 468-477):
  ```
  done  <items> items, <files> files downloaded (<bytes>), <skipped> skipped, <failed> failed — <time>s
  ```

### JSON Output Structure

**Single file result** (lines 502-528):
```json
{
  "item": "identifier",
  "file": "file.mp4",
  "status": "ok|skipped|error",
  "bytes": 1234,
  "elapsed_ms": 500
}
```

**Batch item result** (lines 542-562):
```json
{
  "item": "identifier",
  "status": "ok|error",
  "files_ok": 10,
  "files_skipped": 2,
  "files_failed": 1,
  "bytes": 1234567,
  "elapsed_ms": 5000
}
```

### Mutual Exclusivity

**Lines 206-208:**
```rust
if args.json && args.dashboard {
    bail!("--json and --dashboard are mutually exclusive");
}
```

---

## 7. Joblog & Retry Handling

### Joblog Write Format

Each file download produces one `JoblogEntry` (lines 488-498):

| Status | Entry Type | Fields |
|--------|------------|--------|
| **Complete** | `ok()` | `(bytes, elapsed_ms)` |
| **Skipped** | `skipped()` | (reason in skip reason field) |
| **Failed** | `error()` | `(message, 0)` |

**Code** (lines 490-497):
```rust
let entry = match &r.status {
    DownloadStatus::Complete => entry.ok(r.bytes, r.elapsed.as_millis() as u64),
    DownloadStatus::Skipped(_) => entry.skipped(),
    DownloadStatus::Failed(msg) => entry.error(msg, 0),
    _ => continue,
};
```

### Retry-Failed Logic

**Lines 224-237:**

1. Requires both `--retry-failed` AND `--joblog` flags
2. Reads previous joblog
3. Extracts failed items via `ia_core::joblog::failed_items()`
4. Replaces normal identifier collection with failed list
5. Errors if `--joblog` not provided (line 235):
   ```rust
   bail!("--retry-failed requires --joblog");
   ```

### Concurrency Control

#### File-Level Concurrency (`-j, --jobs`)
- Controls how many files download **simultaneously** from a single item
- Shared `Semaphore` with capacity `jobs` (line 285)
- Default: 2 (global default, not download-specific)
- Applied via `semaphore.acquire()` in ia-core/src/download.rs line 689

#### Item-Level Concurrency (`--items`)
- Controls how many items download **simultaneously** in batch/search mode
- Passed to `download_batch()` as `items_concurrency` parameter (line 425)
- Uses `buffer_unordered(items_concurrency)` in ia-core (line 854)
- Default: 2 (download-specific, line 100)
- **Only applies to batch mode**; single-item mode uses sequential execution

---

## 8. File Filtering

### Filter Structure

Passed to `ia_core::download::download_item()` as `DownloadOpts.filter` (lines 274-281):

```rust
filter: FileFilter {
    glob: args.glob.clone(),              // --glob pattern
    exclude: args.exclude.clone(),        // --exclude pattern
    formats: args.format.clone(),         // -f, --format (Vec)
    source: args.source.clone(),          // --source (enum)
    exclude_source: args.exclude_source.clone(), // --exclude-source
    names: args.files.clone(),            // Positional file names
}
```

### Filter Application

In ia-core/src/download.rs line 636:
```rust
let files = crate::files::list(&item, &opts.filter);
```

The `files::list()` function applies all filters and returns a `Vec<&FileMetadata>`.

### Filter Precedence

From ia-core/src/files.rs:
1. **Format filter**: If `-f` provided, only include files matching any format
2. **Source filter**: If `--source` provided, only include files from that source
3. **Exclude source**: If `--exclude-source` provided, exclude those files
4. **Glob pattern**: If `--glob` provided, apply pattern matching
5. **Exclude pattern**: If `--exclude` provided, remove matching files
6. **File names**: If positional files provided, only download those specific files

---

## 9. Disk Pool / Destination Handling

### Configuration

**Lines 250-265:**
```rust
let destdirs = if args.destdir.is_empty() {
    vec![PathBuf::from(".")]
} else {
    args.destdir.clone()
};

let mut disk_pool = if destdirs.len() > 1 {
    Some(DiskPool::new(&destdirs).context("failed to initialize disk pool")?)
} else {
    None
};
```

- If no `--destdir` flags: default to `.` (current directory)
- If multiple `--destdir` flags: create a `DiskPool` for load balancing
- If single `--destdir`: pool is `None`

### Single-Item Mode

**Lines 312-317:**
```rust
let item_opts = if let Some(ref mut pool) = disk_pool {
    let dest = pool.assign_item(identifier, 0)?;
    make_opts(dest.to_path_buf())
} else {
    opts.clone()
};
```

- If pool exists: calls `pool.assign_item(identifier, 0)` to select destination
- Otherwise: uses first destdir

### Batch Mode

**Lines 417-426:**
```rust
let result = ia_core::download::download_batch(
    client,
    identifiers,
    &opts,           // Uses base_destdir, no pool passed
    semaphore,
    progress,
    on_item_start,
    on_item_complete,
    args.items,
)
.await;
```

**Important limitation**: `download_batch()` does NOT receive the disk pool. All items are downloaded to `base_destdir` (first destdir), even if multiple `--destdir` flags provided.

### Path Safety

**ia-core/src/download.rs lines 16-31:**

`validate_download_path()` rejects:
- Empty file names
- Null bytes (`\0`)
- Control characters (0x00-0x1F)
- Parent directory traversal (`..`)
- Absolute paths (`/etc/passwd`)
- Windows prefix paths (`C:\`)

Additionally checks for symlinks in the destination path (lines 194-209):
- Uses `fs::symlink_metadata()` to detect symlinks without following them
- Prevents symlink injection that could redirect writes outside `dest_dir`

---

## 10. Error Handling Patterns

### File-Level Retry Logic

**ia-core/src/download.rs lines 700-712:**

For each file:
1. Attempts up to `opts.retries` times (default 5)
2. Calls `e.is_retryable()` to check if error should be retried
3. Exponential backoff: `2^attempt` seconds, capped at 60s
4. Non-retryable errors fail immediately without further retries
5. Final error recorded in `FileDownloadResult.status`

### Item-Level Errors

**ia-core/src/download.rs lines 840-851:**

If item metadata fetch fails:
- Wrapped in `Err((identifier, error))`
- Reported to batch result collector
- Item is skipped (not retried at item level)

### Batch-Level Exit Code

**Lines 481-483:**
```rust
if result.files_failed > 0 || result.items_failed > 0 {
    std::process::exit(1);
}
```

- Exit code 1 if any file or item failed
- Exit code 0 on complete success

### Joblog Recovery

Failed items recorded in joblog can be re-downloaded with:
```bash
ia download --joblog path/to/joblog --retry-failed
```

---

## 11. Dry-Run Behavior

### Implementation

**ia-core/src/download.rs lines 258-264:**

```rust
if opts.dry_run {
    return Ok(FileDownloadResult {
        file_name: file.name.clone(),
        bytes: file.size.unwrap_or(0),
        status: DownloadStatus::Skipped("dry run".to_string()),
        elapsed: start.elapsed(),
    });
}
```

### Scope

- Short-circuits BEFORE contacting server
- Still enumerates files and applies filters (metadata fetch happens)
- Reports file list and destination paths
- Returns files as `Skipped("dry run")` status
- Does NOT attempt actual download or disk I/O

### Display

Files show as "skipped" in progress output:
```
file.mp4  (skipped: dry run)
```

---

## Cross-Cutting Issues

### 1. **Confusing `--items` vs `-j/--jobs` naming**

**Problem:**
- Two different concurrency limits with inconsistent naming
- `--jobs` (global, file-level concurrency)
- `--items` (download-only, item-level concurrency in batch)
- Other commands may not expose both levels

**Impact:** Users confused about which flag controls what

**Lines:** 98-100, 285, 425

**Recommendation:** Standardize naming across all commands that support batch:
- Suggest `--items-concurrency` or `--batch-jobs` instead of `--items`
- Document the distinction clearly in help text

### 2. **Disk pool not used in batch mode**

**Problem:**
- `--destdir` is repeatable for multi-disk setup
- Works for single-item downloads (uses `pool.assign_item()`)
- Ignored in batch downloads (all items → `base_destdir`)

**Impact:** Can't distribute large batch downloads across multiple disks

**Lines:** 250-265, 312-317, 417-426

**Recommendation:** Pass disk pool to `download_batch()` or at least warn users that multi-disk is single-item only

### 3. **File names incompatible with all batch methods**

**Problem:**
- Positional `<file...>` args reject `--itemlist`, `--search`, AND stdin
- User error silently fails if piped input mixed with file names
- No clear UI affordance that file names = single-item mode only

**Impact:** Confusing error messages or silent failures

**Lines:** 154-155, 168-169

**Recommendation:**
- Add early validation: if positional files provided, reject any other identifier source
- Better error message showing mutually exclusive usage

### 4. **Inconsistent identifier source priority**

**Problem:**
- Priority order: CLI positional → `--itemlist` → `--search` → stdin
- No clear warning if user provides multiple sources
- Example: `ia download item1 --itemlist ids.txt` — which wins?

**Impact:** Silent behavior change if user forgets to remove old flags

**Lines:** 147-196

**Recommendation:** Add validation to reject multiple identifier sources or make priority explicit in help

### 5. **Progress display mismatch for dry-run**

**Problem:**
- Dry-run is handled silently in core layer (returns `Skipped("dry run")`)
- No special TUI handling to show it's a "dry run" vs real download
- CLI doesn't short-circuit early before progress setup
- Ambiguous in output whether files were actually downloaded

**Impact:** User confusion about what happened

**Lines:** 258-264, 319-368

**Recommendation:**
- Add dry-run indicator in progress display
- Maybe a different color or prefix in progress bars
- Or short-circuit at CLI layer before creating progress callbacks

### 6. **`--quiet` counter inconsistent across commands**

**Problem:**
- Download uses `--quiet` as `u8` counter (0=normal, 1=silent-summary, 2=silent-all)
- Unknown if upload/metadata/tasks handle quiet the same way
- May have different semantics per command

**Impact:** Batch scripts expect `-q` to have consistent behavior across all commands

**Lines:** 201, 354-361, 451-478

**Recommendation:** Audit all commands for consistent quiet levels:
- Level 0 (default): Full output
- Level 1 (`-q`): Suppress progress/summary
- Level 2 (`-qq`): Suppress all output except errors
- Document in main help text

---

## Summary Statistics

- **Positional args:** 2 (identifier, files)
- **Named flags:** 16
- **Global options passed:** 4
- **Batch input methods:** 4 (CLI, --itemlist, --search, stdin)
- **Output formats:** 5 (human, quiet L1, quiet L2, JSON, dashboard)
- **Concurrency limits:** 2 (file-level, item-level)
- **Filter types:** 5 (glob, exclude, format, source, exclude-source)
- **Error handling strategies:** 3 (file retry, item skip, batch aggregate)

