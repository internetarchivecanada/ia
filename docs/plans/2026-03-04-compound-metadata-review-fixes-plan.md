# Compound Metadata Operations — Review Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all issues identified in the PR #200 code review: insert multi-index bug, argv false-positive detection, ChangeGroup type safety, parameter proliferation, dead code, and missing tests.

**Architecture:** Six tasks, ordered by dependency. Task 1 converts `ChangeGroup` from tuple alias to struct (foundational change touching both crates). Task 2 fixes the insert multi-index bug. Task 3 fixes false-positive argv detection. Task 4 consolidates parameters and eliminates duplication. Task 5 moves imports to top-of-file. Task 6 removes dead `--spreadsheet` reference and adds missing test.

**Tech Stack:** Rust, clap 4, serde_json, wiremock (tests), assert_cmd (CLI tests)

**Branch:** `feat/compound-metadata-ops` (existing worktree at `.claude/worktrees/feat/compound-metadata-ops/`)

---

### Task 1: Convert `ChangeGroup` from tuple alias to struct

**Why:** The tuple alias `(Vec<(String, serde_json::Value)>, MetadataOp)` produces opaque `.0` / `.1` access throughout the codebase. A named struct with `changes` and `op` fields is self-documenting and prevents positional errors.

**Files:**
- Modify: `ia-core/src/metadata/write.rs` (definition + all usage)
- Modify: `ia-core/src/metadata/mod.rs` (re-export — unchanged, just verify)
- Modify: `ia-core/tests/metadata_write.rs` (test construction sites)
- Modify: `ia-cli/src/commands/metadata.rs` (all usage sites)

**Step 1: Change the type definition in ia-core**

In `ia-core/src/metadata/write.rs`, replace the type alias (line 27) with a struct:

```rust
// REPLACE:
// pub type ChangeGroup = (Vec<(String, serde_json::Value)>, MetadataOp);

// WITH:
/// A single operation group: changes to apply and which operation to use.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeGroup {
    /// Field:value pairs to apply
    pub changes: Vec<(String, serde_json::Value)>,
    /// The operation type (Set, Append, AppendList, Insert, Remove)
    pub op: MetadataOp,
}
```

**Step 2: Update all construction sites in ia-core/src/metadata/write.rs**

Every `(changes, op)` tuple construction becomes `ChangeGroup { changes, op }`.

Key locations:
- `compute_patch` (line 228): `&[(changes.to_vec(), op.clone())]` → `&[ChangeGroup { changes: changes.to_vec(), op: op.clone() }]`
- `modify` wrapper (line 440): `vec![(req.changes.clone(), req.op.clone())]` → `vec![ChangeGroup { changes: req.changes.clone(), op: req.op.clone() }]`

Update all destructuring:
- `compute_compound_patch` loop (line 249): `for (changes, op) in groups` → `for ChangeGroup { changes, op } in groups` (or `for g in groups` with `g.changes`/`g.op`)

**Step 3: Update all construction sites in ia-core/tests/metadata_write.rs**

Every test that constructs ChangeGroup tuples in `groups: vec![...]` (compound modify tests, lines 1736+) needs the struct syntax. Example:

```rust
// REPLACE:
groups: vec![
    (
        vec![("title".to_string(), json!("New Title"))],
        MetadataOp::Set,
    ),
    (
        vec![("subject".to_string(), json!("science"))],
        MetadataOp::Remove,
    ),
],

// WITH:
groups: vec![
    ChangeGroup {
        changes: vec![("title".to_string(), json!("New Title"))],
        op: MetadataOp::Set,
    },
    ChangeGroup {
        changes: vec![("subject".to_string(), json!("science"))],
        op: MetadataOp::Remove,
    },
],
```

Also update unit tests for `compute_compound_patch` (lines 1242+).

**Step 4: Update all sites in ia-cli/src/commands/metadata.rs**

