# `ia tasks` — Full Tasks CLI Command Design

**Date:** 2026-03-10
**Parent issue:** #240
**Status:** Draft

## Overview

Expand the existing minimal `tasks.rs` (read-only, upload-dashboard-only) into a
full-featured `ia tasks` CLI command with both read and write operations. The
command follows the project's hybrid subcommand pattern: bare `ia tasks
[identifier]` handles the most common use case (listing), while distinct
operations get their own subcommands.

**Command alias:** `ta` (consistent with `do`, `up`, `md`, `se`, `co`, `ls`).

## API Reference

**Endpoint:** `https://archive.org/services/tasks.php`
**Logs endpoint:** `https://catalogd.archive.org/services/tasks.php`
**Auth:** `Authorization: LOW <access>:<secret>` (S3 keys, not cookies)
**Methods:** GET (list/logs/rate-limits), POST (submit), PUT (rerun)

### Run States

| Numeric | Label   | Color |
|---------|---------|-------|
| 0       | Queued  | green |
| 1       | Running | blue  |
| 2       | Error   | red   |
| 9       | Paused  | brown |

### Response Envelope

```json
{"success": true, "value": { ... }}
{"success": false, "error": "message"}
```

Streaming mode (`limit=0`): `Content-Type: application/json-l`, each line is a
JSON object with an additional `category` field (`"summary"`, `"catalog"`, or
`"history"`). The summary line comes first, followed by catalog entries, then
history entries.

## Subcommands

### 1. `ia tasks [identifier]` (bare — list tasks)

The default and most common operation. Lists tasks with smart defaults matching
the Python `ia tasks` behavior.

**Default behavior:**
- Bare `ia tasks` — shows the current user's queued/running tasks (catalog only,
  `submitter=<user_email>, catalog=1, history=0`). The user's email is obtained
  via `whoami()` from `auth.rs`.
- `ia tasks <identifier>` — shows catalog + history for the given item
  (`identifier=X, catalog=1, history=1`)

**Flags:**

| Flag | Type | Description |
|------|------|-------------|
| `--cmd` | `String` | Filter by task command (e.g. `derive.php`) |
| `--submitter` | `String` | Filter by submitter email |
| `--server` | `String` | Filter by server name |
| `--priority` | `i32` | Filter by priority |
| `--args` | `String` | Filter by args (supports wildcards `*`/`%`) |
| `--color` | `String` | Filter by status color (green/blue/red/brown) |
| `--limit` | `u32` | Cap number of results returned (server-side when `--no-summary`, client-side otherwise) |
| `--active-only` | `bool` | Only show active tasks (catalog, no history). Mutually exclusive with `--completed-only`. |
| `--completed-only` | `bool` | Only show completed tasks (history, no catalog). Requires identifier or `--parameter task_id=N`. Mutually exclusive with `--active-only`. |
| `--task-id` | `u64` | Filter by specific task ID |
| `--since` | `String` | Show tasks submitted after this date/time |
| `--before` | `String` | Show tasks submitted before this date/time |
| `--no-summary` | `bool` | Hide the summary counts header |
| `-p, --parameter` | `Vec<String>` | Raw API parameter `KEY=VALUE` (repeatable). Escape hatch for undocumented/new API features. |
| `--json` | `bool` | Output as JSONL (one JSON object per task) |

**Mutual exclusion:** `--active-only` and `--completed-only` are enforced at
runtime with `bail!()` and a helpful error message, consistent with the existing
`--json`/`--dashboard` pattern in `download.rs`. If `--completed-only` is
passed without an identifier (and no `task_id` via `--parameter`), `bail!()`
with: "Error: --completed-only requires an identifier or task_id parameter".

**Pagination:** Transparent to the user. Under the hood, use the streaming JSONL
endpoint (`limit=0`) to fetch all results in one request. When `--limit` is
combined with `--no-summary`, pass `limit` to the server directly (avoids
fetching unnecessary data). Otherwise, `--limit` performs client-side truncation
after streaming (because streaming is needed for accurate summary counts).

**Output format:**
- **Human (default):** Summary header line above a table. Summary line format:
  `Queued: N  Running: N  Error: N  Paused: N` (color-coded). Hidden when all
  counts are zero (or when `--no-summary` is passed). Table columns: `task_id`,
  `identifier`, `cmd`, `status` (color-coded), `submittime`, `server`.
