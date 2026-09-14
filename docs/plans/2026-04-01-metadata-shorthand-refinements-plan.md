# Metadata Shorthand Refinements Implementation Plan

**Goal:** Refine PR #297's metadata shorthand with cleaner help text, robust `--target` detection, batch `--exists`/`--formats` support, stdin filtering, and integration tests.

**Architecture:** All changes are in `ia-cli/src/commands/metadata.rs` (struct + dispatch logic) and `ia-cli/tests/cli.rs` (integration tests). No ia-core changes needed — `parse_identifier_line` already filters empty lines, `item_exists` and `get_item` already exist.

**Tech Stack:** Rust, clap (arg parsing), assert_cmd + predicates (integration tests), tokio + JoinSet (concurrency), console (styled warnings)

---

## File Map

- **Modify:** `ia-cli/src/commands/metadata.rs` — `MetadataArgs` struct, `run()` dispatch, new `run_exists_multi` and `run_formats_multi` functions, help text
- **Modify:** `ia-cli/tests/cli.rs` — new integration tests at end of file

---

### Task 1: Help Text Cleanup

Trim the `after_long_help` examples and hide advanced write options from `ia metadata --help`.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:350-450`

- [ ] **Step 1: Update `after_long_help` examples**

Replace the current `after_long_help` block in `MetadataArgs` (lines 358-392) with:

```rust
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Read metadata</dim>\
         \n  <bold>$ ia metadata nasa</bold>\
         \n  <bold>$ ia metadata nasa --exists</bold>\
         \n\
         \n  <dim># Batch read</dim>\
         \n  <bold>$ ia metadata --search \"collection:nasa\"</bold>\
         \n  <bold>$ ia metadata --itemlist ids.txt</bold>\
         \n  <bold>$ echo id1 | ia metadata</bold>\
         \n\
         \n  <dim># Modify metadata (shorthand for 'ia metadata modify')</dim>\
         \n  <bold>$ ia metadata nasa -m \"title:New Title\"</bold>\
         \n  <bold>$ ia metadata nasa -m \"subject:rockets\" --dry-run</bold>\
         \n\
         \n  <dim># Batch modify</dim>\
         \n  <bold>$ ia metadata --search \"collection:test\" -m \"subject:updated\"</bold>\
         \n  <bold>$ ia metadata --itemlist ids.txt -m \"subject:new-tag\"</bold>\
         \n\
         \n  <dim># Compound operations (single request)</dim>\
         \n  <bold>$ ia metadata nasa -m \"title:New\" + remove -m \"subject:old\"</bold>\
         \n\
         \n  <dim># See 'ia metadata modify --help' for write options (--target, --expect, etc.)</dim>\
         \n\
         \n  <dim># Export to file</dim>\
         \n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.xlsx</bold>\
         \n\
         \n  <dim># Batch import from spreadsheet</dim>\
         \n  <bold>$ ia metadata --spreadsheet data.csv --dry-run</bold>\
         \n\
         \n  <dim># Browse metadata field definitions</dim>\
         \n  <bold>$ ia metadata schema</bold>\n"
    ),
```

- [ ] **Step 2: Add `hide = true` to advanced write options**

Change these four fields in `MetadataArgs`:

```rust
    // ── Write-mode options (hidden from top-level help; see 'ia metadata modify --help') ──
    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, default_value = "metadata", hide = true)]
    pub target: String,

    /// Optimistic concurrency check (repeatable, field:expected_value)
    #[arg(long, hide = true)]
    pub expect: Vec<String>,

    /// Task priority (default: 0 single, -5 batch)
    #[arg(long, hide = true)]
    pub priority: Option<i32>,

    /// Accept reduced priority to reduce rate limiting
    #[arg(long, hide = true)]
    pub reduced_priority: bool,
```

- [ ] **Step 3: Verify help output**

Run: `cargo run -p ia-cli -- metadata --help 2>&1`

Expected: Examples section matches the new layout. `--target`, `--expect`, `--priority`, `--reduced-priority` do NOT appear in the options list. `-m` and `--dry-run` DO appear.

- [ ] **Step 4: Update help integration test**

In `ia-cli/tests/cli.rs`, the existing test `metadata_help_mentions_compound_ops` (line 1045) should still pass. Verify:

Run: `cargo test -p ia-cli --test cli metadata_help_mentions_compound_ops -- --exact`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "refactor: clean up ia metadata --help text

Trim modify examples to essentials, add pointer to 'ia metadata modify --help'
for write options. Hide --target, --expect, --priority, --reduced-priority from
top-level help with #[arg(hide = true)] — they still work but don't clutter
the help output."
```

---

### Task 2: Change `--target` to `Option<String>`