Key locations:
- `run_write` (line 549): `vec![(changes, op)]` → `vec![ChangeGroup { changes, op }]`
- `run_write_insert` (line 580): `.map(|s| { ... Ok((vec![(field, json!(value))], MetadataOp::Insert(index))) })` → use `ChangeGroup { changes: vec![...], op: MetadataOp::Insert(index) }`
- `run_write_inner` signature (line 602): `change_groups: Vec<(Vec<(String, serde_json::Value)>, MetadataOp)>` → `change_groups: Vec<ChangeGroup>`
- `run_dry_run_compound` signature (line 1013): `groups: &[(Vec<(String, serde_json::Value)>, MetadataOp)]` → `groups: &[ChangeGroup]`
- `run_write_inner` field warnings loop (line 610): `for (changes, _) in &change_groups` → `for group in &change_groups` with `group.changes`
- `run_import` local type alias (line 870): remove the local redeclaration, use the imported `ChangeGroup`
- `run_import` group construction (lines 882-893): `last.1 == op` → `last.op == op`, `last.0.push(...)` → `last.changes.push(...)`, `groups.push((vec![...], op))` → `groups.push(ChangeGroup { changes: vec![...], op })`
- `parse_continuation_group` return type (line 1311): already returns `ia_core::metadata::write::ChangeGroup` — update tuple construction to struct construction

Add `ChangeGroup` to the top-of-file imports (line 15-18).

**Step 5: Run tests to verify**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All 571+ tests pass

**Step 6: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 7: Commit**

```
refactor(ia-core): convert ChangeGroup from tuple alias to named struct

Replace `type ChangeGroup = (Vec<(String, Value)>, MetadataOp)` with a
proper struct having `changes` and `op` fields. Eliminates opaque .0/.1
tuple access throughout the codebase, making all usage sites
self-documenting.

Mechanical change — no behavior change.
```

---

### Task 2: Fix insert multi-index bug in `parse_continuation_group`

**Why:** When a continuation has multiple `-m` args with different indices (e.g., `+ insert -m collection[0]:featured -m subject[2]:physics`), only the last index is preserved. The primary `run_write_insert` correctly creates one ChangeGroup per `-m` arg; the continuation path must do the same.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (`parse_continuation_group` + callers)
- Modify: `ia-cli/src/commands/metadata.rs` (unit tests in `compound_tests` module)
- Modify: `ia-core/tests/metadata_write.rs` (integration test)

**Step 1: Write failing tests**

Add to the `compound_tests` module in `ia-cli/src/commands/metadata.rs`:

```rust
#[test]
fn parse_continuation_insert_multi_index_produces_separate_groups() {
    // Two -m args with different indices must produce two separate ChangeGroups
    let groups = parse_continuation_groups(
        "insert",
        &["collection[0]:featured".to_string(), "subject[2]:physics".to_string()],
    )
    .unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].op, MetadataOp::Insert(0));
    assert_eq!(groups[0].changes[0].0, "collection");
    assert_eq!(groups[1].op, MetadataOp::Insert(2));
    assert_eq!(groups[1].changes[0].0, "subject");
}

#[test]
fn parse_continuation_insert_single_still_works() {
    let groups = parse_continuation_groups(
        "insert",
        &["collection[0]:featured".to_string()],
    )
    .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].op, MetadataOp::Insert(0));
}

#[test]
fn parse_continuation_insert_no_index_defaults_zero() {
    let groups = parse_continuation_groups(
        "insert",
        &["collection:featured".to_string()],
    )
    .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].op, MetadataOp::Insert(0));
}

#[test]
fn parse_continuation_non_insert_returns_single_group() {
    // Non-insert ops still return a single group with all changes
    let groups = parse_continuation_groups(
        "modify",
        &["title:New".to_string(), "date:2024".to_string()],
    )
    .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].changes.len(), 2);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli parse_continuation_insert_multi_index -- --nocapture`
Expected: FAIL — `parse_continuation_groups` doesn't exist yet

**Step 3: Rename and change return type**

Rename `parse_continuation_group` → `parse_continuation_groups` and change return type from `Result<ChangeGroup>` to `Result<Vec<ChangeGroup>>`.

