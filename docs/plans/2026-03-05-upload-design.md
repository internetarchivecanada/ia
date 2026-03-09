# Upload Design Document

**Date:** 2026-03-05
**Status:** Approved
**Scope:** `ia upload` command — single item, batch import, template generation

## Overview & Goals

Upload is the most critical operation in the tool — bits in is the foundation of the archive.org mission. Data loss and corruption are unacceptable.

### Design Priorities

1. **Data integrity** — verify by default, checksum everything, never corrupt
2. **Robustness** — handle 503s gracefully, retry intelligently, resume in multipart mode
3. **Performance** — 100-continue to avoid wasted bandwidth, modest file parallelism, efficient I/O
4. **Usability** — informative progress, dashboard, clear errors, dry-run, template generation
5. **Safety** — backup by default, collection checks, test-item mode, skip symlinks

### Phases

- **Phase 1:** Single PUT uploads (core — 90% of use cases)
- **Phase 2:** Multipart upload with resume + cleanup
- **Phase 3:** Dashboard (TUI) — can be developed in parallel with Phase 1

## CLI Interface

### Commands

```
ia upload <identifier> <files>...     # single item upload
ia upload import <spreadsheet>        # batch from CSV/TSV/XLSX/ODS/JSONL
ia upload template <dir> -o <file>    # generate spreadsheet template
ia upload cleanup <id> [file]         # abort incomplete multipart (hidden)
```

### Single Item Flags

| Flag | Default | Description |
|------|---------|-------------|
| `-m, --metadata KEY:VALUE` | — | Set metadata (repeatable) |
| `--remote-name NAME` | — | Explicit remote filename (required for stdin) |
| `--remote-dir DIR` | — | Prepend path prefix to all filenames |
| `--keep-directories` | off | Preserve relative path structure |
| `--no-derive` | off | Skip derivative generation |
| `--no-backup` | off | Don't keep old file versions |
| `--no-auto-make-bucket` | off | Error if item doesn't exist |
| `--no-verify` | off | Skip Content-MD5 verification |
| `--no-size-hint` | off | Don't send x-archive-size-hint |
| `--no-collection-check` | off | Skip collection existence check |
| `--checksums FILE` | — | Pre-computed MD5 file (GNU md5sum or BSD md5 format) |
| `--checksum` | off | Skip files already uploaded (MD5 match) |
| `--delete-after-upload` | off | Delete local file after verified upload |
| `--test-item` | off | Upload to test_collection (real upload, auto-removed after 30 days) |
| `--open-after-upload` | off | Open item in browser after upload |
| `--multipart` | off | Use multipart upload (Phase 2) |
| `--dry-run` | off | Validate everything, upload nothing |
| `-H, --header KEY:VALUE` | — | Arbitrary HTTP header (repeatable) |
| `--retries N` | 10 | Retry attempts on transient failure |
| `--retry-sleep SECS` | 30 | Sleep between retries |
| `--json` | off | JSONL output |
| `--dashboard` | off | Ratatui TUI |

### Import Flags

All single item flags apply except `--remote-name`, `--remote-dir`, `--keep-directories` (these are controlled per-row in the spreadsheet via columns).

### Template Flags

| Flag | Description |
|------|-------------|
| `-o, --output FILE` | Output file (default: stdout) |
| `--format FORMAT` | csv/tsv/xlsx (default: csv) |
| `--identifier-prefix PREFIX` | Prepend to generated identifiers |
| `--identifier-from-filename` | Auto-generate identifiers from filenames |
| `--identifier-from-dirname` | Auto-generate identifiers from parent directory |

### Global Flags That Apply

`-j/--jobs`, `--joblog`, `--retry-failed`, `-q/--quiet`, `-c/--config-file`, `-d/--debug`

### Reserved Global Short Flags (Do NOT Reuse)

`-c`, `-l`, `-d`, `-i`, `-H`, `-j`, `-q`

## ia-core Architecture

### Module Structure

