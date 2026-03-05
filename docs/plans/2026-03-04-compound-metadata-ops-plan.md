# Compound Metadata Operations — Implementation Plan

**Date:** 2026-03-04
**Design doc:** `docs/plans/2026-03-04-compound-metadata-ops-design.md`
**GitHub issues:** #192-#196, #198, #199 (parent)

## Architecture

Chain multiple metadata operation types (modify, append, insert, remove, etc.) in a single `ia metadata` invocation using `+` as a separator. All operations combine into one JSON Patch, submitted as one HTTP POST — one request, one catalog task.

```
ia metadata modify my-item -m title:New + remove -m subject:old + insert -m collection[0]:featured
```

### Layered implementation

1. **ia-core** — `ChangeGroup` struct, `compute_compound_patch()`, `CompoundModifyRequest`, `modify_compound()`
2. **ia-cli argv** — Pre-clap argv interception (`split_compound_args`, `extract_compound_from_argv`)
3. **ia-cli dispatch** — `WriteContext` struct, `build_continuation_groups()`, unified batch loop

## ia-core changes

### Types (`ia-core/src/metadata/write.rs`)

- `ChangeGroup` — Named struct with `changes: Vec<(String, Value)>` and `op: MetadataOp`. Replaces opaque tuple alias.
- `CompoundModifyRequest` — Like `ModifyRequest` but with `groups: Vec<ChangeGroup>` instead of single `changes`/`op`.

### Functions

- `compute_compound_patch(source, groups, expect, identifier)` — Chains `prepare_metadata` calls sequentially (source → group1 → intermediate → group2 → ... → final), diffs source vs final once. Prepends `test` ops from `expect`.
- `modify_compound(client, req)` — Validates auth, fetches metadata once (GET), calls `compute_compound_patch`, POSTs single combined patch.
- `compute_patch()` — Refactored as convenience wrapper around `compute_compound_patch` (single-element group).
- `modify()` — Refactored as convenience wrapper around `modify_compound`. Full backward compatibility for `ia-core` consumers.

### Re-exports (`ia-core/src/metadata/mod.rs`)

Added: `compute_compound_patch`, `modify_compound`, `ChangeGroup`, `CompoundModifyRequest`.

## ia-cli argv interception

### Problem

Clap chokes on bare `+` tokens in argv. Compound operations must be extracted before clap parsing.

### Solution (`ia-cli/src/commands/metadata.rs`)

Pre-scan `std::env::args()` in `main()` before `Cli::parse()`:

1. `find_metadata_subcommand_pos(args, value_taking_flags)` — Walks argv skipping the binary name, global flags, and flag values to find the first positional arg. Returns `Some(pos)` only if that positional is `"metadata"`. Prevents false positives like `ia download metadata`.
2. `split_compound_args(args_after_metadata)` — Splits on bare `+` tokens. Validates: no empty segments, no trailing `+`, each continuation starts with a valid op name, no shared options in continuations. Extracts `-m`/`--metadata` values.
3. `extract_compound_from_argv(raw_args, value_taking_flags)` — Orchestrates 1 + 2. Returns `CompoundFromArgv { filtered_argv, continuations }`.

The `value_taking_flags` parameter is derived dynamically from `Cli::command()` via `value_taking_global_flags()` in `main.rs` — no hardcoded list to maintain.

### Single-pass continuation parsing

The shared-options check and `-m` extraction are interleaved in one loop. This ensures a `-m` value that matches a shared option name (e.g., `-m field:--dry-run`) is consumed as a value, not rejected as a misplaced flag.

### Insert handling

`parse_continuation_groups(op_name, raw_changes)` returns `Vec<ChangeGroup>`. For insert ops, each `-m` arg gets its own `ChangeGroup` with its own index (matching `run_write_insert` behavior). For other ops, all changes are collected into a single group.

## ia-cli dispatch

### WriteContext

Groups `quiet`, `jobs`, `joblog_path` into a struct, reducing parameter count across `run_write`, `run_write_insert`, `run_write_inner`, `run_import` (was 7-8 params, now 4-5).

### build_continuation_groups

Shared helper that parses raw continuation segments into `Vec<ChangeGroup>`, used by both `run_write` and `run_write_insert`. Eliminates duplicated continuation processing.

### Batch loop simplification

The old batch loop used a labeled `'groups` loop with inner retry per group, accumulating `batch_error` and `last_task_id`. With `modify_compound`, each item becomes a single `modify_compound` call — one retry loop, one outcome.

### Import

`run_import` also uses `modify_compound` for consistency — single POST per spreadsheet row.

### Incompatible subcommands

`export` and `import` bail with clear errors if `continuations.is_some()`. Bare read mode (no subcommand) also rejects continuations.

## Shared options

Write-subcommand options that apply to the entire compound operation must appear in the first segment only:
`--target`, `--expect`, `--priority`, `--reduced-priority`, `--dry-run`, `--json`, `--pretty`, `--itemlist`, `--search`.

## Test coverage

### ia-core unit tests (write.rs)
- `compute_compound_patch`: single group matches `compute_patch`, set+remove, set+append-list, same-field-last-wins, set+remove-same-field-net-remove, empty groups, expect prepends test ops, error propagation, three groups
- JSON output shape validation for dry-run rendering

### ia-core integration tests (metadata_write.rs)
- `modify_compound`: set+remove single POST (expect counts on GET/POST), no-net-changes error, auth required, backward compat via `modify()`, three-group single POST, overlapping-fields last-wins
- JSON serialization of compound patch ops

### ia-cli unit tests (metadata.rs compound_tests)
- `split_compound_args`: no plus, single/multiple continuations, trailing +, empty continuation, shared option in continuation, shared option as -m value (allowed), invalid op, long metadata flag
- `parse_continuation_groups`: modify, insert with index, remove, no changes error, append-list, insert multi-index produces separate groups, insert single, insert no-index defaults zero, non-insert single group
- `find_metadata_subcommand_pos`: bare, with flags, download returns none, --host skips value, -c skips value
- `extract_compound_from_argv`: no metadata, no plus, with plus, download false positive, --host false positive, flags before metadata, list false positive

### ia-cli integration tests (cli.rs)
- Trailing + error, unknown op error, shared option in continuation error, empty continuation error, import+compound error, help text mentions compound ops (metadata --help, modify --help)