- **`--json`:** Full response structure including summary and tasks array. Each
  task is a JSON object on its own line.
- When no tasks are found: print "No tasks found." (human) or empty array (JSON).

**Example help (`--help`):**
```
List your pending tasks:
  ia tasks

List tasks for an item (active + completed):
  ia tasks my-item

Filter by command:
  ia tasks --cmd derive.php

Show only completed tasks for an item:
  ia tasks my-item --completed-only

Pass raw API parameters:
  ia tasks -p history=1 -p task_id=101247325
```

### 2. `ia tasks submit <cmd> [identifier]`

Submit a new task to the Tasks API.

**Positional args:**
- `<cmd>` — Task command, e.g. `derive` or `derive.php` (auto-appends `.php` if
  not present). Free-form string — no validation against a hardcoded list, since
  the API adds new commands and has undocumented commands.
- `[identifier]` — Item identifier (required for single mode, omitted for batch
  mode with `--itemlist`/`--search`).

**Flags:**

| Flag | Type | Description |
|------|------|-------------|
| `--args` | `Vec<String>` | Task arguments as `KEY=VALUE` (repeatable) |
| `--comment` | `String` | Explanation for why the task is being submitted |
| `--priority` | `i32` | Task priority (-10 to 10, default 0) |
| `--reduced-priority` | `bool` | Submit at reduced priority to avoid rate-limiting |
| `--wait` | `bool` | Poll until task completes, exit code reflects success/error |
| `--wait-interval` | `u64` | Initial poll interval in seconds for `--wait` (default: 2). Uses exponential backoff, capped at 60s between polls. |
| `--max-retries` | `u32` | Max retries on 429 rate-limit responses (default: 10) |
| `-p, --parameter` | `Vec<String>` | Raw API parameter `KEY=VALUE` (repeatable). Escape hatch for undocumented/new API features. Merged into the POST body. |
| `--json` | `bool` | Output as JSON |

**Batch mode flags (uniform args applied to all items):**

| Flag | Type | Description |
|------|------|-------------|
| `--itemlist` | `PathBuf` | File with one identifier per line |
| `--search` | `String` | Search query; submit task to all matching items |

In batch mode, `<identifier>` is omitted (items come from `--itemlist`/
`--search`). The `<cmd>`, `--args`, `--comment`, `--priority`, and
`--reduced-priority` flags apply uniformly to all items. Global `--jobs`,
`--joblog`, and `--retry-failed` options control concurrency, logging, and
retry.

**Success output (single):**
```
Task submitted: 1234567
Log: https://catalogd.archive.org/log/1234567
```
With `--json`: `{"success": true, "value": {"task_id": 1234567, "log": "..."}}`

**`--wait` behavior:**
- `--wait` is not supported in batch mode — an error is returned with a helpful
  message.
- After submission, poll the task's status using exponential backoff starting
  from `--wait-interval` (default 2s), capped at 60s between polls.
- Show a spinner with current status.
- On completion: print final status and exit 0 for success, non-zero for error.
- No maximum total wait time — tasks can legitimately take hours. The user can
  Ctrl+C to abort.
- Can optionally combine with `--json` for machine-readable final status.

**429 rate-limit handling:**
- Auto-retry with `Retry-After` header if present, exponential backoff if not
  (starting from 2s, capped at 60s).
- Display helpful info: "Rate limited. In-flight: N/M. Retrying in Ns..."
- Cap retries with `--max-retries` (default 10).

**Example help (`--help`):**
```
Submit a derive task:
  ia tasks submit derive my-item

Submit with a comment:
  ia tasks submit make_dark my-item --comment "curation request"

Submit to multiple items:
  ia tasks submit derive --itemlist items.txt --comment "re-derive"

Submit with custom args:
  ia tasks submit derive my-item --args remove_derived="*.jpg"
```

### 3. `ia tasks log <task_id>`

Fetch and display the execution log for a task.

**Positional args:**
- `<task_id>` — Task ID (integer, required)

**Flags:**

| Flag | Type | Description |
|------|------|-------------|
| `--json` | `bool` | Output as `{"task_id": N, "log": "..."}` (raw literal log) |