```
ia-core/src/upload/
├── mod.rs          — public API re-exports
├── types.rs        — UploadOpts, UploadResult, UploadProgress, etc.
├── single.rs       — upload_file() — single file PUT
├── item.rs         — upload_item() — multi-file to one item
├── batch.rs        — upload_batch() — multi-item from spreadsheet
├── headers.rs      — S3 header construction, metadata encoding
├── check_limit.rs  — per-item rate limiter, 503 polling
├── validate.rs     — identifier validation, collection check, file checks
├── multipart.rs    — Phase 2: multipart upload + resume + cleanup
└── template.rs     — spreadsheet template generation
```

### Core Types (`types.rs`)

```rust
pub struct UploadOpts {
    pub metadata: Vec<(String, String)>,
    pub remote_name: Option<String>,
    pub remote_dir: Option<String>,
    pub keep_directories: bool,
    pub verify: bool,           // default: true
    pub checksum: bool,         // skip already-uploaded
    pub checksums: Option<HashMap<String, String>>,  // pre-computed MD5s
    pub delete_after_upload: bool,
    pub no_derive: bool,
    pub no_backup: bool,
    pub no_auto_make_bucket: bool,
    pub no_size_hint: bool,
    pub no_collection_check: bool,
    pub test_item: bool,
    pub multipart: bool,
    pub retries: u32,           // default: 10
    pub retry_sleep: Duration,  // default: 30s
    pub headers: Vec<(String, String)>,
    pub dry_run: bool,
}

pub struct UploadResult {
    pub identifier: String,
    pub key: String,              // remote filename
    pub status: UploadStatus,
    pub bytes: u64,
    pub md5: Option<String>,
    pub elapsed: Duration,
    pub retries: u32,
}

pub enum UploadStatus {
    Uploaded,
    Skipped,       // checksum match
    Failed(String),
    DryRun,
}

pub struct UploadProgress {
    pub identifier: String,
    pub key: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub status: UploadProgressStatus,
}

pub enum UploadProgressStatus {
    Uploading,
    Verifying,        // computing MD5
    WaitingRateLimit,  // 503, polling check_limit
    Complete,
    Skipped,
    Failed,
}
```

### Public API (`mod.rs`)

```rust
/// Upload a single file to an item.
pub async fn upload_file(
    client: &IaClient,
    identifier: &str,
    file: &Path,
    key: &str,
    opts: &UploadOpts,
    is_last_file: bool,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<UploadResult, IaError>;

/// Upload multiple files to a single item.
pub async fn upload_item(
    client: &IaClient,
    identifier: &str,
    files: &[PathBuf],
    opts: &UploadOpts,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<Vec<UploadResult>, IaError>;

/// Batch upload from spreadsheet records.
pub async fn upload_batch(
    client: &IaClient,
    records: Vec<SpreadsheetRecord>,
    opts: &UploadOpts,
    concurrency: usize,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<Vec<UploadResult>, IaError>;
```

### Validation (`validate.rs`)

```rust
/// Validate identifier against IA rules (3-100 chars, [a-zA-Z0-9._-], starts with alphanumeric or @).
pub fn validate_identifier(id: &str) -> Result<(), IaError>;

/// Check that collection(s) exist via metadata API, with retry on transient failure.
pub async fn check_collections(
    client: &IaClient,
    collections: &[&str],
) -> Result<(), IaError>;

/// Pre-flight validation: files exist, not symlinks, required metadata present.
pub fn validate_upload(
    files: &[PathBuf],
    metadata: &[(String, String)],
    opts: &UploadOpts,
) -> Result<(), IaError>;
```

## Upload Flow

### Single File (`single.rs`)

1. Validate file exists, not symlink, get size
2. Determine remote key:
   - Default: basename (`/path/to/file.pdf` → `file.pdf`)
   - `--keep-directories`: relative path structure preserved
   - `--remote-name`: explicit name (required for stdin)
   - `--remote-dir`: prepend prefix (`scans/` + `file.pdf` → `scans/file.pdf`)
3. If `--checksum`: compute local MD5, fetch remote metadata, compare. Skip if match AND no pending catalog tasks.
4. If verify (default): compute MD5 (or look up from `--checksums` file)
5. Build URL: `PUT https://s3.us.archive.org/{identifier}/{url_encoded_key}`
6. Build headers (delegates to `headers.rs`)
7. Set `Expect: 100-continue`
8. Enter retry loop:
   a. If not first attempt: poll `check_limit`, sleep if overloaded
   b. Send PUT with streaming body + progress callback
   c. On 200: success. If `--delete-after-upload`: close file handle, delete local file.
   d. On 503: check body for "appears to be spam" (abort if spam). Otherwise sleep and retry.
   e. On other error: classify retryable vs permanent, retry or fail.
