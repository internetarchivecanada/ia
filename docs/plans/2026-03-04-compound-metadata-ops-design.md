# Compound Metadata Operations Design

**Date:** 2026-03-04
**Status:** Draft

## Problem

Currently, each `ia metadata` invocation supports only one operation type (modify, append, insert, remove, etc.). To apply multiple operation types to the same item, users must run separate commands — each producing a separate HTTP request and a separate archive.org catalog task. This causes three problems:

1. **Multiple catalog tasks** — Each request creates its own task, so operations may execute out of order or with race conditions between them.
2. **Performance** — N operations = N round trips (each includes a GET to fetch current metadata + a POST to apply the patch).
3. **No atomicity** — If the second command fails, the first has already been applied. There is no way to submit a set of changes as a single unit.

## Solution

Allow chaining multiple operation types in a single `ia metadata` invocation using `+` as a separator. All operations are combined into a single JSON Patch array and submitted as one HTTP POST — one request, one catalog task.

```
ia metadata modify my-item -m title:"New Title" -m date:2024 \
  + insert -m "collection[0]:featured" \
  + remove -m subject:old-tag
```

## Syntax

### The `+` Separator

A bare `+` token in argv (a standalone argument that is exactly `"+"`) separates operation segments. It is not a shell special character, requires no quoting, and reads naturally as "and also do this."

A `+` inside a quoted value (e.g., `-m 'title:My title + with a + in it'`) is part of the string and is not treated as a separator. The shell delivers it as a single argv element, not as a standalone `+` token.

### Segment Structure

**Segment 1 (primary):** Full subcommand with identifier and all shared options.

```
modify my-item -m title:"New Title" --dry-run --priority 0
```

**Segments 2+ (continuations):** Operation type + `-m` flags only. The identifier and all shared options are inherited from segment 1.

```
+ insert -m "collection[0]:featured"
+ remove -m subject:old-tag
```

### Shared Options

These options apply to all segments and must appear in the first segment only. If they appear in a continuation segment, the command errors:

- `--target` — metadata target (item-level or file-level)
- `--expect` — optimistic concurrency checks
- `--priority` — catalog task priority
- `--reduced-priority` — accept reduced priority
- `--dry-run` — show changes without writing
- `--json` — structured output
- `--pretty` — pretty-print JSON
- `--itemlist` / `--search` — bulk input sources

If a shared option appears in a continuation:

```
error: --dry-run must appear in the first operation segment (before any +)
```

### Valid Operations in Continuations

Each continuation must start with one of: `modify`, `append`, `append-list`, `insert`, `remove`. It accepts only `-m` (repeatable) for specifying key:value pairs.

## Argv Splitting

Before clap parses the metadata subcommand's args, we pre-process `std::env::args()` and split on bare `+` tokens:

1. Collect all args after the `metadata` subcommand.
2. Split into segments on bare `+` tokens.
3. Parse segment 1 as a full `MetadataArgs` (via clap or manual parsing).
4. Parse each continuation segment as a minimal struct: operation name + `-m` values.
5. Validate that no shared options appear in continuations.

The splitting happens before clap sees the args for the metadata command. Single-operation invocations (no `+`) pass through unchanged — full backward compatibility.

## Patch Computation

### Current Flow (single op)

1. Fetch metadata: GET `/metadata/{id}`
2. `prepare_metadata(source, changes, op)` → destination
3. `json_patch::diff(source, destination)` → patch ops
4. POST patch

### New Flow (compound ops)

1. Fetch metadata once: GET `/metadata/{id}`
2. Chain operations sequentially:
   ```
   intermediate_0 = source (from GET)
   intermediate_1 = prepare_metadata(intermediate_0, changes_1, op_1)
   intermediate_2 = prepare_metadata(intermediate_1, changes_2, op_2)
   ...
   final = prepare_metadata(intermediate_N-1, changes_N, op_N)
   ```
3. Diff once: `json_patch::diff(source, final)` → single combined patch
4. POST once — one request, one catalog task

### ia-core API Change

The `modify()` function in `ia-core/src/metadata/write.rs` needs to accept multiple operation groups:

```rust
pub struct CompoundModifyRequest {
    pub identifier: String,
    pub operations: Vec<(Vec<(String, serde_json::Value)>, MetadataOp)>,
    pub target: String,
    pub expect: Option<HashMap<String, serde_json::Value>>,
    pub priority: Option<i32>,
    pub reduced_priority: bool,
}
```

A new `modify_compound()` function (or extending `modify()`) chains `prepare_metadata` calls and produces a single patch. The existing `ModifyRequest` + `modify()` remain as-is for backward compatibility; single-op is a special case of compound (one-element operations list).