Make `MetadataArgs.target` an `Option<String>` so read mode can cleanly detect explicit usage.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:434-436` (struct field)
- Modify: `ia-cli/src/commands/metadata.rs:574-612` (dispatch logic)

- [ ] **Step 1: Write failing unit test**

Add to the `args_parsing_tests` module at the bottom of `metadata.rs`:

```rust
    #[test]
    fn target_is_none_by_default() {
        let a = parse("nasa");
        assert!(a.target.is_none());
    }

    #[test]
    fn target_some_when_explicit() {
        let a = parse("nasa -m title:X --target files/f.txt");
        assert_eq!(a.target.as_deref(), Some("files/f.txt"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli target_is_none -- --exact 2>&1 | head -20`

Expected: FAIL — `target` is currently `String` not `Option<String>`, so `a.target.is_none()` won't compile.

- [ ] **Step 3: Change the struct field**

In `MetadataArgs`, change:

```rust
    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, hide = true)]
    pub target: Option<String>,
```

Remove `default_value = "metadata"`.

- [ ] **Step 4: Update the write-mode dispatch (shorthand path)**

In the `None =>` arm where `-m` is handled (around line 582), change:

```rust
                let write = WriteOpts {
                    metadata: args.metadata,
                    target: args.target.unwrap_or_else(|| "metadata".into()),
                    expect: args.expect,
                    priority: args.priority,
                    reduced_priority: args.reduced_priority,
                    dry_run: args.dry_run,
                    json: args.json,
                };
```

- [ ] **Step 5: Update the read-mode `--target` validation**

Replace:

```rust
            if args.target != "metadata" {
                bail!("--target requires -m or a write subcommand");
            }
```

With:

```rust
            if args.target.is_some() {
                bail!("--target requires -m or a write subcommand");
            }
```

- [ ] **Step 6: Fix existing unit test for target default**

In `args_parsing_tests`, update `target_defaults_to_metadata`:

```rust
    #[test]
    fn target_defaults_to_metadata() {
        let a = parse("nasa -m title:X");
        // No explicit --target → None, which run() resolves to "metadata"
        assert!(a.target.is_none());
    }
```

And update `target_without_metadata_parses_ok`:

```rust
    #[test]
    fn target_without_metadata_parses_ok() {
        let a = parse("nasa --target files/x");
        assert_eq!(a.target.as_deref(), Some("files/x"));
        assert!(a.metadata.is_empty());
    }
```

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ia-cli 2>&1 | tail -5`

Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "refactor: change MetadataArgs.target to Option<String>

Cleanly distinguishes 'user passed --target' from 'default' so read mode
can detect and reject explicit --target without -m. WriteOpts.target
(shared by subcommands) stays as String with default_value."
```

---

### Task 3: Stdin Empty-Line Filtering and Warning

Change the "no identifiers" error to a warning + exit 0, since empty input is not an error.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:614-638` (batch read path)
- Test: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Write integration test for empty stdin**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_empty_stdin_warns_and_exits_zero() {
    ia().args(["metadata"])
        .write_stdin("\n  \n\n")
        .assert()
        .success()
        .stderr(predicate::str::contains("no identifiers found"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli --test cli metadata_empty_stdin_warns -- --exact`

Expected: FAIL — currently bails with an error (exit code 1).

- [ ] **Step 3: Implement the warning**

In the `None =>` arm, replace the batch read "no identifiers" `bail!` (around line 627-629):

```rust
                let identifiers = collect_identifiers_from_batch(&input, client).await?;
                if identifiers.is_empty() {
                    let source = if input.search.is_some() {
                        "--search"
                    } else if input.itemlist.is_some() {
                        "--itemlist"
                    } else {
                        "stdin"
                    };
                    eprintln!("{}: no identifiers found from {source}", style("warning").yellow().bold());
                    return Ok(());
                }
```

- [ ] **Step 4: Run integration test**

Run: `cargo test -p ia-cli --test cli metadata_empty_stdin_warns -- --exact`

Expected: PASS

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ia-cli 2>&1 | tail -5`

Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "fix: warn instead of error on empty stdin/itemlist input

Empty input (blank lines only) from stdin, --itemlist, or --search is not
an error — it's just nothing to do. Print a warning to stderr and exit 0
so pipe consumers get an empty stream without a nonzero exit code."
```

---

### Task 4: Batch `--exists` Support

Allow `--exists` with `--search`/`--itemlist`/stdin. Matches single-item semantics: silent by default, exit 1 if any don't exist, JSONL with `--json`.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs:614-638` (batch read dispatch)
- Modify: `ia-cli/src/commands/metadata.rs` (new `run_exists_multi` function, near `run_read_multi`)
- Test: `ia-cli/src/commands/metadata.rs` (unit tests in `args_parsing_tests`)
- Test: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Write integration test for batch exists**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_batch_exists_with_itemlist_accepted() {
    // Clap should accept --exists with --itemlist (no longer rejected).
    // Will fail on network/file read, but should NOT fail on arg parsing.
    ia().args(["metadata", "--itemlist", "/tmp/nonexistent_ia_test_ids.txt", "--exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with").not());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli --test cli metadata_batch_exists_with_itemlist_accepted -- --exact`

Expected: FAIL — currently bails with "cannot be used with --search, --itemlist, or stdin input".

- [ ] **Step 3: Add `run_exists_multi` function**

Add after the existing `run_read_multi` function (around line 785):

```rust
async fn run_exists_multi(
    client: &IaClient,
    identifiers: &[String],
    json: bool,
    jobs: usize,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(jobs));
    let client = Arc::new(client.clone());
    let mut set = JoinSet::new();

    for (idx, id) in identifiers.iter().enumerate() {
        let sem = semaphore.clone();
        let client = client.clone();
        let id = id.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await?;
            let exists = client
                .item_exists(&id)
                .await
                .context(format!("failed to check existence of {id}"))?;
            Ok::<_, anyhow::Error>((idx, id, exists))
        });
    }

    let mut results: Vec<(usize, String, bool)> = Vec::new();
    while let Some(result) = set.join_next().await {
        results.push(result??);
    }
    results.sort_by_key(|(idx, _, _)| *idx);

    let any_missing = results.iter().any(|(_, _, exists)| !exists);
    if json {
        for (_, id, exists) in &results {
            println!(
                "{}",
                serde_json::json!({"identifier": id, "exists": exists})
            );
        }
    }

    if any_missing {
        std::process::exit(1);
    }
    Ok(())
}
```

- [ ] **Step 4: Update batch read dispatch to route `--exists`**

In the batch read section of the `None =>` arm (after the empty-identifiers warning), replace the `--exists`/`--formats` rejection and the `run_read_multi` call. The full block from `// Batch read:` onward should become:

```rust
            // Batch read: --search / --itemlist / stdin
            let has_batch_input = args.search.is_some() || args.itemlist.is_some();
            if has_batch_input || (args.identifiers.is_empty() && !std::io::stdin().is_terminal()) {
                let input = BatchInput {
                    identifiers: args.identifiers,
                    itemlist: args.itemlist,
                    search: args.search,
                };
                let identifiers = collect_identifiers_from_batch(&input, client).await?;
                if identifiers.is_empty() {
                    let source = if input.search.is_some() {
                        "--search"
                    } else if input.itemlist.is_some() {
                        "--itemlist"
                    } else {
                        "stdin"
                    };
                    eprintln!("{}: no identifiers found from {source}", style("warning").yellow().bold());
                    return Ok(());
                }

                if args.exists {
                    return run_exists_multi(client, &identifiers, args.json, ctx.jobs).await;
                }
                if args.formats {
                    return run_formats_multi(client, &identifiers, args.json, ctx.jobs).await;
                }
                return run_read_multi(
                    client,
                    &identifiers,
                    args.pretty,
                    args.json,
                    ctx.quiet,
                    ctx.jobs,
                )
                .await;
            }
```

Note: `run_formats_multi` doesn't exist yet — it will be added in Task 5. For now, create a placeholder that bails:

```rust
async fn run_formats_multi(
    _client: &IaClient,
    _identifiers: &[String],
    _json: bool,
    _jobs: usize,
) -> Result<()> {
    bail!("batch --formats: implemented in next task")
}
```

- [ ] **Step 5: Remove the unit test that expected rejection**

In `args_parsing_tests`, the test `conflict_metadata_with_exists` tests clap-level conflict between `-m` and `--exists` — that should still exist. But there's no existing unit test for the batch rejection since it was a runtime check.

Verify no test references the old "cannot be used with --search, --itemlist, or stdin" message:

Run: `cargo test -p ia-cli 2>&1 | grep -i "cannot be used with.*search"`

Expected: No test failures related to this message.

- [ ] **Step 6: Run tests**

Run: `cargo test -p ia-cli 2>&1 | tail -5`

Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: support batch --exists with --search/--itemlist/stdin

Matches single-item semantics: silent by default, exit 1 if any items
don't exist. With --json, outputs one JSONL line per identifier with
an 'exists' boolean field."
```

---

### Task 5: Batch `--formats` Support

Allow `--formats` with `--search`/`--itemlist`/stdin. Outputs JSONL with `identifier` field added.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (replace `run_formats_multi` placeholder)
- Test: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Write integration test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_batch_formats_with_itemlist_accepted() {
    ia().args(["metadata", "--itemlist", "/tmp/nonexistent_ia_test_ids.txt", "--formats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with").not());
}
```

- [ ] **Step 2: Run test to verify it passes**

This should already pass since Task 4 removed the rejection. Confirm:

Run: `cargo test -p ia-cli --test cli metadata_batch_formats_with_itemlist_accepted -- --exact`

Expected: PASS (fails on file read, not arg parsing).

- [ ] **Step 3: Implement `run_formats_multi`**

Replace the placeholder `run_formats_multi` with:

```rust
async fn run_formats_multi(
    client: &IaClient,
    identifiers: &[String],
    _json: bool,
    jobs: usize,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(jobs));
    let client = Arc::new(client.clone());
    let mut set = JoinSet::new();

    for (idx, id) in identifiers.iter().enumerate() {
        let sem = semaphore.clone();
        let client = client.clone();
        let id = id.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await?;
            let item = client
                .get_item(&id)
                .await
                .context(format!("failed to fetch metadata for {id}"))?;
            let mut fmts: Vec<String> = item.files.iter().filter_map(|f| f.format.clone()).collect();
            fmts.sort();
            fmts.dedup();
            Ok::<_, anyhow::Error>((idx, id, fmts))
        });
    }

    let mut results: Vec<(usize, String, Vec<String>)> = Vec::new();
    while let Some(result) = set.join_next().await {
        results.push(result??);
    }
    results.sort_by_key(|(idx, _, _)| *idx);

    // Always JSONL — batch output needs per-identifier attribution.
    // Single-item --formats can print one format per line since the
    // identifier is implicit, but batch mode always needs structure.
    for (_, id, fmts) in &results {
        println!(
            "{}",
            serde_json::json!({"identifier": id, "formats": fmts})
        );
    }

    Ok(())
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p ia-cli 2>&1 | tail -5`

Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: support batch --formats with --search/--itemlist/stdin

Outputs JSONL with identifier and formats array per item. Always uses
JSONL (even without --json) since batch output needs per-identifier
attribution."
```

---

### Task 6: Integration Tests for Runtime Validation

Add CLI integration tests for the write-only option rejection in read mode.

**Files:**
- Modify: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Add runtime validation error tests**

Append to `ia-cli/tests/cli.rs`:

```rust
// ─── Metadata shorthand: runtime validation ─────────────────────────────────

#[test]
fn metadata_dry_run_without_m_errors() {
    ia().args(["metadata", "nasa", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires -m or a write subcommand"));
}

#[test]
fn metadata_target_without_m_errors() {
    ia().args(["metadata", "nasa", "--target", "files/foo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires -m or a write subcommand"));
}

#[test]
fn metadata_expect_without_m_errors() {
    ia().args(["metadata", "nasa", "--expect", "title:old"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires -m or a write subcommand"));
}

#[test]
fn metadata_priority_without_m_errors() {
    ia().args(["metadata", "nasa", "--priority", "5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires -m or a write subcommand"));
}

#[test]
fn metadata_reduced_priority_without_m_errors() {
    ia().args(["metadata", "nasa", "--reduced-priority"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires -m or a write subcommand"));
}

#[test]
fn metadata_shorthand_m_accepted() {
    // -m at top level should be accepted by clap (will fail on network, not parsing)
    ia().args(["metadata", "nasa", "-m", "title:Test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument").not());
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p ia-cli --test cli metadata_dry_run_without_m -- --exact`

Expected: PASS

Run: `cargo test -p ia-cli --test cli metadata_target_without_m -- --exact`

Expected: PASS

Run: `cargo test -p ia-cli --test cli metadata_shorthand_m_accepted -- --exact`

Expected: PASS

- [ ] **Step 3: Run full test suite**

Run: `cargo test -p ia-cli 2>&1 | tail -5`

Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add ia-cli/tests/cli.rs
git commit -m "test: add integration tests for metadata shorthand validation

Cover all five write-only options (--dry-run, --target, --expect,
--priority, --reduced-priority) being rejected without -m, plus
verify -m shorthand is accepted at the clap level."
```

---

### Task 7: Final Verification

Run the full CI suite and verify everything is clean.

**Files:** None (verification only)

- [ ] **Step 1: Run `just ci`**

Run: `just ci`

Expected: fmt-check, check, test, doc all pass.

- [ ] **Step 2: Verify test count increased**

Run: `cargo test -p ia-cli 2>&1 | grep "test result"`

Expected: Test count should be higher than the PR's original 1,350 (new integration + unit tests added).

- [ ] **Step 3: Push**

```bash
git push
```