9. Return `UploadResult`

### Single Item (`item.rs`)

1. Validate identifier
2. If `!no_collection_check`: `check_collections()` with retry
3. If `--test-item`: inject `collection:test_collection` into metadata
4. Gather all files (expand directories via walkdir)
5. If `!no_size_hint`: compute total size, set `x-archive-size-hint`
6. Pre-compute file → remote key mapping
7. Upload files with modest parallelism (2-3 concurrent via semaphore):
   - All files get `x-archive-queue-derive: 0` EXCEPT the last
   - Last file gets `x-archive-queue-derive: 1` (unless `--no-derive`)
   - `x-archive-auto-make-bucket: 1` on first file only
   - Metadata headers on first file only (item creation)
8. Collect results, report summary

**Important: ordering with file parallelism.** Metadata headers and `auto-make-bucket` only go on the first PUT (item creation). Derive is triggered only on the last file. "Last" means last to be *submitted*, not last to *complete*. The first file must start before others (carries metadata + auto-make-bucket). With 2-3 concurrent files, we submit the first file, wait for its headers to be accepted (100-continue), then allow subsequent files to proceed.

### Batch (`batch.rs`)

1. Read spreadsheet, parse records
2. Pre-scan: group by identifier
3. Per group:
   a. Validate identifier format
   b. Validate required metadata present (`mediatype`, `collection`)
   c. Stat all files — exist? not symlinks? compute total size for size hint
   d. Collect unique collections
4. Validate all collections exist (single batch check, not per-item)
5. Report any errors upfront before uploading anything (fail fast)
6. Upload items concurrently (controlled by `-j/--jobs`):
   - Each item uploads its files with modest parallelism (within-item)
   - Per-item rate limiter handles 503s independently
   - Joblog entry per file
7. Collect results, report summary

### Stdin Upload

- Reads stdin into a temporary file (1 MiB chunks)
- `--remote-name` required (no filename to derive from)
- Otherwise follows single file flow

## S3 Header Construction (`headers.rs`)

This is the trickiest part — the encoding rules are subtle and bugs here corrupt metadata silently.

### Header Key Format

```
x-archive-meta{NN:02d}-{key}
```

Where `NN` is zero-padded index (00, 01, 02...), and underscores in `key` become double-dashes:

```
title         → x-archive-meta00-title: My Item
my_field      → x-archive-meta00-my--field: value
```

### Multivalue Metadata

Incrementing index per field:

```
subject: [rust, archive] →
  x-archive-meta00-subject: rust
  x-archive-meta01-subject: archive
```

### Non-ASCII / Whitespace Encoding

Values containing non-ASCII characters or any whitespace are wrapped in `uri()` with percent-encoding (UTF-8):

```
x-archive-meta00-title: uri(My%20Snowman%20%E2%98%83)
```

A value needs quoting if:
- It contains any non-ASCII character, OR
- It contains any whitespace character

### File-Level Metadata

Uses `filemeta` instead of `meta`:

```
x-archive-filemeta00-title: My File
```

### Empty Values

Skipped — empty/blank values do not produce headers.

### Fixed Headers Per Upload

| Header | Value | When |
|--------|-------|------|
| `Authorization` | `LOW {access}:{secret}` | Always |
| `Content-Length` | file size in bytes | Always (NEVER chunked transfer encoding) |
| `Content-MD5` | hex MD5 digest | When verify=true |
| `Expect` | `100-continue` | Always |
| `x-archive-auto-make-bucket` | `1` | First file only (unless `--no-auto-make-bucket`) |
| `x-archive-queue-derive` | `0` or `1` | `0` all files, `1` last file only |
| `x-archive-keep-old-version` | `1` | Unless `--no-backup` |
| `x-archive-size-hint` | total bytes | First file only (unless `--no-size-hint`) |
| `x-archive-meta*` | encoded metadata | First file only |

### URL Construction

```
PUT https://s3.us.archive.org/{identifier}/{url_encoded_key}
```