**Output behavior:**
- **TTY (terminal):** Light colorization of the log text (e.g. timestamps,
  error keywords). The log content varies and is not consistently formatted, so
  colorization should be conservative — highlight obvious patterns without
  mangling unexpected content.
- **Non-TTY (pipe/redirect):** Raw literal log text, byte-for-byte as received
  from the server. This is critical — users pipe task logs to `grep`, save to
  files for analysis, etc.
- **`--json`:** `{"task_id": <id>, "log": "<raw literal log>"}`. The `log`
  field contains the raw text, not colorized.

**Implementation note:** The task log endpoint is at `catalogd.archive.org`
(requests to `archive.org` get a 301 redirect). We should hit `catalogd`
directly.

**Example help (`--help`):**
```
View a task log:
  ia tasks log 1234567

Save a task log to a file:
  ia tasks log 1234567 > task.log

Output as JSON:
  ia tasks log 1234567 --json
```

### 4. `ia tasks rerun <task_id>...`

Rerun failed tasks (error/red status only). Uses PUT with
`{"op": "rerun", "task_id": N}`.

**Positional args:**
- `<task_id>...` — One or more task IDs (variadic). Also accepts `-` to read
  from stdin.

**Stdin parsing:** When reading from stdin (`-`), each line is parsed as:
1. Raw integer: `1234567` — used directly as task ID.
2. JSONL with `task_id` field: `{"task_id": 1234567, ...}` — extracts `task_id`.
3. Blank lines and lines starting with `#` are skipped.

This enables composable pipelines like:
```
ia tasks --cmd derive.php --color red --json | ia tasks rerun -
```

**Query filter flags (built-in batch rerun):**

Note: `--identifier` is a *flag* (not positional) specifically for `rerun`,
because the positional args are reserved for task IDs.

| Flag | Type | Description |
|------|------|-------------|
| `--cmd` | `String` | Rerun all failed tasks matching this command |
| `--color` | `String` | Filter by color (default: `red` when using query filters) |
| `--submitter` | `String` | Filter by submitter |
| `--identifier` | `String` | Filter by identifier |
| `--max-retries` | `u32` | Max retries per rerun request on failure |
| `--json` | `bool` | Output as JSON |

When query filters are provided (instead of explicit task IDs), the command
first lists matching tasks, then reruns each one. This is equivalent to piping
`ia tasks --json | ia tasks rerun -` but more convenient.

**Output:**
- Human: `Rerun task 1234567 (my-item)` per task, summary at end.
- JSON: `{"task_id": 1234567, "identifier": "my-item", "success": true}` per line.

**Example help (`--help`):**
```
Rerun a single failed task:
  ia tasks rerun 1234567

Rerun multiple tasks:
  ia tasks rerun 1234567 1234568 1234569

Rerun all failed derive tasks:
  ia tasks rerun --cmd derive.php

Rerun from a pipeline:
  ia tasks --cmd derive.php --color red --json | ia tasks rerun -
```

### 5. `ia tasks rate-limit [cmd]`

Check task submission rate limits.

**Positional args:**
- `[cmd]` — Task command to check (default: `derive.php`). Auto-appends `.php`.

**Flags:**

| Flag | Type | Description |
|------|------|-------------|
| `--json` | `bool` | Output as JSON |

**Output (human):**
```
Command:        derive.php
Task limit:     500
In-flight:      120
Blocked:        0
```

**Output (JSON):**
```json
{"cmd": "derive.php", "task_limits": 500, "tasks_inflight": 120, "tasks_blocked_by_offline": 0}
```

**Example help (`--help`):**
```
Check derive rate limits (default):
  ia tasks rate-limit

Check a specific command:
  ia tasks rate-limit make_dark
```

## Core Library Changes (`ia-core/src/tasks.rs`)

The existing module has `get_tasks()`, `TasksQuery`, `TasksResponse`,
`TasksValue`, `TasksSummary`, and `TaskEntry`. This design expands it
significantly.

### Error Handling

New `IaError` variants in `error.rs`:

```rust
/// Task submission failed (non-retryable API error).
#[error("task submission failed: {message}")]
TaskSubmitFailed { message: String },

/// Task rerun failed.
#[error("task rerun failed for task {task_id}: {message}")]
TaskRerunFailed { task_id: u64, message: String },

/// Task log not found.
#[error("task log not found for task {task_id}")]
TaskNotFound { task_id: u64 },
```