For insert: create one `ChangeGroup` per `-m` arg (matching `run_write_insert` behavior).
For non-insert ops: wrap the single group in a `vec![]`.

```rust
fn parse_continuation_groups(
    op_name: &str,
    raw_changes: &[String],
) -> Result<Vec<ChangeGroup>> {
    if raw_changes.is_empty() {
        bail!("{op_name} continuation has no -m values");
    }

    if op_name == "insert" {
        // Insert: each -m arg gets its own ChangeGroup with its own index
        // (matching run_write_insert behavior)
        raw_changes
            .iter()
            .map(|s| {
                let (key, value) =
                    parse_key_value(s).context(format!("invalid key:value format: {s:?}"))?;
                let (field, index) = parse_indexed_key(&key).unwrap_or((key, 0));
                Ok(ChangeGroup {
                    changes: vec![(field, json!(value))],
                    op: MetadataOp::Insert(index),
                })
            })
            .collect()
    } else {
        let op = match op_name {
            "modify" => MetadataOp::Set,
            "append" => MetadataOp::Append,
            "append-list" => MetadataOp::AppendList,
            "remove" => MetadataOp::Remove,
            _ => bail!("unknown operation: {op_name}"),
        };
        let changes: Vec<(String, serde_json::Value)> = raw_changes
            .iter()
            .map(|s| {
                let (key, value) =
                    parse_key_value(s).context(format!("invalid key:value format: {s:?}"))?;
                Ok((key, json!(value)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(vec![ChangeGroup { changes, op }])
    }
}
```

**Step 4: Update callers to use `extend` instead of `push`**

In `run_write` and `run_write_insert`, the continuation processing block changes:

```rust
// REPLACE:
//     let group = parse_continuation_group(op_name, raw_changes)?;
//     change_groups.push(group);

// WITH:
    let groups = parse_continuation_groups(op_name, raw_changes)?;
    change_groups.extend(groups);
```

**Step 5: Update existing tests that call `parse_continuation_group`**

Rename all `parse_continuation_group(...)` calls to `parse_continuation_groups(...)` and adjust assertions for `Vec<ChangeGroup>` return type. The existing tests like `parse_continuation_modify`, `parse_continuation_insert_with_index`, `parse_continuation_remove` need to index into the returned Vec (`groups[0]` instead of `group`).

**Step 6: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass

**Step 7: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 8: Commit**

```
fix(ia-cli): insert continuations produce one ChangeGroup per -m arg

Previously, `parse_continuation_group` for insert ops collected all -m
args into a single ChangeGroup with only the last index, silently
discarding earlier indices. This meant `+ insert -m collection[0]:x -m
subject[2]:y` would apply both fields with Insert(2).

Now renamed to `parse_continuation_groups` returning Vec<ChangeGroup>,
matching the behavior of the primary `run_write_insert` which already
creates one group per -m arg. Callers use `extend` instead of `push`.
```

---

### Task 3: Fix false-positive argv detection in `extract_compound_from_argv`

**Why:** The current code uses `raw_args.iter().position(|a| a == "metadata")` which matches "metadata" anywhere in argv. Running `ia download metadata + remove -m x:y` silently strips the `+` and everything after, leaving `ia download metadata` — the compound ops are lost without any error.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (`extract_compound_from_argv`)
- Modify: `ia-cli/src/commands/metadata.rs` (unit tests in `compound_tests`)
- Modify: `ia-cli/tests/cli.rs` (CLI integration tests)

**Step 1: Write failing tests**

Add to the `compound_tests` module:

```rust
#[test]
fn extract_compound_download_metadata_not_detected() {
    // "metadata" is an identifier for download, not the subcommand
    let a = args("ia download metadata + remove -m x:y");
    let result = extract_compound_from_argv(&a).unwrap();
    assert!(result.is_none(), "should not detect compound ops for non-metadata subcommand");
}

#[test]
fn extract_compound_host_metadata_not_detected() {
    // "metadata" is the value of --host, not the subcommand
    let a = args("ia --host metadata metadata modify test -m title:New + remove -m x:y");
    let result = extract_compound_from_argv(&a).unwrap().unwrap();
    // Should find the SECOND "metadata" (the actual subcommand), not the first
    assert_eq!(
        result.filtered_argv,
        args("ia --host metadata metadata modify test -m title:New")
    );
}

#[test]
fn extract_compound_flags_before_metadata() {
    // Global flags before "metadata" should be skipped
    let a = args("ia -d -j 4 metadata modify test -m title:New + remove -m x:y");
    let result = extract_compound_from_argv(&a).unwrap().unwrap();
    assert_eq!(
        result.filtered_argv,
        args("ia -d -j 4 metadata modify test -m title:New")
    );
}

#[test]
fn extract_compound_list_metadata_not_detected() {
    // "metadata" is an identifier for list, not the subcommand
    let a = args("ia list metadata");
    let result = extract_compound_from_argv(&a).unwrap();
    assert!(result.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli extract_compound_download -- --nocapture`
Expected: FAIL — `extract_compound_download_metadata_not_detected` fails because the current code finds "metadata" as identifier

**Step 3: Replace naive position search with proper subcommand detection**

Replace `extract_compound_from_argv` with a version that finds the first positional arg (skipping global flags and their values) and checks if it's "metadata":

```rust
/// Global flags that consume the next argv element as a value.
const VALUE_TAKING_FLAGS: &[&str] = &[
    "-c", "--config-file",
    "-H", "--host",
    "--user-agent-suffix",
    "-j", "--jobs",
    "--joblog",
];

/// Find the position of "metadata" when it appears as the CLI subcommand
/// (first positional arg after the binary name and global flags).
///
/// Returns `None` if the subcommand is anything other than "metadata",
/// preventing false positives like `ia download metadata`.
fn find_metadata_subcommand_pos(args: &[String]) -> Option<usize> {
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        // Flags that consume a value: skip the next arg too
        if VALUE_TAKING_FLAGS.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        // Any other flag (boolean flags like -d, -l, -q, --retry-failed)
        if arg.starts_with('-') {
            continue;
        }
        // First positional arg = subcommand
        return if arg == "metadata" { Some(i) } else { None };
    }
    None
}

pub fn extract_compound_from_argv(raw_args: &[String]) -> Result<Option<CompoundFromArgv>> {
    let Some(meta_pos) = find_metadata_subcommand_pos(raw_args) else {
        return Ok(None);
    };

    let args_after_metadata = &raw_args[meta_pos + 1..];
    let Some(split) = split_compound_args(args_after_metadata)? else {
        return Ok(None);
    };

    let mut filtered = raw_args[..=meta_pos].to_vec();
    filtered.extend(split.primary_args);

    Ok(Some(CompoundFromArgv {
        filtered_argv: filtered,
        continuations: split.continuations,
    }))
}
```

**Step 4: Add unit tests for `find_metadata_subcommand_pos`**

```rust
#[test]
fn find_subcommand_bare_metadata() {
    let a = args("ia metadata modify test -m title:New");
    assert_eq!(find_metadata_subcommand_pos(&a), Some(1));
}

#[test]
fn find_subcommand_with_flags() {
    let a = args("ia -d -j 4 metadata modify test -m title:New");
    assert_eq!(find_metadata_subcommand_pos(&a), Some(4));
}

#[test]
fn find_subcommand_download_returns_none() {
    let a = args("ia download test-item");
    assert_eq!(find_metadata_subcommand_pos(&a), None);
}

#[test]
fn find_subcommand_host_metadata_skips_flag_value() {
    // --host takes a value, so "metadata" at position 3 is the flag value, not subcommand
    let a = args("ia --host metadata metadata modify test -m title:New");
    // The first positional is the second "metadata" (subcommand)
    assert_eq!(find_metadata_subcommand_pos(&a), Some(3));
}

#[test]
fn find_subcommand_config_file_skips_flag_value() {
    let a = args("ia -c /path/to/config metadata modify test -m title:New");
    assert_eq!(find_metadata_subcommand_pos(&a), Some(3));
}
```