The key (remote filename) is URL-encoded. Leading slashes are stripped.

### Content-Length Is Mandatory

IA S3 does NOT support chunked transfer encoding. `Content-Length` must always be set. For empty files, explicitly set `Content-Length: 0`. If the HTTP library adds `transfer-encoding: chunked`, strip it.

### Key Functions

```rust
/// Encode metadata key-value pairs into x-archive-meta headers.
pub fn encode_metadata_headers(
    metadata: &[(String, String)],
) -> Vec<(String, String)>;

/// Encode file-level metadata into x-archive-filemeta headers.
pub fn encode_file_metadata_headers(
    metadata: &[(String, String)],
) -> Vec<(String, String)>;

/// Build complete header set for an upload PUT request.
pub fn build_upload_headers(
    opts: &UploadOpts,
    file_size: u64,
    md5: Option<&str>,
    is_first_file: bool,
    is_last_file: bool,
    access_key: &str,
    secret_key: &str,
) -> HeaderMap;

/// Check if a string value needs uri() encoding.
fn needs_quote(s: &str) -> bool;
```

## Rate Limiting & 503 Handling (`check_limit.rs`)

### Per-Item Rate Limiter

Each item gets its own rate limit state. When a 503 is received, that item's uploads pause and poll `check_limit` until clear. Other items continue independently.

### check_limit Endpoint

```
GET https://s3.us.archive.org?check_limit=1&accesskey={key}&bucket={identifier}
```

Response:
```json
{"bucket": "my-item", "accesskey": "...", "over_limit": 0, "detail": "..."}
```

`over_limit != 0` means overloaded.

### 503 Handling Flow

```
1. Receive 503 on upload PUT
2. Check response body — if "appears to be spam", abort immediately (permanent failure)
3. Enter poll loop:
   a. Call check_limit with item's identifier and access key
   b. If over_limit == 0: break, resume uploading
   c. If over_limit != 0: sleep retry_sleep (default 30s)
   d. If JSON parse fails: treat as overloaded (conservative)
   e. Decrement retry counter, fail if exhausted
4. Re-send the upload (seek body to start)
```

### Design Principle

Hammer away, pause when told, poll until clear, hammer again. This matches IA's design — the system is built to handle bursts with backpressure via 503s. Rate limits are most commonly account-wide or global, but can be per-item. Per-item rate limiters handle both cases correctly: account-wide 503s will cause all items to independently pause and poll.

### Core Type

```rust
pub struct ItemRateLimiter {
    identifier: String,
    access_key: String,
    paused: AtomicBool,
    notify: Notify,
}

impl ItemRateLimiter {
    /// Poll check_limit, sleep if overloaded, return when clear.
    pub async fn wait_until_clear(
        &self,
        client: &IaClient,
        retry_sleep: Duration,
        retries: &mut u32,
        on_status: impl Fn(RateLimitStatus),
    ) -> Result<(), IaError>;

    /// One-shot check: is this item currently rate limited?
    pub async fn is_overloaded(
        &self,
        client: &IaClient,
    ) -> Result<bool, IaError>;
}

pub enum RateLimitStatus {
    Polling,
    Waiting { seconds: u64 },
    Cleared,
    Exhausted,
}
```

## 100-Continue

### Goal

Never send a large body if the server would reject the request. Matches curl's behavior.

### Expected Behavior

1. Send headers with `Expect: 100-continue`
2. Wait for server response (up to ~1 second timeout)
3. `100 Continue` → stream body
4. `403`/`503`/etc. → abort, never send body
5. Timeout with no response → send body anyway (fallback for non-compliant proxies)

### Implementation Strategy

reqwest uses hyper internally. Three possible outcomes to verify during implementation:

1. **reqwest handles it** — hyper sees the `Expect` header, waits for 100 before polling the body. We just set the header and it works. Best case.
2. **hyper handles it but reqwest interferes** — we bypass reqwest's body handling for upload PUTs, using hyper directly for the upload request path only.
3. **Neither handles it automatically** — we implement a two-phase request with direct hyper usage.

**Action item:** write a test that sets `Expect: 100-continue`, verifies the body is NOT sent when the server responds with 403 before 100. This determines which scenario we're in.