## Operation Ordering

Operations are applied in the order they appear on the command line. This matches JSON Patch semantics (ops are sequential). If multiple segments touch the same field, the last one wins.

Example: `modify -m title:A + remove -m title:A` → title is removed (remove runs after modify).

## Error Handling

- **Invalid syntax in any segment** → error before any HTTP request is made.
- **Shared option in continuation** → clear error message naming the option and the rule.
- **Combined patch produces no changes** → existing "no changes computed" error.
- **Immutable/admin-only field warnings** → run validation across all segments before the request.
- **`+` with no operation after it** → error: "expected operation after +"
- **Empty continuation** → error: "expected operation name (modify, append, insert, remove, append-list)"

## Batch Compatibility

Compound ops work with all bulk input sources:

- `--itemlist` — each item gets the same compound operation
- `--search` — each search result gets the same compound operation
- stdin — each identifier gets the same compound operation

Same concurrency model: JoinSet + Semaphore + RateLimiter.

### Spreadsheet Incompatibility

`--spreadsheet` is **not compatible** with `+`. Spreadsheet mode has its own column-prefix system for mixed operations (e.g., `append:description`, `remove:subject`). If both are used:

```
error: --spreadsheet cannot be combined with + compound operations
```

## JSON Output

### Normal Mode (`--json`)

Same shape as today — one result per item with a single task_id from the single POST:

```json
{"item": "my-item", "status": "ok", "task_id": 12345, "elapsed_ms": 450}
```

### Dry Run (`--dry-run --json`)

The changes array includes all patch ops from all segments combined:

```json
{
  "item": "my-item",
  "status": "would_modify",
  "dry_run": true,
  "changes": [
    {"op": "replace", "path": "/title", "value": "New Title"},
    {"op": "add", "path": "/collection/0", "value": "featured"},
    {"op": "remove", "path": "/subject/2"}
  ]
}
```

## Help Text & Discoverability

### `ia metadata --help`

Add a "Compound Operations" section in `after_long_help`:

```
Compound Operations:
  Chain multiple operations with + to submit as a single request:

  $ ia metadata modify my-item -m title:"New" + append -m "description:more text"
  $ ia metadata modify my-item -m date:2024 + insert -m "collection[0]:featured" + remove -m subject:old

  Options like --dry-run, --target, --priority apply to all operations
  and must appear in the first segment (before any +).
```

### Per-Operation Help

Each operation's help text (`ia metadata modify --help`, etc.) gets a brief note:

```
Tip: Chain with other operations using +
  $ ia metadata modify my-item -m title:"New" + remove -m subject:old
```

## Testing Strategy

### Unit Tests (ia-core)

- **`prepare_metadata` chaining:** Apply multiple ops sequentially to the same metadata. Test every meaningful combination:
  - Set + Remove (modify field, then remove another)
  - Set + Append (modify field, then append to another)
  - Set + Insert (modify field, then insert into list)
  - Insert + Remove (insert into list, then remove from another)
  - Remove + Set (remove field, then set it again)
  - Same field across segments (modify title + remove title → removed)
  - Three or more segments chained
- **`compute_patch` with multiple op groups:** Verify single combined patch array is correct.
- **Edge cases:**
  - All segments produce no-ops → "no changes" error
  - One segment is a no-op, others have changes → only real changes in patch
  - Operations on array fields with index manipulation across segments

### Integration Tests (ia-cli)

- **Argv splitting:**
  - Bare `+` splits correctly
  - `+` inside quoted values is not treated as separator
  - `+` at end of args → error
  - Multiple `+` in a row → error
  - No `+` → backward-compatible single-op behavior
  - Shared options in continuation → clear error
- **End-to-end with wiremock:**
  - Compound modify + remove → single POST with combined patch
  - Compound with --dry-run → correct output, no POST
  - Compound with --json → correct JSON shape
  - Compound with --itemlist → each item gets compound ops
  - Compound with --spreadsheet → error
- **Backward compatibility:**
  - All existing single-op commands work identically
  - No behavior change when `+` is not present

## Backward Compatibility

This is purely additive. The `+` token has no current meaning in the metadata command. All existing invocations work identically. The `+` feature is opt-in — users who don't need it never see it.

## Future Considerations

- The spreadsheet import column-prefix system (`append:`, `remove:`, etc.) already supports mixed ops per-row. Compound `+` syntax is the CLI equivalent for ad-hoc use.
- If other commands need compound operations in the future, the argv-splitting infrastructure can be reused.