Rate limiting (429) is handled by the existing `IaError::Http` variant with
status 429. The retry logic lives in the CLI layer (or a helper in core),
not in the error type itself. `is_retryable()` returns `true` for status 429.

### New/Modified Types

All new types derive `Debug, Clone`. Types that appear in `--json` output
also derive `Serialize`. Types deserialized from the API derive `Deserialize`.

```rust
/// Extended query — add all API filter fields.
#[derive(Debug, Default, Clone)]
pub struct TasksQuery {
    pub identifier: Option<String>,
    pub cmd: Option<String>,
    pub limit: Option<u32>,
    pub submitter: Option<String>,
    pub args: Option<String>,
    pub server: Option<String>,
    pub priority: Option<i32>,
    pub color: Option<String>,
    pub task_id: Option<u64>,
    pub submittime_after: Option<String>,
    pub submittime_before: Option<String>,
    pub catalog: Option<bool>,     // include active tasks
    pub history: Option<bool>,     // include completed tasks
    pub summary: Option<bool>,     // include summary counts
    pub extra_params: Vec<(String, String)>,  // --parameter escape hatch
}

/// Extended task entry — add fields from history responses.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskEntry {
    pub task_id: u64,
    pub identifier: String,
    pub cmd: String,
    #[serde(default)]
    pub submitter: String,
    #[serde(default)]
    pub submittime: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub args: Option<serde_json::Value>,  // task arguments (varies)
    #[serde(default)]
    pub category: Option<String>,         // "catalog" or "history" (streaming)
    #[serde(default)]
    pub finished: Option<u64>,            // completion time (history only)
}

/// Task submission request.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSubmission {
    pub identifier: String,
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<HashMap<String, String>>,
    #[serde(skip)]
    pub comment: Option<String>,  // goes into args.comment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip)]
    pub reduced_priority: bool,
    #[serde(skip)]
    pub extra_params: Vec<(String, String)>,
}

/// Task submission response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskSubmitResponse {
    pub task_id: u64,
    pub log: String,
}

/// Rate limit info.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitInfo {
    pub cmd: String,
    pub task_limits: u32,
    pub tasks_inflight: u32,
    pub tasks_blocked_by_offline: u32,
}
```

### New Functions

```rust
/// List tasks using streaming JSONL (limit=0).
/// Parses the response line-by-line. Each line is a JSON object with a
/// `category` field. Summary lines are merged into `TasksSummary`. Catalog
/// and history entries are collected into a flat `Vec<TaskEntry>` (the
/// `category` field on each entry preserves the distinction for callers
/// that need it, e.g. --active-only / --completed-only filtering).
/// Malformed lines are skipped with a debug log.
pub async fn list_tasks(
    client: &IaClient,
    query: &TasksQuery,
) -> Result<(TasksSummary, Vec<TaskEntry>)>;

/// Submit a task (POST).
/// Comment is merged into args.comment before sending.
/// If reduced_priority is set, adds the `X-Accept-Reduced-Priority: 1` header.
pub async fn submit_task(
    client: &IaClient,
    submission: &TaskSubmission,
) -> Result<TaskSubmitResponse>;

/// Rerun a failed task (PUT).
/// Sends {"op": "rerun", "task_id": N}.
pub async fn rerun_task(
    client: &IaClient,
    task_id: u64,
) -> Result<()>;

/// Fetch task log (GET with task_log param).
/// Returns raw text. Hits catalogd.archive.org directly (avoids 301
/// redirect from archive.org).
pub async fn get_task_log(
    client: &IaClient,
    task_id: u64,
) -> Result<String>;

/// Check rate limits for a command.
/// Uses GET with rate_limits=1&cmd=<cmd>.
pub async fn get_rate_limit(
    client: &IaClient,
    cmd: &str,
) -> Result<RateLimitInfo>;

/// Poll a task until completion (for --wait).
/// Uses exponential backoff from initial_interval, capped at 60s.
/// No total timeout — caller can cancel via tokio CancellationToken
/// or Ctrl+C.
pub async fn wait_for_task(
    client: &IaClient,
    task_id: u64,
    initial_interval: Duration,
) -> Result<Option<TaskEntry>>;
```