**Step 5: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass

**Step 6: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 7: Commit**

```
fix(ia-cli): prevent false-positive compound detection on non-metadata commands

Previously extract_compound_from_argv used a naive
`args.position(|a| a == "metadata")` that matched "metadata" anywhere
in argv. This caused `ia download metadata + remove -m x:y` to silently
strip the compound args and just run `ia download metadata`.

Now uses find_metadata_subcommand_pos which properly skips the binary
name, global flags, and flag values to find the first positional arg.
Only proceeds if that first positional is "metadata". Commands like
`ia download metadata` and `ia --host metadata metadata ...` are now
handled correctly.
```

---

### Task 4: Consolidate parameters and eliminate duplication

**Why:** `run_write` has 8 parameters, `run_write_insert` has 7. The `continuations` parameter is threaded through 4 call sites with identical processing code duplicated in two functions. A `WriteContext` struct and early continuation processing eliminate both issues.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (multiple functions)

**Step 1: Create `WriteContext` struct**

Add near the top of the file (after the existing struct definitions, around line 240):

```rust
/// Runtime context for write operations — groups parameters that are
/// always passed together through the write call chain.
struct WriteContext {
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
}
```

**Step 2: Update `run_write_inner` signature**

```rust
// REPLACE 7 params:
async fn run_write_inner(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    change_groups: Vec<ChangeGroup>,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {

// WITH 4 params (remove #[allow(clippy::too_many_arguments)]):
async fn run_write_inner(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    change_groups: Vec<ChangeGroup>,
    ctx: &WriteContext,
) -> Result<()> {
```

Update all usages of `quiet`, `jobs`, `joblog_path` inside the function body to use `ctx.quiet`, `ctx.jobs`, `ctx.joblog_path`.

**Step 3: Update `run_write` and `run_write_insert`**

Both functions build change_groups and call `run_write_inner`. Update their signatures similarly:

```rust
async fn run_write(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    op: MetadataOp,
    continuations: Option<Vec<(String, Vec<String>)>>,
    ctx: &WriteContext,
) -> Result<()> {
    // ... parse primary changes ...
    // ... build change_groups + extend with continuations ...
    run_write_inner(client, input, write, change_groups, ctx).await
}

async fn run_write_insert(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    continuations: Option<Vec<(String, Vec<String>)>>,
    ctx: &WriteContext,
) -> Result<()> {
    // ... parse insert changes ...
    // ... build change_groups + extend with continuations ...
    run_write_inner(client, input, write, change_groups, ctx).await
}
```

**Step 4: Extract continuation processing into a helper**

The duplicated block becomes a function:

```rust
/// Parse raw continuation segments into ChangeGroups.
fn build_continuation_groups(
    continuations: Option<Vec<(String, Vec<String>)>>,
) -> Result<Vec<ChangeGroup>> {
    let Some(conts) = continuations else {
        return Ok(vec![]);
    };
    let mut groups = Vec::new();
    for (op_name, raw_changes) in &conts {
        groups.extend(parse_continuation_groups(op_name, raw_changes)?);
    }
    Ok(groups)
}
```

Then in both `run_write` and `run_write_insert`:

```rust
change_groups.extend(build_continuation_groups(continuations)?);
```

**Step 5: Update `run()` to construct `WriteContext`**

```rust
pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    continuations: Option<Vec<(String, Vec<String>)>>,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let ctx = WriteContext { quiet, jobs, joblog_path };
    match args.command {
        // ... pass &ctx instead of quiet, jobs, joblog_path ...
        Some(MetadataCommand::Modify(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Set, continuations, &ctx).await
        }
        // ... etc ...
    }
}
```

Also update `run_import` to take `&WriteContext`.

**Step 6: Remove `#[allow(clippy::too_many_arguments)]` annotations**