**If hyper is needed directly:** scope it tightly to upload PUT only. All other HTTP calls remain on reqwest. hyper is already a transitive dependency via reqwest, so no new crate needed.

## Checksum & Verification

### Three Distinct Features

**1. Verify (on by default, `--no-verify` to disable)**

- Compute MD5 of local file before upload
- Send as `Content-MD5` header on PUT
- Server validates — rejects with `BadDigest` (400) if mismatch
- Catches corruption in transit
- Cost: one full file read before upload (usually cached by OS page cache)

**2. Checksum Skip (`--checksum`, off by default)**

- Before uploading, compute local MD5
- Fetch item metadata, find remote file's MD5
- If match AND no pending catalog tasks → skip upload
- If no remote file or mismatch → upload normally
- Tasks check prevents skipping when item is in flux
- Useful for idempotent re-runs; joblog + `--retry-failed` handles batch resume more efficiently

**3. Pre-Computed Checksums (`--checksums FILE`)**

- Parse MD5 file (GNU `md5sum` or BSD `md5` format)
- Match filenames by basename (or relative path with `--keep-directories`)
- Skips the pre-read — use provided MD5 for Content-MD5 header and checksum skip
- Warn and compute if a file is missing from the checksums file (don't fail)

### Flag Interactions

| Flags | MD5 Computed? | Content-MD5 Header? | Skip Check? |
|-------|--------------|---------------------|-------------|
| (default: verify on) | Yes | Yes | No |
| `--checksum` | Yes | Yes | Yes |
| `--no-verify` | No | No | No |
| `--no-verify --checksum` | Yes (for skip) | No | Yes |
| `--checksums FILE` | From file | Yes | If `--checksum` |

### `--delete-after-upload` Flow

1. Forces verify=true (non-negotiable)
2. Compute MD5, send Content-MD5 header
3. Upload file
4. Receive 200 → server confirmed MD5 match
5. Close file handle
6. Delete local file
7. If upload failed or MD5 mismatch → do NOT delete

## Concurrency Model

### Within a Single Item

Modest file-level parallelism: 2-3 concurrent files via semaphore. This keeps single-item uploads fast while respecting IA's catalog which runs tasks synchronously per item.

**Ordering constraints:**
- First file must be submitted first (carries metadata headers + auto-make-bucket)
- Last file must be identified and flagged for derive
- "Last" = last to be submitted, not last to complete

### Batch Mode

`-j/--jobs` controls how many items upload concurrently. Each item's files upload with modest parallelism internally. Per-item rate limiters handle 503s independently.

### Concurrency Pattern

Same as download: `Arc<Semaphore>` for file-level rate limiting, `tokio::task::JoinSet` for structured concurrency, per-item rate limiter for 503 handling.

## Progress & Dashboard

### Default Output (Single Item)

```
my-item
  ✓ document.pdf                    4.2 MiB  2.1 MiB/s
  ↑ large-video.mp4          [==========>       ] 67%  1.2 GiB/1.8 GiB  45 MiB/s
  · notes.txt                       pending
```

### Default Output (Batch/Import)

```
Uploading 42 items (156 files, 12.4 GiB)
  ✓ item-001    3/3 files    124 MiB
  ↑ item-002    1/5 files    [======>           ] 34%
  ⏸ item-003    rate limited (polling check_limit...)
  · item-004    pending
```

### `--json` Output (JSONL)

```json
{"identifier":"my-item","key":"document.pdf","status":"uploaded","bytes":4404019,"md5":"a1b2c3...","elapsed_ms":2100,"retries":0}
{"identifier":"my-item","key":"large-video.mp4","status":"uploading","bytes_sent":1288490188,"total_bytes":1932735283}
```

### `--dashboard` (Ratatui TUI)

Shared infrastructure with download dashboard. Panels ordered by priority
(lower-priority panels hidden first when terminal is small):

| Priority | Panel | Content |
|----------|-------|---------|
| 1 | **Files** | Current file transfers with animated progress bars |
| 2 | **Items** | Item list with per-item file progress, status, speed |
| 3 | **Errors** | Recent errors with context |
| 4 | **Throughput** | Aggregate upload speed sparkline |
| 5 | **Rate Limit** | Per-item rate limit status, check_limit poll results |
| 6 | **S3 Tasks** | Live s3-put task count from tasks API (refreshes ~60s) |

### Shared TUI Framework (Trait-Based)

Refactor the download dashboard into a trait-based framework that both download
and upload dashboards implement. The framework provides:

- `Dashboard` trait with `update()`, `draw()`, `handle_input()` methods
- Terminal lifecycle management (setup, teardown, panic guard)
- Event loop skeleton (tick rate, input polling, render cycle)
- Throughput tracker (sampling, history, sparkline rendering)
- Common panel widgets (error panel, item list, progress bars)
- Keyboard handling (q/Ctrl-C quit, j/k scroll)

The download dashboard is migrated onto this framework first, proving the
abstraction before the upload dashboard is built on top of it.

### Byte-Level Upload Progress

`single.rs` wraps the request body in an async progress-reporting stream that
fires the `UploadProgress` callback as bytes are written, enabling smooth
animated progress bars. Without this, bars would jump from 0% to 100%.
Similar to how the download dashboard tracks bytes via reqwest's streaming
response, but applied to the outgoing request body.

### S3 Tasks Panel — Minimal Tasks API

A minimal read-only `tasks` module in `ia-core` provides just enough to power
the S3 Tasks panel: a single GET to `/services/tasks.php` filtered by
identifier/cmd, returning task counts and statuses. This module will be expanded
into a full `ia tasks` CLI command later. No throwaway work — the types and
client methods are reusable.

### Upload-Specific Additions

- S3 task queue panel (new, upload only)
- Rate limit status panel (new, upload only — download doesn't hit S3)
- "Verifying" status in progress (MD5 computation phase)

## Error Handling

### New `IaError` Variants

```rust
// Upload-specific
UploadFailed { identifier: String, key: String, message: String },
SpamDetected { identifier: String },
CollectionNotFound { collection: String },
InvalidIdentifier { identifier: String, reason: String },
MissingRequiredMetadata { field: String },
CheckLimitFailed { identifier: String },
FileTooLarge { path: PathBuf, size: u64 },
EmptyUpload,
SymlinkSkipped { path: PathBuf },

// Phase 2
MultipartAborted { identifier: String, key: String },
MultipartIncomplete { identifier: String, key: String, upload_id: String },
```

### Retryable vs Permanent

| Error | Retryable? |
|-------|-----------|
| 503 SlowDown | Yes — poll check_limit, retry |
| 503 spam detected | No — abort immediately |
| 500 InternalError | Yes |
| 400 BadDigest | No — data corruption, fail with clear message |
| 400 MissingContentLength | No — bug in our code |
| 403 AccessDenied | No — bad credentials |
| 403 InvalidAccessKeyId | No — bad credentials |
| 409 OperationAborted | Yes — conflicting operation, retry after delay |
| Network error | Yes |
| IO error (local disk) | No |
| check_limit unreachable | Yes — treat as overloaded, sleep and retry |

### S3 XML Error Parsing

IA S3 returns errors as XML:
```xml
<Error>
  <Code>SlowDown</Code>
  <Message>Please reduce your request rate.</Message>
  <Resource>/my-item/file.pdf</Resource>
  <RequestId>db1b9e2b-...</RequestId>
</Error>
```

Parse `Code` and `Message` with basic string matching (no XML crate). Filter out `Resource` text containing `PUT`. On parse failure, fall back to status code + raw body.

### `--json` Error Output

```json
{"error":{"code":"slow_down","message":"Rate limited on my-item, retrying (3/10)","identifier":"my-item","key":"file.pdf"}}
```

## Import Spreadsheet & Template

### Spreadsheet Format

| Column | Required | Description |
|--------|----------|-------------|
| `identifier` (or `item`) | Yes | Target item identifier |
| `file` | Yes | Local file path |
| `REMOTE_NAME` | No | Override remote filename |
| All other columns | No | Metadata key:value pairs |

**Rules:**
- One file per row (100 files for one item = 100 rows with same identifier)
- First row with a given identifier carries metadata (item-level)
- Required metadata (`mediatype`, `collection`) validated per unique identifier, not per row
- Supports CSV, TSV, XLSX, ODS, JSONL (reuses `spreadsheet.rs` reader)

### Pre-Scan Phase

1. Read all records
2. Group by identifier
3. Per group: validate identifier format, validate required metadata, stat all files (exist? symlinks?), compute total size for size hint
4. Collect unique collections, validate all exist (single batch check)
5. Report ALL errors upfront before uploading anything (fail fast)
6. Proceed with upload

### Template Generation (`ia upload template`)

```
ia upload template ./my-photos/ -o upload.csv
```

Generates:
```csv
identifier,file,REMOTE_NAME,mediatype,collection,title,description,creator,date,subject,language
,./my-photos/img001.jpg,,,,,,,,
,./my-photos/img002.jpg,,,,,,,,
,./my-photos/subdir/img003.jpg,,,,,,,,
```

- Walks directory recursively, one row per file
- Skips symlinks, hidden files (dotfiles)
- `identifier` column empty — user fills in
- Required + recommended metadata columns as empty headers
- `file` column pre-filled with relative paths

**`--identifier-prefix`:** Pre-fills identifier column with `{prefix}{sanitized_filename}`.

**`--identifier-from-dirname`:** Uses parent directory name as identifier.

**Identifier sanitization:** lowercase, strip invalid chars, ensure starts with alphanumeric, 5-100 chars, conform to IA rules.

## Multipart Upload (Phase 2)

### Opt-In Only

`--multipart` flag. Off by default to avoid orphaned uploads on IA.

### When to Recommend

- Files >5 GB (re-upload on failure is painful)
- Unreliable connections
- Upload jobs that may be interrupted and resumed

### Flow

1. **Initiate:** `POST /{identifier}/{key}?uploads` → UploadId
2. **Split:** file into parts (default 100 MiB)
3. **Upload parts:** `PUT /{identifier}/{key}?partNumber={N}&uploadId={ID}` → ETag
4. **Complete:** `POST /{identifier}/{key}?uploadId={ID}` with XML manifest of (partNumber, ETag) pairs. Header: `x-archive-keep-old-version: 1` (must be at completion time).
5. Server assembles parts into final file.

### Resume (Automatic)

1. Before initiating, check for existing uploads: `GET /{identifier}?uploads`
2. If found for same key: `GET /{identifier}/{key}?uploadId={ID}` → list completed parts
3. Skip already-uploaded parts, upload remaining
4. Complete

No local state file needed — all state lives on the server.

### Cleanup (Hidden Subcommand)

```
ia upload cleanup my-item              # list all incomplete multipart uploads
ia upload cleanup my-item file.zip     # abort specific file's upload
ia upload cleanup my-item --abort-all  # abort everything
```

API calls:
```
List:  GET /{identifier}?uploads
Abort: DELETE /{identifier}/{key}?uploadId={ID}
```

### Part Size

- S3 minimum: 5 MiB (except last part)
- Default: 100 MiB (balances resume granularity vs overhead)
- Not exposed as a flag in Phase 2

## Testing Strategy

### Safety Rule

**NEVER send live requests to s3.us.archive.org in tests.**

### Test Layers

**1. Unit Tests (ia-core)**

- `headers.rs` — metadata encoding (the most critical unit):
  - Underscore → double-dash conversion
  - `uri()` wrapping for non-ASCII and whitespace
  - Multivalue index numbering
  - Empty value skipping
  - Edge cases: emoji, CJK, mixed ASCII/non-ASCII, very long values
- `validate.rs` — identifier validation against IA rules
- `check_limit.rs` — JSON response parsing, malformed JSON → treat as overloaded
- `template.rs` — spreadsheet generation, identifier sanitization
- S3 XML error parsing — extract Code/Message from various responses
- `--checksums` file parsing — GNU md5sum and BSD md5 formats

**2. Integration Tests with wiremock (ia-core)**

- Full upload flow against mock S3 endpoint:
  - Successful single file PUT (verify all headers, body, Content-Length)
  - 503 → check_limit poll → retry → success
  - 503 spam detection → immediate abort
  - 403 → permanent failure
  - 400 BadDigest → Content-MD5 mismatch
  - Checksum skip (mock metadata endpoint with matching MD5)
  - Empty file upload (Content-Length: 0)
  - Multiple files with derive-on-last-file behavior
  - Metadata header encoding on first file
  - `x-archive-keep-old-version` present/absent
  - Collection check (mock metadata endpoint)
  - Retry exhaustion
  - 100-continue behavior (if testable via wiremock)
- Batch upload flow:
  - Multiple items, concurrent uploads
  - Per-item rate limiting (503 on one item, others continue)
  - Joblog entries written correctly

**3. CLI Tests (ia-cli, assert_cmd)**

- `ia upload --dry-run` — validates without uploading
- `ia upload --test-item` — injects test_collection
- `ia upload --json` — JSONL output format
- `ia upload import` — reads spreadsheet, validates required fields
- `ia upload template` — generates correct spreadsheet
- Flag interactions: `--no-verify --checksum`, `--delete-after-upload` forces verify
- Error cases: missing file, missing metadata, bad identifier, symlink skipped

**4. Simulated Error Testing (manual, developer only)**

Use `x-archive-simulate-error:{ErrorType}` header against live S3 to verify error handling. Not in automated tests. Error types to test:
- `SlowDown` (503)
- `AccessDenied` (403)
- `BadDigest` (400)
- `InternalError` (500)
- `MissingContentLength` (411)
- `ServiceUnavailable` (503)

**5. Property-Based Testing (if warranted)**

- Header encoding: any valid metadata key/value produces valid headers
- Identifier sanitization: output always passes validation

## Phasing

### Phase 1: Core Upload (Single PUT)

The MVP. Covers 90% of use cases.

| Component | Description |
|-----------|-------------|
| `upload/types.rs` | All types |
| `upload/headers.rs` | S3 header construction, metadata encoding |
| `upload/validate.rs` | Identifier validation, collection check, file pre-flight |
| `upload/check_limit.rs` | Per-item rate limiter, 503 polling |
| `upload/single.rs` | Single file PUT with retry loop, 100-continue |
| `upload/item.rs` | Multi-file to one item, derive-on-last, size hint |
| `upload/batch.rs` | Batch from spreadsheet, pre-scan, concurrent items |
| `upload/template.rs` | Spreadsheet template generation |
| CLI: `commands/upload.rs` | All subcommands, flags, output modes |
| CLI: default output | Progress bars, rate limit status |
| CLI: `--json` output | JSONL per file |
| Tests | Full unit + integration + CLI test suite |

### Phase 2: Multipart Upload

Additive. New module + flag, no changes to Phase 1 code.

| Component | Description |
|-----------|-------------|
| `upload/multipart.rs` | Initiate, upload parts, complete, abort, resume |
| `upload cleanup` subcommand | Hidden, list/abort incomplete uploads |
| `--multipart` flag | Opt-in on upload and import |
| Tests | Wiremock tests for multipart flow, resume, cleanup |

### Phase 3: Dashboard

Can be developed in parallel with Phase 1 once progress callback API is stable.

| Component | Description |
|-----------|-------------|
| Shared TUI infrastructure | Factor out common panels from download dashboard |
| Upload dashboard panels | S3 task queue, rate limit status, file progress |
| `--dashboard` flag | Ratatui TUI for upload and import |

## Key Differences from Python `internetarchive`

| Aspect | Python | Rust |
|--------|--------|------|
| Verify | Off by default | **On by default** (Content-MD5 always sent) |
| 100-continue | Not implemented | **Implemented** (matching curl behavior) |
| Concurrency | Sequential within item | **Modest parallelism** (2-3 files) within item |
| Rate limiting | Per-request polling | **Per-item rate limiter** |
| Resume | None (full re-upload) | **Multipart resume** (Phase 2, opt-in) |
| Batch input | CSV only | **CSV/TSV/XLSX/ODS/JSONL** |
| Template generation | None | **`ia upload template`** |
| Dashboard | None | **Ratatui TUI** |
| Test mode | Manual | **`--test-item` flag** |
| Checksum file | None | **`--checksums` for pre-computed MD5s** |

## References

- IA-S3 API: https://archive.org/developers/ias3.html
- Python library source: `internetarchive/item.py`, `internetarchive/iarequest.py`
- Metadata schema: https://archive.org/developers/metadata-schema
- Items documentation: https://archive.org/developers/items.html
- Error simulation: `curl s3.us.archive.org -H 'x-archive-simulate-error:help'`
