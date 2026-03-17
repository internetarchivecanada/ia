# Batch Interface Review Fixes

**Date:** 2026-03-17
**PR:** #269 (worktree-batch-interface)
**Context:** Post-review fixes identified by 9-agent review team before human review.

## Problem

PR #269 redesigned the batch interface across the CLI. An automated
review team of 7 domain reviewers + 2 adversarial filters identified
three categories of issues:

1. **Tasks submit positional ambiguity** — The `cmd` argument is a
   positional, requiring heuristic normalization (`looks_like_task_cmd`)
   that breaks for custom task commands with `--itemlist`/`--search`.
2. **Inconsistent JSONL parsing** — `ia search --json | ia metadata modify`
   silently treats raw JSON strings as identifiers because metadata's
   stdin reader lacks JSONL parsing. Tasks `--itemlist` and AI `--itemlist`
   also lack it, while download handles it everywhere.
3. **Identifier logic scattered across 3 files in 2 crates** —
   `validate_identifier` in `upload/validate.rs`, `sanitize_identifier`
   and `generate_identifier` in `upload/template.rs`,
   `parse_identifier_line` in `ia-cli/download.rs`. These are all
   identifier operations that should live together.

## Solution

### 1. New module: `ia-core/src/identifier.rs`

Consolidate all identifier operations into a single discoverable module.

**Functions to move:**

| Function | From | Notes |
|----------|------|-------|
| `validate_identifier(id: &str) -> Result<(), IaError>` | `upload/validate.rs` | Validates IA identifier rules (3-100 chars, valid charset, starts with alnum/@) |
| `sanitize_identifier(s: &str) -> String` | `upload/template.rs` (private) | Transform counterpart of validate — makes strings into valid identifiers. Make `pub`. |
| `generate_identifier(path, prefix, from_filename, from_dirname) -> String` | `upload/template.rs` (private) | Derives identifier from file path. Make `pub`. Change signature from `(&Path, &TemplateOpts)` to `(&Path, Option<&str>, bool, bool)` — no struct needed for 3 params. |
| `parse_identifier_line(line: &str) -> Option<String>` | `ia-cli/download.rs` (private) | Extracts identifier from plain text or JSONL. No CLI dependency — pure string parsing. |

**What stays put:**

- `validate_required_metadata()`, `check_collections()`, `validate_file()`
  stay in `upload/validate.rs` — genuinely upload-specific.
- `TemplateOpts`, `TemplateRow`, `generate_template()`, CSV/XLSX writers
  stay in `upload/template.rs` — template generation logic.

**Re-export:** Add `pub mod identifier;` to `ia-core/src/lib.rs`.

**Tests:** `validate_identifier` and `sanitize_identifier` tests move
with their functions. `generate_identifier` has no direct unit tests
(only exercised indirectly via `generate_template`) — new direct unit
tests must be written. `parse_identifier_line` tests move from
`download.rs`. See Testing section for full details.

### 2. Tasks submit: `--cmd` flag replaces positional

Replace the `cmd` positional argument on `SubmitArgs` with a `--cmd`
flag.

**Before (broken):**
```
ia tasks submit my-item derive           # two positionals, ambiguous
ia tasks submit derive --itemlist ids.txt # breaks for custom commands
```

**After (clean):**
```
ia tasks submit my-item --cmd derive
ia tasks submit --cmd derive --itemlist items.txt
ia tasks submit --cmd derive              # stdin auto-detect
ia tasks submit --spreadsheet jobs.csv    # cmd from spreadsheet rows
```

**What this eliminates:**
- `normalize_submit_args()` — entire function deleted
- `looks_like_task_cmd()` — entire function deleted
- The deprecation detection/swap logic
- The buggy batch-source normalization heuristic (lines 583-589)

**Clap declaration:**
```rust
/// Task command (e.g. derive, make_dark)
#[arg(long, required_unless_present = "spreadsheet")]
pub cmd: Option<String>,
```

**No deprecation bridge needed** — the positional `cmd` form was
introduced in this same unreleased PR. No one to deprecate for.

This also fixes the bug where custom task commands (e.g. `reduce_item`)
failed with `--itemlist`/`--search` because `looks_like_task_cmd` only
recognized 8 hardcoded command names + `.php` suffix. With `--cmd` as
an explicit flag, any command string works.

### 3. Consistent JSONL parsing everywhere

Use `parse_identifier_line` (from `ia_core::identifier`) in every
`collect_identifiers` function, for both `--itemlist` file reading and
stdin reading.

**Files to update:**

| File | Function | Current behavior | Fix |
|------|----------|-----------------|-----|
| `metadata.rs` | `collect_identifiers_from_batch` | Plain text only in both `--itemlist` file loop (lines 1613-1621) and stdin loop (lines 1634-1648) | Use `parse_identifier_line` in both loops |
| `metadata.rs` | `collect_identifiers_from_export` | `--itemlist` uses `read_identifiers_from_file` (unchanged). Stdin (lines 864-873) is plain text only. | Use `parse_identifier_line` in stdin loop only. Leave `read_identifiers_from_file` as-is. |
| `tasks.rs` | `collect_submit_identifiers` | JSONL in stdin only (inline), plain text for `--itemlist` file loop (lines 1186-1194) | Use `parse_identifier_line` in both `--itemlist` file loop and stdin loop |
| `ai.rs` | `collect_identifiers` | JSONL in stdin only (inline), plain text for `--itemlist` file loop (lines 387-395) | Use `parse_identifier_line` in both `--itemlist` file loop and stdin loop |
| `download.rs` | `collect_identifiers` | Already correct (uses `parse_identifier_line` everywhere) | Update import path only |