### Existing `get_tasks()` Function

The existing `get_tasks()` is currently used by the upload dashboard TUI. It
will be migrated to use `list_tasks()` internally, since `list_tasks()` is a
strict superset. The old `TasksValue` type can be removed once the TUI is
updated.

### Streaming JSONL Parsing

The `list_tasks()` function uses `reqwest::Response::bytes_stream()` to read
the response incrementally. Lines are split on `\n` and each parsed as a JSON
object via `serde_json::from_str`. The `category` field determines handling:

- `"summary"` → merge into `TasksSummary`
- `"catalog"` or `"history"` → deserialize as `TaskEntry`, push to result vec
- Malformed or unrecognized lines → skip with `tracing::debug!` log

The summary line always comes first in the API response, so the summary is
available before task entries start arriving. However, since `list_tasks()`
collects all entries before returning, this ordering detail is an
implementation note, not a caller-visible guarantee.

## CLI Module (`ia-cli/src/commands/tasks.rs`)

New file following the existing command module pattern. Subcommand routing via
clap's subcommand derive. Layered help: `-h` (terse) vs `--help` (long with
examples using `color-print` crate's `cstr!` macro for `after_long_help`).

### Batch Infrastructure

Submit batch mode reuses the project's existing batch patterns:
- `--itemlist` / `--search` for item sources (same as download/upload)
- Global `--jobs` for concurrency
- Global `--joblog` for JSONL append-only logging
- Global `--retry-failed` to retry from joblog
- `BatchSummary` / `BatchDisplay` from `output.rs` for progress

### 429 Retry

Auto-retry on 429 with:
1. `Retry-After` header value if present
2. Exponential backoff if not (starting from 2s, capped at 60s)
3. Display: "Rate limited. In-flight: N/M. Retrying in Ns..."
4. `--max-retries` to cap (default 10)

## Documentation Updates

When this feature is implemented:
- Update `docs/usage.md` with `ia tasks` section
- Update `README.md` command list
- GitHub Release notes cover the new command

## Testing Strategy

All write operations (POST/PUT) MUST use wiremock mocks. Read operations also
use wiremock. No live API calls in tests.

### ia-core Unit/Integration Tests

- **list_tasks:** query by identifier, bare (submitter default), with filters,
  empty results, streaming JSONL parsing (summary + catalog + history lines),
  malformed line handling, all query params passed correctly, extra_params
  escape hatch, catalog-only vs history-only via query params
- **submit_task:** successful submission, 429 rate limiting (with/without
  Retry-After), 403 permission error, 409 conflict (rename), success:false,
  auto-append .php, args serialization, comment merged into args, priority,
  reduced priority, extra_params
- **rerun_task:** successful rerun, non-error task rejection (API-side),
  multiple reruns, auth required
- **get_task_log:** successful log fetch, 404 not found, encoding handling,
  hits catalogd host
- **get_rate_limit:** successful response, auth required, different commands
- **wait_for_task:** task completes successfully, task errors, exponential
  backoff timing, cancellation

### ia-cli Integration Tests

- **Bare listing:** `ia tasks` output format, table rendering, summary header,
  `--no-summary`, `--json` output
- **Identifier listing:** `ia tasks <id>` with catalog+history, `--active-only`,
  `--completed-only`, `--completed-only` without identifier (error)
- **Mutual exclusion:** `--active-only` + `--completed-only` (error)
- **Filters:** `--cmd`, `--submitter`, `--color`, `--parameter`
- **Submit single:** success output, `--json`, `--wait`, error cases
- **Submit batch:** `--itemlist`, `--search`, `--jobs`, `--joblog`
- **Log:** TTY colorization, non-TTY raw output, `--json`
- **Rerun:** single ID, multiple IDs, stdin (raw + JSONL), query filters
- **Rate-limit:** default cmd, custom cmd, `--json`
- **Alias:** `ia ta` works

## Non-Goals (Out of Scope)

- Per-item args in batch mode (spreadsheet import) — can add later if needed
- Dashboard/TUI for tasks — the upload TUI already shows task counts
- Hardcoded command validation — free-form string, API validates
- `--wait` on batch submit — error with helpful message; users can script it
