# Metadata Shorthand Refinements Design

**Date:** 2026-04-01
**PR:** #297 (`feat/metadata-shorthand`)
**Status:** Design

## Context

PR #297 adds `-m`/`--metadata` to the top-level `ia metadata` command as shorthand
for `ia metadata modify`, plus `--search`/`--itemlist` for batch reads and writes.
This spec covers six refinements identified during code review.

## 1. Help Text Cleanup

### Problem
The PR's `after_long_help` duplicates modify documentation that already exists in
`ia metadata modify --help`. Advanced write-only options (`--target`, `--expect`,
`--priority`, `--reduced-priority`) also appear in the top-level options list,
cluttering it.

### Design
- **Keep modify examples** in `after_long_help` for discoverability — users scanning
  `--help` should immediately see that `-m` works at the top level.
- **Add a pointer** after the modify examples:
  `# See 'ia metadata modify --help' for write options`
- **Hide advanced write options** from the top-level help with `#[arg(hide = true)]`
  on `--target`, `--expect`, `--priority`, `--reduced-priority` in `MetadataArgs`.
- **Keep visible:** `-m` and `--dry-run` — these are the core shorthand flags.

### Resulting help structure
```
Examples:
  # Read metadata
  $ ia metadata nasa
  $ ia metadata nasa --exists

  # Batch read
  $ ia metadata --search "collection:nasa"
  $ ia metadata --itemlist ids.txt
  $ echo id1 | ia metadata

  # Modify metadata (shorthand for 'ia metadata modify')
  $ ia metadata nasa -m "title:New Title"
  $ ia metadata nasa -m "subject:rockets" --dry-run

  # Batch modify
  $ ia metadata --search "collection:test" -m "subject:updated"
  $ ia metadata --itemlist ids.txt -m "subject:new-tag"

  # Compound operations (single request)
  $ ia metadata nasa -m "title:New" + remove -m "subject:old"

  # See 'ia metadata modify --help' for write options (--target, --expect, etc.)

  # Export to file
  $ ia metadata export --search "collection:nasa" -o data.xlsx

  # Batch import from spreadsheet
  $ ia metadata --spreadsheet data.csv --dry-run

  # Browse metadata field definitions
  $ ia metadata schema
```

## 2. `--target` as `Option<String>`

### Problem
`MetadataArgs.target` uses `default_value = "metadata"`, then runtime validation
checks `args.target != "metadata"` to detect explicit usage. This coupling is
fragile — if the default ever changes, the guard silently breaks. There's also no
way to distinguish "user didn't pass --target" from "user passed --target metadata".

### Design
- Change `MetadataArgs.target` from `String` with `default_value` to `Option<String>`.
- **Read mode** (no `-m`): if `target.is_some()` → bail with
  `"--target requires -m or a write subcommand"`.
- **Write mode** (has `-m`): pass `target.unwrap_or_else(|| "metadata".into())`
  into `WriteOpts`.
- `WriteOpts.target` (shared by all write subcommands) stays as `String` with
  `default_value = "metadata"` — no change to subcommand behavior.

## 3. Batch `--exists` and `--formats`

### Problem
The PR explicitly rejects `--exists` and `--formats` with `--search`/`--itemlist`/stdin.
Batch exists-checking is a natural use case (e.g., "which items in this list actually
exist?").

### Design
Lift the restriction. Support `--exists` and `--formats` in batch mode.

**`--exists` batch behavior (matches single-item semantics):**
- Without `--json`: silent output, exit 0 if all exist, exit 1 if any don't.
- With `--json`: one JSONL line per identifier —
  `{"identifier": "x", "exists": true}` or `{"identifier": "x", "exists": false}`.
  Exit 1 if any don't exist.

**`--formats` batch behavior:**
- JSONL output, one JSON object per identifier with an `identifier` field added.
- Uses the same concurrent semaphore pattern as `run_read_multi`.

**Implementation:** Add a `run_exists_multi` and `run_formats_multi` function (or
extend `run_read_multi` with mode parameters) called from the batch read path when
the respective flag is set. Use the existing `client.item_exists()` and
`client.get_item()` APIs.

## 4. Stdin Empty-Line Filtering

### Problem
If stdin contains only blank lines (e.g., `echo "" | ia md`), the batch path
activates and fails with "no identifiers found" instead of showing the usage error.
This is confusing.

### Design
- Filter blank lines (empty or whitespace-only) from stdin input before processing.
  Apply the same filter to `--itemlist` file reading for consistency.
- When no identifiers remain after filtering:
  - Print `warning: no identifiers found from stdin` (or `from --itemlist`) to stderr.
  - Print nothing to stdout (empty stream for `--json`/pipe consumers).
  - Exit 0 — empty input is not an error, just nothing to do.

**Where to filter:** In `collect_identifiers_from_batch` (or its stdin/itemlist
reading helpers), trim and skip empty lines before adding to the identifiers list.

## 5. Leave `bail!()` Checks As-Is

The six sequential runtime validation checks for write-only options in read mode
stay as explicit individual `if` + `bail!()` statements. No abstraction — the set
of write options is stable and each check is trivially readable.

## 6. Integration Tests

### Problem
The PR has 35 unit tests for clap parsing but no CLI integration tests for the
runtime validation error paths.

### Design
Add integration tests using the existing `assert_cmd` + `predicates` pattern.

**Runtime validation error tests (no HTTP, in `cli.rs`):**
- `ia metadata nasa --dry-run` → stderr contains "requires -m or a write subcommand"
- `ia metadata nasa --target files/foo` → same
- `ia metadata nasa --expect x:y` → same
- `ia metadata nasa --priority 5` → same
- `ia metadata nasa --reduced-priority` → same

**Shorthand acceptance test:**
- `ia metadata nasa -m "title:X"` → does NOT produce a clap error (will fail on
  network, but should not fail on argument parsing)

**Batch exists/formats tests (with wiremock):**
- `ia metadata --itemlist <file> --exists` with mock responses → correct JSONL + exit code
- `ia metadata --itemlist <file> --formats` with mock responses → correct JSONL output

**Stdin filtering test:**
- Pipe empty/whitespace-only lines → stderr contains "no identifiers found", exit 0