After this change, `ia search --json | ia <any-command>` works
consistently across download, metadata, tasks, and ai.

### 4. Caller migration

Update all imports from old locations to `ia_core::identifier::*`:

| Caller | Old import | New import |
|--------|-----------|------------|
| `upload/item.rs` | `crate::upload::validate::validate_identifier` | `crate::identifier::validate_identifier` |
| `upload/batch.rs` | `crate::upload::validate::validate_identifier` | `crate::identifier::validate_identifier` |
| `upload/template.rs` | local `sanitize_identifier`, `generate_identifier` | `crate::identifier::{sanitize_identifier, generate_identifier}`. `generate_template` call site must adapt from `generate_identifier(&path, opts)` to `generate_identifier(&path, opts.identifier_prefix.as_deref(), opts.identifier_from_filename, opts.identifier_from_dirname)`. |
| `collection.rs` | `crate::upload::validate::validate_identifier` | `crate::identifier::validate_identifier` |
| `ia-cli/download.rs` | local `parse_identifier_line` | `ia_core::identifier::parse_identifier_line` |
| `ia-cli/tasks.rs` | inline JSONL parsing | `ia_core::identifier::parse_identifier_line` |
| `ia-cli/ai.rs` | inline JSONL parsing | `ia_core::identifier::parse_identifier_line` |
| `ia-cli/metadata.rs` | plain text only | `ia_core::identifier::parse_identifier_line` |

## Testing

### Moved tests (keep existing coverage)

- `validate_identifier`: 6 tests from `upload/validate.rs` (valid_identifiers, invalid_identifier_too_short, invalid_identifier_too_long, invalid_identifier_bad_chars, invalid_identifier_bad_start, valid_identifier_at_max_length)
- `sanitize_identifier`: 7 tests from `upload/template.rs` (sanitize_simple, sanitize_strips_leading_non_alnum, sanitize_too_short, sanitize_truncates_long, sanitize_preserves_dots_and_underscores, sanitize_empty_input, sanitize_all_special_chars)
- `parse_identifier_line`: 6 tests from `download.rs` (parse_plain_identifier, parse_plain_identifier_with_whitespace, parse_jsonl_identifier, parse_jsonl_with_extra_fields, parse_empty_and_comment_lines, parse_json_without_identifier_field)

### New tests in `ia-core/src/identifier.rs`

- `generate_identifier` direct unit tests (currently only tested
  indirectly through `generate_template` in template.rs — those tests
  stay there, but we need direct coverage):
  - from filename: stem extracted, sanitized
  - from dirname: parent dir name extracted, sanitized
  - with prefix: prefix prepended with `-`
  - neither from_filename nor from_dirname: returns empty
  - path with no stem/parent: returns empty
  - result too short after sanitize: returns empty
- `sanitize_identifier` / `validate_identifier` coherence: sanitize
  output always passes validate, or is empty
- `parse_identifier_line` with malformed JSON (missing closing brace)

### New/updated CLI integration tests

- `tasks submit --cmd` required without `--spreadsheet` (clap error).
  Note: error message changes from custom "task command is required" to
  clap's generated message.
- `tasks submit --cmd derive --itemlist items.txt` works (regression
  test for the custom-command bug)
- `tasks submit --cmd custom_task --search "query"` works (verifies
  arbitrary command names, not just the hardcoded 8)
- All existing tasks submit tests updated to use `--cmd` flag. Tests
  that need updating (~10): test_tasks_submit_json, test_tasks_submit_wait,
  test_tasks_submit_rate_limited_retry, test_tasks_submit_malformed_args,
  test_tasks_submit_with_args_and_comment, test_tasks_submit_batch_itemlist,
  test_tasks_retry_failed_requires_joblog, test_tasks_submit_cmd_required_without_spreadsheet,
  test_tasks_submit_spreadsheet_nonexistent.
- Delete `test_tasks_submit_deprecated_old_arg_order` entirely — no
  deprecation bridge exists for the positional form.
- Help text tests updated for `--cmd`.
- Help text examples in tasks.rs `after_long_help` and parent `TasksArgs`
  help updated to show `--cmd` syntax.

### Verify no regressions

- `just ci` (fmt-check + check + test + doc) must pass
- All 1,126+ existing tests must pass
- `cargo doc --no-deps` with `-D warnings` must pass

## File Map

| File | Change |
|------|--------|
| `ia-core/src/identifier.rs` | **NEW** — validate, sanitize, generate, parse_identifier_line + all tests |
| `ia-core/src/lib.rs` | Add `pub mod identifier;` |
| `ia-core/src/upload/validate.rs` | Remove `validate_identifier` + its tests |
| `ia-core/src/upload/template.rs` | Remove `sanitize_identifier`, `generate_identifier` + their tests; import from `identifier` |
| `ia-cli/src/commands/tasks.rs` | `--cmd` flag, delete `normalize_submit_args`/`looks_like_task_cmd`, use `parse_identifier_line` |
| `ia-cli/src/commands/download.rs` | Remove local `parse_identifier_line` + tests, import from `ia_core::identifier` |
| `ia-cli/src/commands/metadata.rs` | Use `parse_identifier_line` in both collect functions |
| `ia-cli/src/commands/ai.rs` | Use `parse_identifier_line` in collect function |
| `ia-core/src/upload/item.rs` | Update import |
| `ia-core/src/upload/batch.rs` | Update import |
| `ia-core/src/collection.rs` | Update import |
| `ia-cli/tests/tasks.rs` | Update all submit tests to `--cmd`, add new tests |
| `ia-cli/tests/cli.rs` | Update any tasks-related tests |
| `docs/usage.md` | Update tasks submit examples to `--cmd` |
