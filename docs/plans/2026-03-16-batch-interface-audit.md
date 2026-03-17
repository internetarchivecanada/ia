# Batch Interface Audit & Unified Design Proposal

**Date:** 2026-03-16
**Status:** Proposal
**Breaking changes:** On the table

## Executive Summary

An 8-agent audit of every `ia` CLI command reveals systemic inconsistencies in
how batch input, global options, concurrency, output formatting, stdin handling,
and error reporting work across commands. While some differences are
domain-appropriate, many are accidental — the result of each command being
implemented independently without a shared interface contract.

This document catalogs every inconsistency found, proposes a unified batch
interface design, and identifies which breaking changes are worth making.

---

## Table of Contents

1. [Inconsistency Catalog](#1-inconsistency-catalog)
2. [Proposed Unified Interface](#2-proposed-unified-interface)
3. [Breaking Changes](#3-breaking-changes)
4. [Command-by-Command Remediation](#4-command-by-command-remediation)
5. [Global Options Contract](#5-global-options-contract)
6. [Progressive Disclosure Model](#6-progressive-disclosure-model)
7. [Appendix: Full Interface Map](#7-appendix-full-interface-map)
8. [Appendix: Audit Methodology](#8-appendix-audit-methodology)

---

## 1. Inconsistency Catalog

### 1.1 Batch Input: Five Patterns Where There Should Be Two

The CLI has accumulated at least five distinct batch input patterns. A user who
learns how `ia download` handles batch input cannot predict how `ia tasks
submit` or `ia metadata import` will work.

#### Pattern A: Full batch (positional + --itemlist + --search + stdin auto-detect)
Used by: `download`, `metadata modify/append/append-list/insert/remove`, `ai`

```bash
ia download item1 item2                           # positional
ia download --itemlist ids.txt                     # file
ia download --search "collection:nasa"             # search
echo item1 | ia download                           # stdin (auto-detected)
ia search --json "q" | ia download                 # JSONL stdin (auto-detected)
```

#### Pattern B: Partial batch (positional + --itemlist + --search, NO stdin)
Used by: `tasks submit`

```bash
ia tasks submit derive item1                       # positional
ia tasks submit derive --itemlist ids.txt           # file
ia tasks submit derive --search "collection:nasa"  # search
echo item1 | ia tasks submit derive                # DOES NOT WORK (no stdin)
```

**Why is stdin missing?** No technical reason. Just wasn't implemented.

#### Pattern C: Stdin via explicit `-` marker + filter-based discovery
Used by: `tasks rerun`

```bash
ia tasks rerun 123 456                             # positional task IDs
ia tasks rerun -                                   # stdin via explicit `-`
ia tasks rerun --cmd derive --color red             # filter-based (unique!)
```

This is the only command with filter-based batch discovery. Users must learn
three different batch mechanisms within the `tasks` subcommand alone.

#### Pattern D: Spreadsheet file only (no --itemlist, no --search, no stdin)
Used by: `upload import`, `metadata import`

```bash
ia upload import batch.csv                          # spreadsheet file
cat batch.csv | ia upload import                    # DOES NOT WORK
ia upload import --search "q"                       # DOES NOT WORK
```

**The rejection is appropriate** for XLSX/ODS (needs random access), but CSV/TSV
could theoretically work from stdin. The error message in metadata import is
helpful; upload import silently fails.

#### Pattern E: Single-item only (no batch at all)
Used by: `upload bare`, `collection create`, `list`, `tasks log`, `tasks
rate-limit`, `config *`, `status`, `update`

Some of these are correctly single-item (`list`, `tasks log`). Others could
benefit from batch: `collection create` and `upload bare` notably have no batch
equivalent — you can't create 50 collections without scripting a loop.

#### What's Wrong

| Issue | Commands Affected |
|-------|------------------|
| `tasks submit` has --itemlist and --search but no stdin | tasks submit |
| `tasks rerun` uses `-` marker instead of auto-detect | tasks rerun |
| `tasks rerun` has filter-based batch (unique, undiscoverable) | tasks rerun |
| `metadata export` positional args are FILES, not identifiers | metadata export |
| `metadata export` has --search but not --itemlist | metadata export |
| No `collection import` for batch collection creation | collection |
| `upload bare` has no batch equivalent beyond `upload import` | upload |
| stdin parsing differs: download parses JSONL, ai only does plain text | ai |

### 1.2 --json: Per-Command Flag With Inconsistent Semantics

Every command defines its own `--json` flag independently. This is not
inherently wrong (each command produces different JSON), but the implementations
are inconsistent:

| Issue | Detail |
|-------|--------|
| `metadata export --json` is redundant | Export already outputs JSONL to stdout by default. `--json` does nothing useful. |
| `metadata export --pretty` exists | No other command has `--pretty`. Why only export? |
| Collection/config/status/update define `--json` locally | While download/upload/metadata/tasks get it from their subcommand args structs. The flag is always local, but the *pattern* for where it's defined varies. |
| JSON output shape varies wildly | download: per-file objects. upload: per-file objects. metadata modify: per-item objects. tasks list: summary + per-task objects. tasks log: single object wrapper. search: per-result objects. No common envelope. |
| `--json` and `--dashboard` mutual exclusion | Only validated at runtime (line 343 in upload.rs), not via clap `conflicts_with`. Users see the error late. |
| No `--json` on `update` | Can't script update checks. |

#### What a Unified JSON Contract Would Look Like

Every command with `--json` should follow this contract:
- **Stdout**: One JSON object per line (JSONL), one per logical result unit
- **Stderr**: Human-readable status/progress (never JSON)
- **Envelope fields** shared across commands:
  ```json
  {"status": "ok|error|skipped|dry_run", "item": "identifier", ...command-specific fields...}
  ```
- **Error shape** consistent:
  ```json
  {"status": "error", "item": "id", "error": {"code": "not_found", "message": "..."}}
  ```

Currently, each command invents its own status field names (`success: true/false`
vs `status: "ok"/"error"` vs no status field).

### 1.3 --quiet: Different Level Counts Across Commands

`--quiet` is defined as a `u8` counter (`-q` = 1, `-qq` = 2, `-qqq` = 3), but
commands implement different numbers of levels:

| Command | Level 0 | Level 1 (-q) | Level 2 (-qq) | Notes |
|---------|---------|-------------|---------------|-------|
| download | progress bars + per-item | summary only | silent | 3 levels, well-implemented |
| upload bare | progress bars + per-item | summary only | ??? | Unclear if level 2 is silent or same as 1 |
| upload import | progress bars + per-item | summary only | ??? | Same ambiguity |
| upload template | always verbose | — | — | Ignores --quiet entirely |
| upload cleanup | always verbose | — | — | Ignores --quiet entirely |
| metadata modify | per-item status | suppresses dry-run msg | unclear | Inconsistent level semantics |
| metadata export | per-item + summary | summary only | silent | Decent |
| tasks list | table + summary | suppresses summary | silent | OK |
| tasks submit | per-item + summary | per-item only | silent | OK |
| tasks rerun | per-item + summary | per-item only | silent | OK |
| tasks log | full log | — | — | Ignores --quiet |
| tasks rate-limit | key-value | — | — | Ignores --quiet |
| search | table + summary | suppresses headers | unclear | Partial |
| list | table + summary | — | — | Doesn't clearly use quiet |
| collection create | text output | partial suppression | — | Only checks quiet==0 |
| ai | verbose | respects | respects | Good |
| config * | — | — | — | Ignores --quiet entirely |
| status | — | — | — | Ignores --quiet entirely |

**A user who learns `-q` means "summary only" and `-qq` means "silent" from
`ia download` will be surprised when `ia upload template -qq` still prints
verbose output.**

### 1.4 --jobs: Three Concurrency Models

| Command | What --jobs controls | Additional knobs | Notes |
|---------|---------------------|-----------------|-------|
| download | Files per item | `--items` (items concurrently) | TWO knobs, unique to download |
| upload import | Items concurrently | — | --jobs ignored in bare upload! |
| upload bare | — (ignored) | — | User passes --jobs, nothing happens |
| metadata | Items concurrently | — | Via semaphore |
| tasks submit | Submissions concurrently | — | Via buffer_unordered |
| tasks rerun | Reruns concurrently | — | Via buffer_unordered |
| ai | **SILENTLY IGNORED** | `--ai-jobs` (LLM concurrency) | User's --jobs does nothing |
| search | — | — | N/A (serial API) |
| list | — | — | N/A (single item) |

**Three problems:**
1. `upload bare` accepts `--jobs` and silently ignores it
2. `ai` accepts `--jobs` and silently ignores it (uses `--ai-jobs` instead)
3. `download` has `--items` which no other command has — the concept of
   "concurrent items in a batch" is expressed differently everywhere

### 1.5 --joblog and --retry-failed: Inconsistent Coverage

| Command | --joblog | --retry-failed | Notes |
|---------|---------|---------------|-------|
| download | writes per-file | reads failed items | Fully integrated |
| upload bare | writes per-file | **REJECTED** (error) | Can't retry bare upload from joblog |
| upload import | writes per-file | reads failed items | Fully integrated |
| metadata modify/append/etc | writes per-item | **MISSING** | Writes to joblog but can't read it back! |
| metadata export | **MISSING** | — | Batch operation with no tracking |
| metadata import | writes per-item | — | Writes but no retry |
| tasks submit | writes per-item | reads failed items | Fully integrated |
| tasks rerun | **MISSING** | — | Can't track rerun operations |
| ai | writes per-item | — | Writes but no retry |
| status | **own --joblog arg** | — | Doesn't use global --joblog |

**The metadata gap is the worst:** metadata write operations write to joblog but
`--retry-failed` isn't wired up. A user doing `ia metadata modify --search
"collection:nasa" -m subject:astronomy --joblog log.jsonl` and getting rate-
limited on 200 of 5000 items has no way to do
`ia metadata modify --retry-failed --joblog log.jsonl` — the flag simply
doesn't exist on metadata.

### 1.6 Stdin Handling: Three Conventions

| Convention | Commands | How it works |
|-----------|----------|-------------|
| Auto-detect | download, metadata write ops, metadata export, ai | If stdin is not a terminal and no other input source, read from stdin |
| Explicit `-` | tasks rerun, upload bare (for files, not IDs) | User must pass `-` as positional arg |
| Rejected | metadata import, upload import | Explicit error message |
| Not supported | tasks submit, tasks list, tasks log, tasks rate-limit, search, list, collection, config, status, update | Neither auto-detect nor `-` |

**Parsing also differs:**
- `download`: Parses both plain identifiers AND JSONL (`{"identifier":"..."}`)
  via `parse_identifier_line()` — this enables `ia search --json | ia download`
- `ai`: Only parses plain identifiers (no JSONL support) — so
  `ia search --json | ia ai` would feed JSON strings as identifiers, breaking
- `tasks rerun`: Parses both plain task IDs AND JSONL (`{"task_id": N}`) —
  enables `ia tasks list --json | ia tasks rerun -`
- `metadata write ops`: Plain identifiers only

### 1.7 Positional Argument Semantics: Same Position, Different Meaning

The first positional argument means different things across commands:

| Command | First positional | Second positional | Semantics |
|---------|-----------------|------------------|-----------|
| `download` | identifier (optional) | files (optional) | ID then files |
| `upload bare` | identifier (required) | files (required) | ID then files |
| `metadata modify` | identifiers (0+) | — | Multiple IDs |
| `metadata export` | **files** (0+) | — | Input FILES, not IDs! |
| `metadata import` | **file** (0-1) | — | Single input file |
| `tasks submit` | **command** (required) | identifier (optional) | CMD then ID |
| `tasks rerun` | task_ids (0+) | — | Task IDs, not item IDs |
| `tasks log` | task_id (required) | — | Single task ID |
| `tasks rate-limit` | cmd (optional, default "derive") | — | Command name |
| `search *` | query (required) | — | Query string |
| `list` | identifier (required) | — | Single ID |
| `collection create` | identifier (required) | — | Single ID |
| `ai` | identifiers (0+) | — | Multiple IDs |

**Surprising inconsistencies:**
- `metadata export` takes FILES as positional args, but `metadata modify` takes
  IDENTIFIERS. The same parent command (`ia metadata`) has opposite positional
  arg semantics between subcommands.
- `tasks submit` puts the COMMAND first and the identifier second — inverted
  from every other identifier-based command.
- `download` and `upload` both take `<id> [files]` but the identifier is
  optional in download (for batch mode) and required in upload.

### 1.8 Dry-Run: Inconsistent Availability and Output

| Command | --dry-run | What it shows | --dry-run + --json |
|---------|----------|--------------|-------------------|
| download | ✓ | Files as "skipped (dry run)" | ✓ (skipped results) |
| upload bare | ✓ | Metadata + file list + URL | ✓ (structured preview) |
| upload import | ✓ | Per-item grouped summary | ✓ (structured preview) |
| metadata modify | ✓ | "would_modify" status per item | ✓ (status objects) |
| metadata import | ✓ | Aggregated change count | ✓ (but different format from modify!) |
| tasks submit | ✓ | What would be submitted | ✓ (preview objects) |
| tasks rerun | **MISSING** | — | — |
| tasks log | — | N/A (read-only) | — |
| collection create | ✓ | Validation without sending | ✓ (preview) |
| ai | ✓ | Suggestions without applying | ✓ (preview) |

**`tasks rerun` is the gap** — you can dry-run a task submission but not a
task rerun. Since rerun can operate on hundreds of tasks via filters, a preview
would be valuable.

**Dry-run output format inconsistencies:**
- `metadata modify --dry-run` shows JSON patch operations per item
- `metadata import --dry-run` shows an aggregated "N items, M changes" summary
- Both are metadata write operations. Why different granularity?

### 1.9 --args Flag Overloading in Tasks

`tasks list` and `tasks submit` both have an `--args` flag, but with completely
different semantics:

| Command | --args meaning | Type | Example |
|---------|---------------|------|---------|
| `tasks list` | Filter pattern (wildcards) | String | `--args "reduce*"` |
| `tasks submit` | Task arguments (KEY=VALUE) | Repeatable structured | `--args key=value` |

A user who learns `--args` on one subcommand will be confused by the other.

### 1.10 Upload Flag Fragmentation

Flags available on `upload bare` that are MISSING from `upload import`:

| Flag | On bare | On import | Impact |
|------|---------|-----------|--------|
| `--remote-name` | ✓ | **missing** | Can't rename files in batch upload |
| `--remote-dir` | ✓ | **missing** | Can't set path prefix in batch |
| `--keep-directories` | ✓ | **missing** | Can't preserve dir structure in batch |
| `--open-after-upload` | ✓ | **missing** | Minor |

**The `REMOTE_NAME` spreadsheet column exists in templates** (batch.rs:136) but
is **stripped from metadata** — it's not used for anything. Users see the column,
fill it in, and it gets silently ignored.

### 1.11 Error Handling: Three Exit Patterns

| Pattern | Commands | How it works |
|---------|----------|-------------|
| `Result::Err` propagation | Most commands | Error bubbles to main, anyhow prints it |
| `process::exit(1)` on partial failure | download, status | Explicit exit after printing results |
| `process::exit(1)` on ANY error | config (check, whoami, print-*) | Hard exit, no error propagation |
| Mixed (exit for JSON, Result for text) | update | Different paths based on --json flag |

The config pattern (`process::exit(1)` everywhere) breaks testability and is
inconsistent with every other command.

### 1.12 Search: Scrape Hardcoded in All Batch Operations

Every command with `--search` (download, metadata, tasks, ai) hardcodes the
scrape backend. There's no way to use advanced search or FTS for batch:

```bash
# These all use scrape internally, regardless of query syntax:
ia download --search "collection:nasa AND mediatype:audio"
ia metadata modify --search "date:[2020 TO 2024]" -m subject:recent
ia tasks submit derive --search "collection:test"
```

Advanced search supports boolean operators, field weighting, and availability
filtering that scrape doesn't. FTS supports full-text content search.

### 1.13 Search Output: Advanced Artificially Limited

The advanced search CLI limits results to **a single page** (default 50 rows)
even though the core library supports multi-page pagination:

```rust
// search.rs:282-284 — CLI caps count to rows, enforcing single page
let count = rows;
```

A user running `ia search advanced "collection:nasa" --rows 1000` gets 1000
results but can never get 1001. The scrape and FTS backends have no such limit.
This isn't documented.

### 1.14 List: Missing Batch and Missing Filter Exposure

The `list` command supports `--glob` and `--source` for file filtering, but the
underlying `FileFilter` struct also supports `--exclude`, `--formats`,
`--exclude-source`, and `--names` — none of which are exposed in the CLI:

| FileFilter field | Exposed in `list`? | Exposed in `download`? |
|-----------------|-------------------|----------------------|
| glob | ✓ | ✓ |
| exclude | **no** | ✓ |
| formats | **no** | ✓ (`-f/--format`) |
| source | ✓ | ✓ |
| exclude_source | **no** | ✓ |
| names | **no** | ✓ (positional files) |

`download` exposes all filtering options; `list` exposes a subset. A user who
discovers `--format` on download can't use it on `list`.

### 1.15 Compound Operations: Metadata-Only Feature

The `+` compound operation syntax is unique to metadata write commands:

```bash
ia metadata modify myitem -m title:"New" + remove -m oldfield + append -m desc:"extra"
```

No other command supports chaining multiple operations. This is a powerful
feature that's invisible unless you read the full `--help` output.

---

## 2. Proposed Unified Interface

### 2.1 Batch Input: Two Canonical Patterns

**Rule:** Every command that operates on identifiers should support the same
four input sources, in the same priority order. Every command that needs
structured per-item data should use an `import` subcommand.

#### Pattern 1: Identifier Batch (for all identifier-based ops)

```
IDENTIFIERS = positional [ids...]
            | --itemlist FILE    (one per line, supports JSONL)
            | --search QUERY     (scrape by default)
            | stdin              (auto-detected when not a terminal)
```

**Shared implementation:** Extract a `collect_identifiers()` function into a
shared module. Currently each command reimplements this. The function should:
- Accept all four sources
- Parse both plain text and JSONL (`{"identifier": "..."}`)
- Skip blank lines and `#` comments
- Deduplicate while preserving order
- Return `Vec<String>`

**Commands that need changes:**

| Command | Current | Change needed |
|---------|---------|--------------|
| tasks submit | No stdin | Add stdin auto-detect |
| metadata export | Positional = files, not IDs | Keep positional as files, but add --itemlist |
| ai | Stdin only parses plain text | Add JSONL parsing |
| tasks rerun | Uses `-` marker | **Breaking:** switch to auto-detect (see 3.1) |

#### Pattern 2: Structured Import (for per-item customization)

```
ia <command> import <spreadsheet>    (CSV/TSV/XLSX/ODS/JSONL)
```

**Commands with import:** upload import, metadata import.

**Commands that should gain import:**
- `collection import` — batch-create collections from spreadsheet
- `tasks submit import` — batch-submit tasks with per-item args/priority/comment

### 2.2 Concurrency: One Model, Clear Semantics

**Rule:** `--jobs` always means "maximum concurrent operations." Its scope
depends on the command:

| Command | --jobs scope | Notes |
|---------|-------------|-------|
| download | concurrent file downloads | Per-item files; use `--items` for items |
| upload import | concurrent item uploads | Per-batch items |
| upload bare | concurrent file uploads | Currently ignored — FIX |
| metadata * | concurrent metadata ops | Per-identifier |
| tasks submit | concurrent submissions | Per-identifier |
| tasks rerun | concurrent reruns | Per-task |
| ai | concurrent LLM requests | **Breaking:** replaces `--ai-jobs` |

**Changes:**
- `upload bare`: Use `--jobs` for concurrent file uploads within the item
  (currently sequential)
- `ai`: Drop `--ai-jobs`, use `--jobs` directly. If both are set, error.

**Download's `--items` stays** — it's the only command with genuine two-level
concurrency (files-within-items vs items-in-batch). But rename the help text to
make the distinction clearer.

### 2.3 Quiet Levels: Three Levels, Everywhere

**Rule:** Every command that produces output should respect `--quiet`:

| Level | Behavior | What to show |
|-------|----------|-------------|
| 0 (default) | Verbose | Progress bars, per-item status, tables, summaries |
| 1 (`-q`) | Summary | One-line result summary, errors to stderr |
| 2+ (`-qq`) | Silent | Nothing to stderr, exit code only. JSON still to stdout if `--json`. |

**Commands that need fixes:**

| Command | Fix needed |
|---------|-----------|
| upload template | Add quiet support (suppress success message) |
| upload cleanup | Add quiet support |
| tasks log | Add quiet support (suppress colorized log at -q, nothing at -qq) |
| tasks rate-limit | Add quiet support |
| collection create | Fix: currently only checks quiet==0, needs level 2 |
| config * | Add quiet support where meaningful (check, whoami) |
| status | Add quiet support |

### 2.4 JSON Output: Per-Command With Shared Conventions

`--json` stays per-command (not global). But all JSON output should follow
shared conventions:

**JSONL contract (batch operations):**
```json
{"status": "ok", "identifier": "myitem", ...command-specific fields...}
{"status": "error", "identifier": "other", "error": {"code": "rate_limited", "message": "429 Too Many Requests"}}
{"status": "skipped", "identifier": "third", "reason": "already_exists"}
{"status": "dry_run", "identifier": "fourth", ...preview fields...}
```

**Required fields:** `status` (always), `identifier` or `item` (for item ops),
`task_id` (for task ops).

**Error shape:** Always `{"code": "snake_case", "message": "human-readable"}`.

**Current violations:**
- tasks submit uses `"success": true/false` instead of `"status": "ok"/"error"`
- download uses `"status": "ok"` but metadata modify uses `"status": "ok"` with
  different surrounding fields
- tasks rerun uses `"success": true/false`
- metadata export `--json` is redundant (already JSONL) — remove the flag or
  make it control compact vs pretty

### 2.5 --retry-failed: Everywhere That --joblog Writes

**Rule:** If a command writes to `--joblog`, it should also support
`--retry-failed`.

| Command | --joblog writes | --retry-failed | Fix |
|---------|----------------|---------------|-----|
| download | ✓ | ✓ | — |
| upload import | ✓ | ✓ | — |
| upload bare | ✓ | rejected | Add (retry the same ID+files) |
| metadata write ops | ✓ | **MISSING** | Add |
| metadata import | ✓ | — | Add |
| tasks submit | ✓ | ✓ | — |
| tasks rerun | missing | — | Add --joblog, then add --retry-failed |
| ai | ✓ | — | Add |

### 2.6 Dry-Run: Available on All Write Commands

**Rule:** Every command that modifies server state should support `--dry-run`.

| Command | Has --dry-run | Fix |
|---------|-------------|-----|
| download | ✓ | — |
| upload bare | ✓ | — |
| upload import | ✓ | — |
| upload cleanup (abort) | no | Add (show what would be aborted) |
| metadata modify/append/etc | ✓ | — |
| metadata import | ✓ | — |
| tasks submit | ✓ | — |
| tasks rerun | **MISSING** | Add (show tasks that would be rerun) |
| collection create | ✓ | — |
| ai | ✓ | — |

### 2.7 Upload Import: Expose Missing Flags

Flags on `upload bare` that should also be on `upload import`:

| Flag | Proposal |
|------|---------|
| `--remote-dir` | Add to import — applies as prefix to all remote keys |
| `--keep-directories` | Add to import — preserve dir structure |
| `--open-after-upload` | Skip — doesn't make sense for batch |
| `--remote-name` | Skip — use REMOTE_NAME column instead, but MAKE IT WORK |

**Critical fix:** The `REMOTE_NAME` spreadsheet column is currently stripped and
ignored (batch.rs:136). It should be used as the S3 key for that file. Users
already see it in templates and fill it in.

### 2.8 List: Expose All File Filters

Match `download`'s filter options on `list`:

```bash
ia list myitem --glob "*.mp4|*.webm"           # already works
ia list myitem --exclude "*.xml|*.sqlite"       # ADD (FileFilter.exclude)
ia list myitem --format mp4 --format webm       # ADD (FileFilter.formats)
ia list myitem --exclude-source metadata         # ADD (FileFilter.exclude_source)
```

The underlying `FileFilter` struct already supports all of these — they just
need CLI flag definitions.

### 2.9 Search: Expose Backend Selection on --search

Add an optional `--search-backend` flag to commands that accept `--search`:

```bash
ia download --search "complex query" --search-backend advanced
ia metadata modify --search "content:keyword" --search-backend fts -m tag:found
```

Default remains `scrape`. This is additive, no breaking change.

Also: **lift the single-page limit on advanced search CLI.** The core library
supports multi-page; the CLI artificially caps at one page. Either auto-paginate
like scrape/FTS, or add a `--pages` flag.

---

## 3. Breaking Changes

### 3.1 tasks rerun: Switch from `-` to stdin auto-detect

**Current:** `ia tasks list --json | ia tasks rerun -`
**Proposed:** `ia tasks list --json | ia tasks rerun`

The `-` marker is unnecessary — if stdin is piped and no positional task IDs are
provided, read from stdin. This matches download, metadata, and ai.

**Migration:** Warn on `-` for one release ("note: `-` is no longer needed for
stdin, it's auto-detected"), then remove.

**Risk:** Low. The `-` convention is undiscoverable; most users pipe without it.

### 3.2 ai: Drop --ai-jobs, use --jobs

**Current:** `ia ai --ai-jobs 4 myitem`
**Proposed:** `ia ai --jobs 4 myitem` (or global `-j 4`)

**Migration:** Accept both for one release with deprecation warning, then
remove `--ai-jobs`.

**Risk:** Low. AI command is relatively new.

### 3.3 tasks submit: Add stdin auto-detect

**Current:** Only positional + --itemlist + --search
**Proposed:** Also auto-detect stdin

```bash
ia search --itemlist "collection:broken" | ia tasks submit derive
```

**Risk:** None. Additive behavior — only activates when no other input source is
provided and stdin is piped.

### 3.4 Standardize JSON status field

**Current:**
- tasks submit/rerun: `{"success": true/false, ...}`
- download/metadata/upload: `{"status": "ok"/"error", ...}`

**Proposed:** All commands use `{"status": "ok"/"error"/"skipped"/"dry_run"}`.

**Migration:** **Breaking** for anyone parsing `tasks submit --json` output.
Add `"success"` as a deprecated alias for one release.

### 3.5 metadata export: Remove redundant --json flag

**Current:** `ia metadata export items.txt --json` outputs JSONL (but so does
bare `ia metadata export items.txt`)

**Proposed:** Remove `--json` from export. Default output is JSONL to stdout.
`-o file.csv` outputs CSV. `--pretty` controls JSON formatting.

**Risk:** Low. The flag currently does nothing useful.

---

## 4. Command-by-Command Remediation

### download
- **Status:** Well-integrated, reference implementation for batch
- **Fixes:** None needed
- **Notes:** Two-level concurrency (--jobs + --items) is appropriate

### upload bare
- **Fixes:**
  - Use `--jobs` for concurrent file uploads (currently ignored)
  - Consider allowing `--retry-failed` (retry same item+files from joblog)

### upload import
- **Fixes:**
  - Add `--remote-dir` flag
  - Add `--keep-directories` flag
  - Make `REMOTE_NAME` column functional (currently stripped)
  - Ensure `--quiet` has 3 levels

### upload template
- **Fixes:**
  - Respect `--quiet` (suppress "Wrote N rows" message)
  - Validate generated identifiers with `validate_identifier()`

### upload cleanup
- **Fixes:**
  - Respect `--quiet`
  - Add `--dry-run` (show what would be aborted)

### metadata modify/append/append-list/insert/remove
- **Fixes:**
  - Add `--retry-failed` support
  - Standardize dry-run output granularity (currently differs between modify
    and import)

### metadata export
- **Fixes:**
  - Add `--joblog` support
  - Add `--itemlist` for identifier input
  - Remove or repurpose `--json` flag (redundant)

### metadata import
- **Fixes:**
  - Add `--retry-failed` support
  - Document stdin rejection clearly

### metadata schema
- **Status:** Appropriately single-item, no fixes

### tasks list
- **Fixes:**
  - Rename `--args` to `--args-filter` or `--task-args` to avoid overloading
    with `tasks submit --args`

### tasks submit
- **Fixes:**
  - Add stdin auto-detect for identifiers
  - Add `tasks submit import <spreadsheet>` for batch with per-item args

### tasks rerun
- **Fixes:**
  - Switch to stdin auto-detect (breaking, see 3.1)
  - Add `--dry-run` (show tasks that would be rerun)
  - Add `--joblog` support

### tasks log
- **Fixes:**
  - Add `--quiet` support (suppress at -q, silent at -qq)

### tasks rate-limit
- **Fixes:**
  - Add `--quiet` support

### search (scrape/advanced/fts)
- **Fixes:**
  - Lift single-page limit on advanced CLI
  - Add `--field` support to FTS backend
  - Add `--search-backend` to batch commands that use --search

### list
- **Fixes:**
  - Expose `--exclude`, `--format`, `--exclude-source` (already in FileFilter)

### collection create
- **Fixes:**
  - Add `collection import <spreadsheet>` subcommand
  - Fix `--quiet` to support 3 levels

### config
- **Fixes:**
  - Replace `process::exit(1)` with `Result::Err` (~6 sites)
  - Add `--quiet` to check/whoami

### status
- **Fixes:**
  - Use global `--joblog` instead of own `--joblog` arg
  - Add `--quiet` support

### update
- **Fixes:**
  - Fix mixed exit pattern (exit(1) for JSON, Result::Err for text)
  - Add `--json` to `update` (for scripting update checks)

### ai
- **Fixes:**
  - Drop `--ai-jobs`, use `--jobs` (breaking, see 3.2)
  - Add JSONL parsing to stdin (currently only plain text)
  - Add `--retry-failed` support

---

## 5. Global Options Contract

Every new command should be checked against this contract:

### Required for all commands

| Option | Requirement |
|--------|------------|
| `--quiet` | Respect 3 levels (0=verbose, 1=summary, 2=silent) |
| `--json` | Per-command flag, follow shared JSONL conventions |

### Required for batch/write commands

| Option | Requirement |
|--------|------------|
| `--jobs` | Control concurrency. Document what it parallelizes. |
| `--joblog` | Track results per logical unit of work. |
| `--retry-failed` | If `--joblog` is supported, `--retry-failed` must be too. |
| `--dry-run` | Preview what would happen without side effects. |

### stdin contract

| Condition | Behavior |
|-----------|---------|
| stdin is a terminal | Ignore stdin |
| stdin is piped + no other input source | Read stdin |
| stdin is piped + other input sources present | Ignore stdin (other sources take priority) |
| stdin format | Parse as plain text (one per line) or JSONL (extract `identifier` field) |
| Blank lines | Skip |
| Lines starting with `#` | Skip (comments) |

### Shared implementation

Extract into `ia-cli/src/input.rs`:

```rust
pub fn collect_identifiers(
    positional: &[String],
    itemlist: Option<&Path>,
    search: Option<&str>,
    client: &IaClient,
) -> Result<Vec<String>>
```

This replaces the ~5 independent implementations currently scattered across
download.rs, metadata.rs, tasks.rs, ai.rs.

---

## 6. Progressive Disclosure Model

### Level 1: Single Item (new user)
```bash
ia download myitem
ia upload myitem file.pdf -m mediatype:texts
ia metadata modify myitem -m title:"Better Title"
ia tasks submit derive myitem
ia list myitem
```

Clean, simple. No mention of batch, joblog, or retry in basic `--help`.

### Level 2: Multiple Items (intermediate)
```bash
ia download --search "collection:nasa"
ia metadata modify item1 item2 item3 -m subject:astronomy
ia tasks submit derive --itemlist ids.txt
echo item1 | ia download
```

User discovers batch via `--help` long form or documentation.

### Level 3: Tracked Batch (power user)
```bash
ia upload import batch.csv --jobs 4 --joblog upload.jsonl
ia metadata import corrections.xlsx --dry-run
ia upload import batch.csv --retry-failed --joblog upload.jsonl
ia status --joblog upload.jsonl
```

Joblog tracking, retry, dry-run preview. Discoverable via `--help` or
`ia status` output hints.

### Level 4: Pipeline Composition (expert)
```bash
# Search → download pipeline
ia search "collection:nasa AND date:[2020 TO *]" --json \
  | ia download --jobs 4 --joblog dl.jsonl

# Rerun failed tasks
ia tasks list --cmd derive --color red --json | ia tasks rerun

# Export → edit → reimport workflow
ia metadata export --search "collection:mine" -o metadata.csv
# ... edit in spreadsheet editor ...
ia metadata import metadata.csv --dry-run
ia metadata import metadata.csv --joblog import.jsonl

# Batch collection creation
ia collection import collections.csv
```

Piping between commands. JSON as interchange format. Import subcommands for
structured batch.

---

## 7. Appendix: Full Interface Map

### Batch Input Methods

| Command | Positional | --itemlist | --search | stdin | Spreadsheet |
|---------|-----------|-----------|---------|-------|-------------|
| download | `<id> [files]` | ✓ | ✓ | auto | — |
| upload bare | `<id> <files>` | — | — | `-` (file data) | — |
| upload import | — | — | — | — | `<file>` |
| upload template | `<dir>` | — | — | — | — (generates) |
| upload cleanup | `<id> [file]` | — | — | — | — |
| metadata (bare) | `[ids]` | — | — | — | — |
| metadata export | `[files]` | ✗ ADD | ✓ | auto | — |
| metadata modify | `[ids]` | ✓ | ✓ | auto | — |
| metadata append | `[ids]` | ✓ | ✓ | auto | — |
| metadata append-list | `[ids]` | ✓ | ✓ | auto | — |
| metadata insert | `[ids]` | ✓ | ✓ | auto | — |
| metadata remove | `[ids]` | ✓ | ✓ | auto | — |
| metadata import | — | — | — | — | `<file>` |
| metadata schema | `[field]` | — | — | — | — |
| tasks list | `[id]` | — | — | — | — |
| tasks submit | `<cmd> [id]` | ✓ | ✓ | ✗ ADD | — |
| tasks rerun | `[task_ids]` | — | — | `-` → auto | — |
| tasks log | `<task_id>` | — | — | — | — |
| tasks rate-limit | `[cmd]` | — | — | — | — |
| search * | `<query>` | — | — | — | — |
| list | `<id>` | — | — | — | — |
| collection create | `<id>` | — | — | — | — |
| ai | `[ids]` | ✓ | ✓ | auto | — |

### Global Options Coverage

```
                  quiet   jobs    joblog  retry   stdin   dry-run  json
download           ✓(3)    ✓       ✓       ✓      auto     ✓       ✓
upload bare        ✓(2→3)  ✗→✓     ✓       ✗→✓    -(file)  ✓       ✓
upload import      ✓(2→3)  ✓       ✓       ✓      —        ✓       ✓
upload template    ✗→✓     —       —       —      —        —       ✓
upload cleanup     ✗→✓     —       —       —      —        ✗→✓     ✓
metadata modify    ✓       ✓       ✓       ✗→✓    auto     ✓       ✓
metadata export    ✓       ✓       ✗→✓     —      auto     —       repurpose
metadata import    ✓       ✓       ✓       ✗→✓    —        ✓       ✓
metadata schema    —       —       —       —      —        —       ✓
tasks list         ✓       —       —       —      —        —       ✓
tasks submit       ✓       ✓       ✓       ✓      ✗→auto   ✓       ✓
tasks rerun        ✓       ✓       ✗→✓     ✗→✓    -→auto   ✗→✓     ✓
tasks log          ✗→✓     —       —       —      —        —       ✓
tasks rate-limit   ✗→✓     —       —       —      —        —       ✓
search             ✓       —       —       —      —        —       ✓
list               ✓       —       —       —      —        —       ✓
collection create  ✓(→3)   —       —       —      —        ✓       ✓
ai                 ✓       ✗→✓     ✓       ✗→✓    auto     ✓       ✓
config             ✗→✓     —       —       —      —        —       ✓
status             ✗→✓     —       own→✓   —      —        —       ✓
update             —       —       —       —      —        —       ✗→✓

Legend:
  ✓      = currently supported
  ✓(3)   = supported with 3 quiet levels
  ✓(2→3) = currently 2 levels, standardize to 3
  ✗→✓    = currently missing, add
  -→auto = change from `-` marker to auto-detect
  own→✓  = change from own flag to global
  —      = not applicable
```

### Output Modes

| Command | Default | --json | --quiet=1 | --quiet=2 | --dashboard |
|---------|---------|--------|-----------|-----------|-------------|
| download | progress bars | JSONL/file | summary | silent | ✓ (TUI) |
| upload bare | progress bars | JSONL/file | summary | silent | ✓ (TUI) |
| upload import | progress bars | JSONL/file | summary | silent | ✓ (TUI) |
| upload template | CSV/TSV | JSONL | quiet | silent | — |
| upload cleanup | text | JSON | quiet | silent | — |
| metadata modify | per-item text | JSONL/item | summary | silent | — |
| metadata export | JSONL | (same) | summary | silent | — |
| metadata import | per-item text | JSONL/item | summary | silent | — |
| metadata schema | table | JSON | — | — | — |
| tasks list | table + summary | JSONL | no summary | silent | — |
| tasks submit | per-item text | JSONL | summary | silent | — |
| tasks rerun | per-item text | JSONL | summary | silent | — |
| tasks log | colorized text | JSON | raw text | silent | — |
| tasks rate-limit | key-value | JSON | — | silent | — |
| search | table | JSONL | no headers | silent | — |
| list | table | JSONL | summary | silent | — |
| collection create | text | JSON | quiet | silent | — |
| ai | TUI/text | JSONL | summary | silent | — |

---

## 8. Appendix: Audit Methodology

This audit was conducted by an 8-agent team running in parallel tmux panes:

1. **Metadata Analyst** — mapped all 8 metadata subcommands (export, modify,
   append, append-list, insert, remove, import, schema) with line citations
2. **Upload Analyst** — mapped all 4 upload subcommands (bare, import, template,
   cleanup) with flag availability matrix
3. **Tasks Analyst** — mapped all 5 tasks subcommands (list, submit, log, rerun,
   rate-limit) with batch pattern analysis
4. **Download Analyst** — mapped download command's full interface including
   dual-concurrency model and 4 batch input methods
5. **Search & List Analyst** — mapped 3 search backends (scrape, advanced, FTS)
   and list command, identified pagination inconsistencies
6. **Minor Commands Analyst** — audited collection, config, status, update, ai
   for global option compliance
7. **Global Plumbing Analyst** — built command x global-option support matrix,
   traced how each option threads from main.rs through every command
8. **UX Devil's Advocate** — challenged every proposal; prevented "consistency
   for consistency's sake" changes that wouldn't help real users

Each analyst read the full source code of their assigned modules and provided
line-number citations. Cross-cutting issues were communicated between analysts
via direct messages.

### Key Corrections from Devil's Advocate

The devil's advocate correctly pushed back on several proposals:

- **"Make --json global"** — Rejected. Per-command is correct because each
  command produces different JSON. `-j` is already taken by `--jobs`.
- **"Add --jobs to search/list"** — Rejected. Nothing to parallelize.
- **"Progress bars on metadata"** — Rejected. Operations are too fast.
- **"Standardize download's --items"** — Rejected. Two-level concurrency is
  domain-appropriate.
- **"exit(1) for partial failure is wrong"** — Rejected. This is correct Unix
  convention.

The advocate also confirmed the legitimate issues: metadata missing
--retry-failed, AI's silent --jobs ignore, config's process::exit pattern,
and the need for documenting the batch interface design.
