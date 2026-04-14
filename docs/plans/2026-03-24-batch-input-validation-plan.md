# Batch Input Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce mutual exclusivity between identifier sources (`--search`, `--itemlist`, positional args, stdin) across all batch commands, reject invalid identifiers early, and consolidate duplicated collection logic into a shared function.

**Architecture:** New shared module `ia-cli/src/identifier.rs` with a single `collect_identifiers()` function. Clap `conflicts_with_all` attributes provide parse-time enforcement; the shared function provides runtime enforcement as belt-and-suspenders. Each command's custom collect function is replaced with a call to the shared one.

**Tech Stack:** Rust, clap (conflicts_with_all), ia-core (parse_identifier_line, validate_identifier, search::scrape)

---

### Task 1: Create shared `ia-cli/src/identifier.rs` module

**Files:**
- Create: `ia-cli/src/identifier.rs`
- Modify: `ia-cli/src/commands/mod.rs` (add `pub mod identifier` to parent — actually this lives at `ia-cli/src/`, not in commands/)
- Modify: `ia-cli/src/main.rs` (add `pub mod identifier`)

- [ ] **Step 1: Write failing tests for mutual exclusivity**

Create `ia-cli/src/identifier.rs` with test module. Tests call `collect_identifiers()` with multiple sources and assert errors.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn errors_when_search_and_itemlist_both_provided() {
        // Can't easily construct IaClient in unit tests, so test the
        // validation helper directly
        let err = validate_sources(
            &["id1".into()],
            Some(Path::new("list.txt")),
            Some("query"),
        ).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn errors_when_positional_and_search_both_provided() {
        let err = validate_sources(
            &["id1".into()],
            None,
            Some("query"),
        ).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn errors_when_positional_and_itemlist_both_provided() {
        let err = validate_sources(
            &["id1".into()],
            Some(Path::new("list.txt")),
            None,
        ).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn ok_when_only_positional() {
        validate_sources(&["id1".into()], None, None).unwrap();
    }

    #[tokio::test]
    async fn ok_when_only_search() {
        validate_sources(&[], None, Some("query")).unwrap();
    }

    #[tokio::test]
    async fn ok_when_only_itemlist() {
        validate_sources(&[], Some(Path::new("f.txt")), None).unwrap();
    }

    #[tokio::test]
    async fn ok_when_no_sources_stdin_fallback() {
        validate_sources(&[], None, None).unwrap();
    }

    #[test]
    fn rejects_whitespace_only_identifiers() {
        let err = validate_collected_identifiers(&[" ".into(), "  \t".into()])
            .unwrap_err();
        assert!(err.to_string().contains("invalid identifier"));
    }

    #[test]
    fn rejects_empty_identifiers() {
        let err = validate_collected_identifiers(&["".into()])
            .unwrap_err();
        assert!(err.to_string().contains("invalid identifier"));
    }

    #[test]
    fn accepts_valid_identifiers() {
        validate_collected_identifiers(&["nasa".into(), "my-item-123".into()])
            .unwrap();
    }

    #[test]
    fn deduplicates_preserving_order() {
        let input = vec!["aaa".into(), "bbb".into(), "aaa".into(), "ccc".into()];
        let result = dedup_identifiers(input);
        assert_eq!(result, vec!["aaa", "bbb", "ccc"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --lib identifier -- --nocapture`
Expected: compile error (functions don't exist yet)

- [ ] **Step 3: Implement the shared module**

```rust
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{bail, Context, Result};
use futures::StreamExt;

use ia_core::identifier::{parse_identifier_line, validate_identifier};
use ia_core::search::SearchOpts;
use ia_core::IaClient;

/// Validate that at most one identifier source is active.
///
/// Sources: positional args, `--itemlist`, `--search`. Stdin is a fallback
/// (only used when all three are absent) and is not counted as a source.
pub fn validate_sources(
    positional: &[String],
    itemlist: Option<&Path>,
    search: Option<&str>,
) -> Result<()> {
    let count = [
        !positional.is_empty(),
        itemlist.is_some(),
        search.is_some(),
    ]
    .iter()
    .filter(|&&b| b)
    .count();

    if count > 1 {
        let mut active = Vec::new();
        if !positional.is_empty() {
            active.push("positional identifiers");
        }
        if itemlist.is_some() {
            active.push("--itemlist");
        }
        if search.is_some() {
            active.push("--search");
        }
        bail!(
            "identifier sources are mutually exclusive, but got: {}",
            active.join(", ")
        );
    }
    Ok(())
}

/// Validate all collected identifiers (trim + validate).
///
/// Returns the first validation error encountered.
pub fn validate_collected_identifiers(ids: &[String]) -> Result<()> {
    for id in ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            bail!("invalid identifier: got empty or whitespace-only value");
        }
        validate_identifier(trimmed)
            .context(format!("invalid identifier: {id:?}"))?;
    }
    Ok(())
}

/// Deduplicate identifiers preserving first-seen order.
pub fn dedup_identifiers(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|id| seen.insert(id.clone())).collect()
}

/// Collect identifiers from exactly one source: positional args, `--itemlist`,
/// `--search`, or stdin (fallback). Validates mutual exclusivity and each
/// identifier.
pub async fn collect_identifiers(
    positional: &[String],
    itemlist: Option<&Path>,
    search: Option<(&str, &SearchOpts)>,
    client: &IaClient,
) -> Result<Vec<String>> {
    validate_sources(
        positional,
        itemlist,
        search.map(|(_, _)| "").map(|_| search.unwrap().0).map(|_| std::path::Path::new("")),
        // Simplified:
    )?;

    // (See actual implementation below — this is simplified for the plan)

    let mut ids = Vec::new();

    if !positional.is_empty() {
        ids.extend(positional.iter().map(|s| s.trim().to_string()));
    } else if let Some(path) = itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            if let Some(id) = parse_identifier_line(line) {
                ids.push(id);
            }
        }
    } else if let Some((query, opts)) = search {
        let mut stream = ia_core::search::scrape(client, query, opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    } else if !std::io::stdin().is_terminal() {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.context("failed to read from stdin")?;
            if let Some(id) = parse_identifier_line(&line) {
                ids.push(id);
            }
        }
    }

    let ids = dedup_identifiers(ids);
    validate_collected_identifiers(&ids)?;

    Ok(ids)
}
```

**Note:** The actual `validate_sources` call in `collect_identifiers` should be:
```rust
validate_sources(
    positional,
    itemlist,
    search.map(|(q, _)| q),
)?;
```

- [ ] **Step 4: Register module in `ia-cli/src/main.rs`**

Add `pub mod identifier;` alongside the existing module declarations.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-cli --lib identifier -- --nocapture`
Expected: all tests PASS

- [ ] **Step 6: Commit**

```bash
git add ia-cli/src/identifier.rs ia-cli/src/main.rs
git commit -m "feat: add shared batch identifier collection with mutual exclusivity

Adds ia-cli/src/identifier.rs with:
- validate_sources(): enforces mutual exclusivity between positional
  args, --itemlist, and --search
- collect_identifiers(): single entry point for all batch commands
- validate_collected_identifiers(): rejects whitespace/invalid IDs
- dedup_identifiers(): preserves first-seen order
"
```

---

### Task 2: Add clap `conflicts_with_all` to command structs

**Files:**
- Modify: `ia-cli/src/commands/download.rs` (DownloadArgs)
- Modify: `ia-cli/src/commands/metadata.rs` (BatchInput, ExportArgs)
- Modify: `ia-cli/src/commands/tasks.rs` (SubmitArgs)
- Modify: `ia-cli/src/commands/ai.rs` (QaArgs)

- [ ] **Step 1: Write CLI integration tests for conflicts**

Add tests to `ia-cli/tests/cli.rs` (or a new `ia-cli/tests/batch_input.rs`):

```rust
use assert_cmd::Command;
use predicates::prelude::*;

fn ia_cmd() -> Command {
    Command::cargo_bin("ia").unwrap()
}

#[test]
fn download_search_and_itemlist_conflict() {
    ia_cmd()
        .args(["download", "--search", "collection:test", "--itemlist", "ids.txt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn download_identifier_and_search_conflict() {
    ia_cmd()
        .args(["download", "my-item", "--search", "collection:test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn download_identifier_and_itemlist_conflict() {
    ia_cmd()
        .args(["download", "my-item", "--itemlist", "ids.txt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn metadata_modify_search_and_itemlist_conflict() {
    ia_cmd()
        .args(["metadata", "modify", "--search", "x", "--itemlist", "f.txt", "-t", "k:v"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn tasks_submit_identifier_and_search_conflict() {
    ia_cmd()
        .args(["tasks", "submit", "my-item", "--search", "x", "--cmd", "derive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --test batch_input -- --nocapture`
Expected: FAIL (no conflicts enforced yet)

- [ ] **Step 3: Add `conflicts_with_all` attributes**

**download.rs — DownloadArgs:**
```rust
/// Item identifier to download
#[arg(conflicts_with_all = ["itemlist", "search"])]
pub identifier: Option<String>,

/// File containing item identifiers (one per line)
#[arg(long, conflicts_with_all = ["identifier", "search"])]
itemlist: Option<PathBuf>,

/// Download items matching a search query
#[arg(short = 's', long, conflicts_with_all = ["identifier", "itemlist"])]
search: Option<String>,
```

**metadata.rs — BatchInput:**
```rust
/// Item identifier(s)
#[arg(conflicts_with_all = ["itemlist", "search"])]
pub identifiers: Vec<String>,

/// Read identifiers from file (one per line)
#[arg(long, conflicts_with_all = ["identifiers", "search"])]
pub itemlist: Option<PathBuf>,

/// Use search results as input
#[arg(long, conflicts_with_all = ["identifiers", "itemlist"])]
pub search: Option<String>,
```

**metadata.rs — ExportArgs:**
```rust
/// Input files
#[arg(conflicts_with_all = ["itemlist", "search"])]
pub files: Vec<PathBuf>,

/// Read identifiers from file
#[arg(long, conflicts_with_all = ["files", "search"])]
pub itemlist: Option<PathBuf>,

/// Use search results as input
#[arg(long, conflicts_with_all = ["files", "itemlist"])]
pub search: Option<String>,
```

**tasks.rs — SubmitArgs:**
```rust
/// Item identifier
#[arg(conflicts_with_all = ["itemlist", "search"])]
pub identifier: Option<String>,

/// Read identifiers from file
#[arg(long, conflicts_with_all = ["identifier", "search"])]
pub itemlist: Option<std::path::PathBuf>,

/// Use search results as input
#[arg(long, conflicts_with_all = ["identifier", "itemlist"])]
pub search: Option<String>,
```
(Note: `--spreadsheet` already conflicts with all three.)

**ai.rs — QaArgs:**
```rust
/// Item identifier(s) to QA
#[arg(conflicts_with_all = ["itemlist", "search"])]
pub identifiers: Vec<String>,

/// Read identifiers from file
#[arg(long, conflicts_with_all = ["identifiers", "search"])]
pub itemlist: Option<PathBuf>,

/// QA items matching a search query
#[arg(long, conflicts_with_all = ["identifiers", "itemlist"])]
pub search: Option<String>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --test batch_input -- --nocapture`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/download.rs ia-cli/src/commands/metadata.rs \
       ia-cli/src/commands/tasks.rs ia-cli/src/commands/ai.rs \
       ia-cli/tests/batch_input.rs
git commit -m "fix: enforce mutual exclusivity between --search, --itemlist, and positional args

Adds conflicts_with_all attributes to all batch command structs so clap
rejects conflicting sources at parse time with clear error messages.

Affected commands: download, metadata (batch + export), tasks submit, ai qa
"
```

---

### Task 3: Replace per-command collect functions with shared implementation

**Files:**
- Modify: `ia-cli/src/commands/download.rs` — remove `collect_identifiers`, use shared
- Modify: `ia-cli/src/commands/metadata.rs` — remove `collect_identifiers_from_batch` and `collect_identifiers_from_export`, use shared
- Modify: `ia-cli/src/commands/tasks.rs` — remove `collect_submit_identifiers`, use shared
- Modify: `ia-cli/src/commands/ai.rs` — remove `collect_identifiers`, use shared

- [ ] **Step 1: Run full test suite to establish baseline**

Run: `just ci`
Expected: all pass (green baseline)

- [ ] **Step 2: Replace download.rs collect_identifiers**

Replace the `collect_identifiers` function (lines 138-188) with a call to the shared function. Keep the download-specific file-arg validation (`files require an identifier`, `cannot combine files with --itemlist/--search`) as pre-checks before calling the shared function.

```rust
use crate::identifier;

// In run():
// Download-specific: file args require single-item mode
if !args.files.is_empty() && args.identifier.is_none() {
    bail!("file names require an identifier: ia download <identifier> <file> [file ...]");
}

let search_opts = args.search.as_deref().map(|q| (q, &SearchOpts::default()));
let positional: Vec<String> = args.identifier.iter().cloned().collect();
let ids = identifier::collect_identifiers(
    &positional,
    args.itemlist.as_deref(),
    search_opts,
    client,
).await?;
```

Delete the old `collect_identifiers` function.

- [ ] **Step 3: Replace metadata.rs collect functions**

Replace `collect_identifiers_from_batch` (lines 1666-1708):
```rust
use crate::identifier;

// In the batch write runner:
let search_opts = input.search.as_deref().map(|q| (q, &SearchOpts::default()));
let ids = identifier::collect_identifiers(
    &input.identifiers,
    input.itemlist.as_deref(),
    search_opts,
    client,
).await?;
```

Replace `collect_identifiers_from_export` (lines 823-891) — this one reads from spreadsheet files first, then passes to shared:
```rust
// In run_export:
// Export-specific: read identifiers from input spreadsheet files
let mut file_ids = Vec::new();
for path in &args.files {
    let ids = ia_core::spreadsheet::read_identifiers_from_file(path)
        .context(format!("failed to read identifiers from {}", path.display()))?;
    file_ids.extend(ids);
}

// Shared collection (files act as positional source)
let positional = if !file_ids.is_empty() {
    file_ids
} else {
    Vec::new()
};
let search_opts = args.search.as_deref().map(|q| (q, &SearchOpts::default()));
let ids = identifier::collect_identifiers(
    &positional,
    args.itemlist.as_deref(),
    search_opts,
    client,
).await?;

if ids.is_empty() {
    bail!(
        "No input provided. Pass files, --search, or pipe identifiers via stdin.\n\
         Examples:\n  \
         ia metadata export items.csv\n  \
         ia metadata export --search \"collection:nasa\"\n  \
         echo id1 | ia metadata export"
    );
}
```

Delete both old functions.

- [ ] **Step 4: Replace tasks.rs collect_submit_identifiers**

Replace `collect_submit_identifiers` (lines 1131-1176):
```rust
use crate::identifier;

// In run_submit:
let positional: Vec<String> = args.identifier.iter().cloned().collect();
let search_opts = args.search.as_deref().map(|q| {
    (q, &ia_core::search::SearchOpts::default())
});
let ids = identifier::collect_identifiers(
    &positional,
    args.itemlist.as_deref(),
    search_opts,
    client,
).await?;
```

Delete the old function.

- [ ] **Step 5: Replace ai.rs collect_identifiers**

Replace `collect_identifiers` (lines 1255-1300):
```rust
use crate::identifier;

// In run_qa:
let search_opts = if let Some(ref query) = qa_args.search {
    let params = crate::commands::search::parse_extra_params(&qa_args.search_parameters)?;
    Some((query.as_str(), SearchOpts { params, ..SearchOpts::default() }))
} else {
    None
};
// Need to handle the owned SearchOpts carefully — build it before the call
let ids = identifier::collect_identifiers(
    &qa_args.identifiers,
    qa_args.itemlist.as_deref(),
    search_opts.as_ref().map(|(q, o)| (*q, o)),
    client,
).await?;
```

Delete the old function.

- [ ] **Step 6: Remove unused imports from all modified files**

Each file previously imported `parse_identifier_line`, `IsTerminal`, `SearchOpts` etc. for the local collect function. Remove any that are no longer needed.

- [ ] **Step 7: Run full test suite**

Run: `just ci`
Expected: all pass (fmt, check, test, doc)

- [ ] **Step 8: Commit**

```bash
git add ia-cli/src/commands/download.rs ia-cli/src/commands/metadata.rs \
       ia-cli/src/commands/tasks.rs ia-cli/src/commands/ai.rs \
       ia-cli/src/identifier.rs
git commit -m "refactor: replace per-command collect functions with shared identifier module

Removes 5 duplicated collect_identifiers functions from download, metadata
(batch + export), tasks submit, and ai qa. All now delegate to the shared
identifier::collect_identifiers() which enforces mutual exclusivity,
validates identifiers, and deduplicates.

Each command retains its own specific pre-checks (e.g., download's file-arg
validation, export's spreadsheet reading, ai's search_parameters).
"
```

---

### Task 4: Final verification and cleanup

- [ ] **Step 1: Run full CI**

Run: `just ci`
Expected: all pass

- [ ] **Step 2: Manual smoke test**

```bash
# Should error: conflicting sources
cargo run -p ia-cli -- download my-item --search "collection:test"
cargo run -p ia-cli -- download my-item --itemlist /dev/null
cargo run -p ia-cli -- metadata modify --search "x" --itemlist /dev/null -t k:v

# Should work: single source
cargo run -p ia-cli -- download --search "identifier:nasa" --dry-run
```

- [ ] **Step 3: Commit any final fixups if needed**