They should no longer be needed on `run_write`, `run_write_insert`, or `run_write_inner`. If clippy still warns (7+ args), reduce further or add targeted allows with a comment.

**Step 7: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass

**Step 8: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 9: Commit**

```
refactor(ia-cli): consolidate write parameters into WriteContext struct

Group quiet/jobs/joblog_path into WriteContext, eliminating 3 repeated
parameters across 4 functions. Extract build_continuation_groups helper
to remove duplicated continuation processing in run_write and
run_write_insert.

Removes #[allow(clippy::too_many_arguments)] — functions now have
4-5 params instead of 7-8.
```

---

### Task 5: Move `CompoundModifyRequest` imports to top of file

**Why:** Two `use ia_core::metadata::write::CompoundModifyRequest;` statements sit inside function bodies (lines 607, 938), inconsistent with the file's convention of top-level ia_core imports.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Add to top-of-file imports**

Update the existing import block (lines 15-18):

```rust
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, ChangeGroup,
    CompoundModifyRequest, MetadataOp, ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS, REMOVE_TAG,
};
```

Note: `ChangeGroup` should already be here from Task 1.

**Step 2: Remove the two function-body imports**

Delete these lines:
- `use ia_core::metadata::write::CompoundModifyRequest;` in `run_write_inner`
- `use ia_core::metadata::write::CompoundModifyRequest;` in `run_import`

**Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All tests pass

**Step 4: Run clippy**

Run: `cargo clippy -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 5: Commit**

```
style(ia-cli): move CompoundModifyRequest import to top of file

Consistent with the file's convention of top-level ia_core imports.
```

---

### Task 6: Remove dead `--spreadsheet` from SHARED_OPTIONS, add missing test

**Why:** `--spreadsheet` was replaced by the `import` subcommand during CLI restructuring. The reference in `SHARED_OPTIONS` is unreachable dead code. The actual guard is in `run()` which checks `if continuations.is_some()` for the `Import` variant. Also missing: a CLI integration test for `import + compound → error`.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (`SHARED_OPTIONS`)
- Modify: `ia-cli/tests/cli.rs` (add test)
- Modify: `docs/plans/2026-03-04-compound-metadata-ops-design.md` (fix outdated reference)

**Step 1: Remove `--spreadsheet` from SHARED_OPTIONS**

In `ia-cli/src/commands/metadata.rs`, remove `"--spreadsheet"` from the `SHARED_OPTIONS` const.

**Step 2: Add CLI integration test for import + compound error**

In `ia-cli/tests/cli.rs`, add to the compound metadata operations section:

```rust
#[test]
fn metadata_import_with_compound_errors() {
    // import uses its own column-prefix system, not compatible with +
    ia().args([
        "metadata", "import", "data.csv", "+", "remove", "-m", "x:y",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("compound operations (+) cannot be used with import"));
}
```

**Step 3: Update design doc**

In `docs/plans/2026-03-04-compound-metadata-ops-design.md`, update the "Spreadsheet Incompatibility" section (around lines 153-160) to reference the `import` subcommand instead of `--spreadsheet`:

```markdown
### Import Incompatibility

The `import` subcommand is **not compatible** with `+`. Import has its own column-prefix
system for mixed operations per-row (e.g., `append:description`, `remove:subject`).
If both are used:

```
error: compound operations (+) cannot be used with import
```
```

**Step 4: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass

**Step 5: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings

**Step 6: Commit**

```
fix(ia-cli): remove dead --spreadsheet from SHARED_OPTIONS, add import+compound test

--spreadsheet was replaced by the import subcommand during CLI
restructuring. The reference in SHARED_OPTIONS was unreachable. The
actual compound+import guard is in run() dispatch.

Also adds a CLI integration test for the import+compound error path
and updates the design doc to reference import instead of --spreadsheet.
```

---

## Final Verification

After all 6 tasks:

Run: `cargo test -p ia-core -p ia-cli`
Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Run: `cargo build -p ia-cli`

All must pass. Then update PR #200 description and force-push.
