# CLI Subcommand Restructuring — Implementation Plan

**Goal:** Restructure `ia metadata`, `ia search`, and `ia ai` commands to use sub-subcommands, following the pattern established by `ia config`.

**Architecture:** Each restructured command gets a `Command` enum (via `#[derive(Subcommand)]`) dispatching to focused subcommand structs. Shared options use `#[command(flatten)]` to avoid duplication. Bare forms (`ia metadata ID`, `ia search 'query'`, `ia ai ID`) are preserved via `subcommand_required = false` with fallback positional args.

**Tech Stack:** Rust, clap 4 (derive), existing ia-core library functions unchanged. CLI-only refactor — no ia-core API changes except FTS `!L` prefix fix.

**Design doc:** `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`

---

## Task 0: Create GitHub Issues

**Purpose:** Track all work before writing code. Each issue must have labels, description, and reference to the design doc.

Create these issues on `internetarchivecanada/ia`:

### Issue 1: `ia search` — Split into backend subcommands (scrape, advanced, fts)

**Labels:** `enhancement`, `cli`
**Body:**
```
Split `ia search` into backend-specific subcommands: `scrape` (default), `advanced`, `fts`.

Each subcommand documents only the options its backend supports. Bare `ia search 'query'` defaults to scrape for soft migration.

Key changes:
- `SearchCommand` enum: Scrape, Advanced, Fts
- Each backend gets focused arg struct with only its supported options
- Remove `--fts` flag (replaced by `fts` subcommand)
- Remove `-n, --count` (use `head` or `-p` params instead)
- Advanced search: single page only (no auto-pagination)
- FTS: add `--dsl`, `--scope`, `--size`, `--from` options
- Shared options: `-n/--num-found`, `--itemlist`, `--json`, `-p/--parameters`, `--timeout`

Design doc: `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`
```

### Issue 2: FTS `!L` prefix bug fix

**Labels:** `bug`, `search`
**Body:**
```
The Python `internetarchive` library prepends `!L` to non-DSL FTS queries for literal text search. The Rust implementation sends raw queries without this prefix, producing different search results.

Fix: In `ia-core/src/search.rs` `fts()` function, prepend `!L ` to the query string by default. Add a `dsl` field to `SearchOpts` (or a new `FtsOpts` struct) — when true, skip the prefix.

The new `ia search fts --dsl` flag maps to this.

Design doc: `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`
```

### Issue 3: `ia ai` — Extract `undo` as subcommand

**Labels:** `enhancement`, `cli`
**Body:**
```
Extract `--undo` from `ia ai` into a proper `ia ai undo <JOBLOG>` subcommand.

Currently `--undo` conflicts with `--headless`, `--record-only`, and `--dry-run` via `conflicts_with`. As a subcommand, undo gets its own focused arg struct with only `<JOBLOG>`, `--dry-run`, and `--json`.

Bare `ia ai ID` continues to work (soft migration).

Design doc: `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`
```

### Issue 4: `ia metadata` — Split into read/write subcommands

**Labels:** `enhancement`, `cli`
**Body:**
```
Major restructure of `ia metadata` into subcommands:

**Bare read (soft migration):**
- `ia metadata ID` — show metadata (default when no subcommand)
- `ia metadata ID --exists` / `--formats` / `--json` / `--pretty`

**Export (bulk read):**
- `ia metadata export --search 'query'` — JSONL to stdout
- `ia metadata export -o data.csv` — to spreadsheet file

**Write subcommands** (each with `-m/--metadata`, `--target`, `--expect`, `--priority`, `--reduced-priority`, `--dry-run`, `--json`, batch input):
- `ia metadata modify ID -m field:value`
- `ia metadata append ID -m field:value`
- `ia metadata append-list ID -m field:value`
- `ia metadata insert ID -m 'field[N]:value'`
- `ia metadata remove ID -m field:value`

**Import (bulk write from file):**
- `ia metadata import data.csv`
- Column prefix convention: `append:field`, `append-list:field`, `insert:field[N]`, `remove:field`

Design doc: `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`
```

### Issue 5: Parent tracking issue

**Labels:** `enhancement`, `cli`
**Body:**
```
Parent issue for CLI subcommand restructuring.

Design doc: `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md`

Child issues:
- #N — `ia search` backend subcommands
- #N — FTS `!L` prefix bug fix
- #N — `ia ai undo` subcommand
- #N — `ia metadata` subcommands
```

**Step 1:** Create all five issues via `gh issue create`.
**Step 2:** Update parent issue body with actual issue numbers.
**Step 3:** Note issue numbers for PR linking.

---

## Task 1: Restructure `ia ai` — Extract `undo` subcommand

This is the simplest restructure — a good warmup for the pattern.

**Files:**
- Modify: `ia-cli/src/commands/ai.rs`
- Modify: `ia-cli/src/main.rs` (dispatch unchanged, just args struct changes)
- Test: `ia-cli/tests/cli.rs` (add subcommand tests)

### Step 1: Write failing tests for new `ia ai undo` subcommand

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn ai_undo_subcommand_requires_joblog() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["ai", "undo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("JOBLOG"));
}

#[test]
fn ai_undo_subcommand_shown_in_help() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["ai", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("undo"));
}

#[test]
fn ai_bare_still_works() {
    // Bare `ia ai` with no args should show error about missing input, NOT about subcommands
    Command::cargo_bin("ia")
        .unwrap()
        .args(["ai"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no input specified"));
}
```

**Step 2:** Run tests, verify they fail.

```
cargo test -p ia-cli --test cli ai_undo
```

### Step 3: Restructure `AiArgs` with optional subcommand

In `ia-cli/src/commands/ai.rs`, restructure:

1. Create `AiCommand` enum with single `Undo` variant:

```rust
#[derive(Debug, Subcommand)]
pub enum AiCommand {
    /// Reverse changes recorded in a previous session's joblog
    #[command(
        long_about = "Reverse metadata changes from a previous AI session. Reads the joblog \
            file, finds all successful AI changes, and applies the reverse operations.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Undo all changes from a session</dim>\n  <bold>$ ia ai undo session.jsonl</bold>\
             \n\n  <dim># Preview what would be undone</dim>\n  <bold>$ ia ai undo session.jsonl --dry-run</bold>\n"
        ),
    )]
    Undo(UndoArgs),
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Path to the joblog file containing changes to reverse
    pub joblog: PathBuf,

    /// Preview reversals without applying
    #[arg(long)]
    pub dry_run: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}
```

2. Modify `AiArgs` to have optional subcommand:

```rust
#[derive(Args)]
#[command(subcommand_required = false)]
pub struct AiArgs {
    // ... keep all existing fields EXCEPT remove `undo: Option<PathBuf>` ...

    #[command(subcommand)]
    pub command: Option<AiCommand>,
}
```

3. Remove `undo` field and all `conflicts_with = "undo"` attributes from mode flags.

4. Update `run()` function to check subcommand first:

```rust
pub async fn run(
    client: &IaClient,
    args: AiArgs,
    quiet: u8,
    _jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Handle subcommands
    if let Some(AiCommand::Undo(undo_args)) = args.command {
        let undo_writer = joblog_path
            .as_ref()
            .map(|p| JoblogWriter::open(p))
            .transpose()?;
        let summary = ia_core::ai::undo::undo_from_joblog(
            client,
            &undo_args.joblog,
            undo_writer.as_ref(),
            undo_args.dry_run,
        )
        .await?;

        if quiet == 0 && !undo_args.json {
            // ... existing summary output ...
        }
        if undo_args.json {
            println!("{}", serde_json::to_string(&summary).unwrap_or_default());
        }
        return Ok(());
    }

    // Rest of existing run() unchanged (the analysis pipeline)
    // ...
}
```

**Step 4:** Run tests, verify they pass.

```
cargo test -p ia-cli --test cli ai_undo
cargo test -p ia-cli --test cli
```

**Step 5:** Run full test suite + clippy.

```
cargo test -p ia-core -p ia-cli
cargo clippy -p ia-core -p ia-cli -- -D warnings
```

**Step 6:** Commit.

```
git add ia-cli/src/commands/ai.rs ia-cli/tests/cli.rs
git commit -m "refactor: extract ia ai undo as subcommand

Move --undo flag to a proper 'ia ai undo <JOBLOG>' subcommand.
Undo now has its own focused help with only --dry-run and --json.
Bare 'ia ai ID' continues to work (soft migration).

Closes #N"
```

---

## Task 2: Restructure `ia search` — Backend subcommands

**Files:**
- Modify: `ia-cli/src/commands/search.rs` (major rewrite)
- Modify: `ia-cli/src/main.rs` (dispatch may need minor update)
- Test: `ia-cli/tests/cli.rs`

### Step 1: Write failing tests for new subcommands

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn search_subcommands_shown_in_help() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("scrape"))
        .stdout(predicates::str::contains("advanced"))
        .stdout(predicates::str::contains("fts"));
}

#[test]
fn search_scrape_help_has_sort() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "scrape", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--sort"));
}

#[test]
fn search_fts_help_has_dsl() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--dsl"));
}

#[test]
fn search_fts_help_has_scope() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--scope"));
}

#[test]
fn search_advanced_help_no_dsl() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "advanced", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::is_match("--dsl").unwrap().not());
}

#[test]
fn search_fts_help_no_sort() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["search", "fts", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::is_match("--sort").unwrap().not());
}
```

**Step 2:** Run tests, verify they fail.

```
cargo test -p ia-cli --test cli search_
```

### Step 3: Create shared args structs and subcommand enum

Rewrite `ia-cli/src/commands/search.rs`:

```rust
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::StreamExt;

use ia_core::search::{SearchOpts, SearchResult};
use ia_core::IaClient;

/// Shared output/control options available on all search subcommands.
#[derive(Debug, Args)]
pub struct SharedSearchArgs {
    /// Output identifiers only (one per line)
    #[arg(long)]
    pub itemlist: bool,

    /// Print result count only
    #[arg(short = 'n', long)]
    pub num_found: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// Extra parameters (key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum SearchCommand {
    /// Search via scrape API (cursor-based, auto-paginates)
    #[command(
        long_about = "Search the Internet Archive using the scrape API. Uses cursor-based \
            pagination and automatically streams all matching results. This is the default \
            backend when no subcommand is specified.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Search a collection</dim>\n  <bold>$ ia search scrape \"collection:nasa\"</bold>\
             \n\n  <dim># Sort by downloads</dim>\n  <bold>$ ia search scrape \"mediatype:audio\" --sort \"downloads desc\"</bold>\
             \n\n  <dim># Get identifiers only</dim>\n  <bold>$ ia search scrape \"date:1969\" --itemlist</bold>\n"
        ),
    )]
    Scrape(ScrapeArgs),

    /// Search via advanced search API (page-based, single page)
    #[command(
        long_about = "Search the Internet Archive using the advanced search API. Returns a \
            single page of results. Use -p rows=N to control page size and -p page=N to \
            select a page.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Single page of results</dim>\n  <bold>$ ia search advanced \"collection:nasa\"</bold>\
             \n\n  <dim># With sorting and field selection</dim>\n  <bold>$ ia search advanced \"mediatype:texts\" --sort \"date desc\" --field title --field date</bold>\n"
        ),
    )]
    Advanced(AdvancedArgs),

    /// Full-text search (scroll-based, auto-paginates)
    #[command(
        long_about = "Search the Internet Archive using the full-text search backend. Searches \
            inside file contents, not just item metadata. Uses scroll-based pagination.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Full-text search</dim>\n  <bold>$ ia search fts \"apollo 11 landing\"</bold>\
             \n\n  <dim># With Elasticsearch DSL</dim>\n  <bold>$ ia search fts --dsl '{\"match\": {\"text\": \"moon landing\"}}'</bold>\
             \n\n  <dim># Scoped to an index</dim>\n  <bold>$ ia search fts \"nasa\" --scope nasa_index</bold>\n"
        ),
    )]
    Fts(FtsArgs),
}

#[derive(Debug, Args)]
pub struct ScrapeArgs {
    /// Search query
    pub query: String,

    /// Sort fields (repeatable, e.g., "downloads desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Debug, Args)]
pub struct AdvancedArgs {
    /// Search query
    pub query: String,

    /// Sort fields (repeatable, e.g., "date desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Debug, Args)]
pub struct FtsArgs {
    /// Search query
    pub query: String,

    /// Use raw Elasticsearch DSL (skip !L prefix)
    #[arg(long)]
    pub dsl: bool,

    /// Index/scope filter
    #[arg(long)]
    pub scope: Option<String>,

    /// Results per scroll batch (default: 1000)
    #[arg(long)]
    pub size: Option<usize>,

    /// Starting offset
    #[arg(long)]
    pub from: Option<usize>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}
```

Then update `SearchArgs` to support optional subcommand with fallback:

```rust
#[derive(Args)]
#[command(
    long_about = "Search the Internet Archive. Uses the scrape API by default. \
        Choose a subcommand for a different backend.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Search (uses scrape by default)</dim>\n  <bold>$ ia search \"collection:nasa\"</bold>\
         \n\n  <dim># Explicit backend</dim>\n  <bold>$ ia search scrape \"collection:nasa\"</bold>\
         \n\n  <dim># Advanced search (single page)</dim>\n  <bold>$ ia search advanced \"mediatype:texts\"</bold>\
         \n\n  <dim># Full-text search</dim>\n  <bold>$ ia search fts \"apollo 11\"</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct SearchArgs {
    /// Search query (uses scrape API by default)
    #[arg()]
    pub query: Option<String>,

    // Bare-mode options (when no subcommand specified, defaults to scrape)
    /// Sort fields (repeatable, e.g., "downloads desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    /// Output identifiers only (one per line)
    #[arg(long)]
    pub itemlist: bool,

    /// Print result count only
    #[arg(short = 'n', long)]
    pub num_found: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// Extra parameters (key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Option<SearchCommand>,
}
```

### Step 4: Implement the `run()` function with dispatch

```rust
pub async fn run(client: &IaClient, args: SearchArgs, quiet: u8) -> Result<()> {
    match args.command {
        Some(SearchCommand::Scrape(sub)) => {
            run_scrape(client, sub.query, sub.sort, sub.field, sub.shared, quiet).await
        }
        Some(SearchCommand::Advanced(sub)) => {
            run_advanced(client, sub.query, sub.sort, sub.field, sub.shared, quiet).await
        }
        Some(SearchCommand::Fts(sub)) => {
            run_fts(client, sub, quiet).await
        }
        None => {
            // Bare mode: default to scrape
            let query = args.query.ok_or_else(|| {
                anyhow::anyhow!("search query required. Run 'ia search --help' for usage.")
            })?;
            let shared = SharedSearchArgs {
                itemlist: args.itemlist,
                num_found: args.num_found,
                json: args.json,
                parameters: args.parameters,
                timeout: args.timeout,
            };
            run_scrape(client, query, args.sort, args.field, shared, quiet).await
        }
    }
}
```

Then implement `run_scrape()`, `run_advanced()`, `run_fts()` as separate functions that each call the appropriate `ia_core::search::*` backend.

`run_scrape()` — calls `ia_core::search::scrape()` or `ia_core::search::num_found()`, streams results.
`run_advanced()` — calls `ia_core::search::advanced()`, returns **single page only** (modify the ia-core `advanced()` function or add a `single_page` parameter to `SearchOpts`).
`run_fts()` — calls `ia_core::search::fts()`, passes `--dsl`, `--scope`, `--size`, `--from` to the backend.

**Note on advanced search single-page:** The current `ia_core::search::advanced()` auto-paginates. Either:
- Add a `single_page: bool` field to `SearchOpts` and check it in the loop, OR
- Create a separate `ia_core::search::advanced_page()` function that returns one page.

The former is simpler.

### Step 5: Update `run_output()` helper

Extract the shared output logic (JSON, itemlist, pretty, summary) into a helper function so all three `run_*` functions can reuse it:

```rust
async fn run_output(
    mut stream: Pin<Box<dyn futures::Stream<Item = ia_core::Result<SearchResult>> + Send + '_>>,
    shared: &SharedSearchArgs,
    fields_requested: &[String],
    quiet: u8,
) -> Result<()> {
    // ... shared output loop from current run() ...
}
```

### Step 6: Run tests, verify they pass.

```
cargo test -p ia-cli --test cli search_
cargo test -p ia-core -p ia-cli
cargo clippy -p ia-core -p ia-cli -- -D warnings
```

### Step 7: Commit.

```
git add ia-cli/src/commands/search.rs ia-cli/tests/cli.rs
git commit -m "refactor: split ia search into backend subcommands

Add scrape, advanced, fts as subcommands of ia search. Each backend
documents only its supported options. Bare 'ia search <query>' defaults
to scrape for soft migration.

- scrape: cursor-based, auto-paginates, supports --sort and --field
- advanced: page-based, single page only, supports --sort and --field
- fts: scroll-based, adds --dsl, --scope, --size, --from

Removed --fts flag (replaced by fts subcommand) and -n/--count
(use head or -p params). Renamed --num-found to -n/--num-found.

Closes #N"
```

---

## Task 3: Fix FTS `!L` prefix bug

**Files:**
- Modify: `ia-core/src/search.rs` (`fts()` function)
- Test: `ia-core/src/search.rs` (add tests in existing test module)

### Step 1: Write failing test

Add to the test module in `ia-core/src/search.rs`:

```rust
#[tokio::test]
async fn fts_prepends_literal_prefix() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ia-pub-fts-api"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "query": "!L apollo 11",
            "size": 1000,
            "scroll": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": { "total": 0, "hits": [] }
        })))
        .mount(&mock_server)
        .await;

    let mut config = mock_config(&mock_server.uri());
    config.general.secure = false;
    let client = IaClient::from_config(config).unwrap();
    let opts = SearchOpts::default();
    let results: Vec<_> = fts(&client, "apollo 11", &opts).collect().await;
    assert!(results.is_empty());
}

#[tokio::test]
async fn fts_dsl_mode_skips_prefix() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ia-pub-fts-api"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "query": "{\"match\": {\"text\": \"moon\"}}",
            "size": 1000,
            "scroll": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": { "total": 0, "hits": [] }
        })))
        .mount(&mock_server)
        .await;

    let mut config = mock_config(&mock_server.uri());
    config.general.secure = false;
    let client = IaClient::from_config(config).unwrap();
    let mut opts = SearchOpts::default();
    opts.dsl = true;
    let results: Vec<_> = fts(&client, r#"{"match": {"text": "moon"}}"#, &opts).collect().await;
    assert!(results.is_empty());
}
```

**Step 2:** Run test, verify it fails (the `dsl` field doesn't exist on `SearchOpts` yet).

```
cargo test -p ia-core fts_prepends
```

### Step 3: Add `dsl` field to `SearchOpts` and fix `fts()`

In `ia-core/src/search.rs`:

Add to `SearchOpts`:
```rust
pub struct SearchOpts {
    pub fields: Vec<String>,
    pub sorts: Vec<String>,
    pub count: usize,
    pub timeout: Option<u64>,
    pub params: Vec<(String, String)>,
    /// FTS: skip !L prefix for raw Elasticsearch DSL queries
    pub dsl: bool,
}
```

In the `fts()` function, modify the query construction:

```rust
let query = if opts.dsl {
    query.to_string()
} else {
    format!("!L {}", query)
};
```

### Step 4: Run tests, verify they pass.

```
cargo test -p ia-core fts_
cargo test -p ia-core -p ia-cli
cargo clippy -p ia-core -p ia-cli -- -D warnings
```

### Step 5: Commit.

```
git add ia-core/src/search.rs
git commit -m "fix: add !L prefix to FTS queries for literal text search

The Python internetarchive library prepends '!L' to non-DSL FTS queries.
The Rust port was sending raw queries, producing different search results.

Adds dsl field to SearchOpts. When false (default), queries are prefixed
with '!L ' for literal text matching. When true (--dsl flag), the prefix
is skipped for raw Elasticsearch DSL queries.

Closes #N"
```

---

## Task 4: Restructure `ia metadata` — Subcommand enum and shared args

This is the largest task. Split into sub-steps.

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (major rewrite)
- Modify: `ia-cli/src/main.rs` (dispatch may need minor update)
- Test: `ia-cli/tests/cli.rs`
- (ia-core unchanged — all existing functions work as-is)

### Step 1: Write failing tests for new subcommands

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_subcommands_shown_in_help() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("export"))
        .stdout(predicates::str::contains("modify"))
        .stdout(predicates::str::contains("append"))
        .stdout(predicates::str::contains("append-list"))
        .stdout(predicates::str::contains("insert"))
        .stdout(predicates::str::contains("remove"))
        .stdout(predicates::str::contains("import"));
}

#[test]
fn metadata_modify_help_has_metadata_flag() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--metadata"));
}

#[test]
fn metadata_modify_help_has_target() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--target"));
}

#[test]
fn metadata_export_help_has_output() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--output"));
}

#[test]
fn metadata_import_help_has_dry_run() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "import", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--dry-run"));
}

#[test]
fn metadata_modify_help_no_exists_flag() {
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata", "modify", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::is_match("--exists").unwrap().not());
}

#[test]
fn metadata_bare_read_requires_identifier() {
    // Bare `ia metadata` with no args or subcommand should error
    Command::cargo_bin("ia")
        .unwrap()
        .args(["metadata"])
        .assert()
        .failure();
}
```

**Step 2:** Run tests, verify they fail.

```
cargo test -p ia-cli --test cli metadata_
```

### Step 3: Define shared structs

Create shared arg structs at the top of `ia-cli/src/commands/metadata.rs`:

```rust
/// Shared write options for all metadata write subcommands.
#[derive(Debug, Args)]
pub struct WriteOpts {
    /// Field:value pairs to apply (repeatable)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Optimistic concurrency check (repeatable, field:expected_value)
    #[arg(long)]
    pub expect: Vec<String>,

    /// Task priority (default: 0 single, -5 batch)
    #[arg(long)]
    pub priority: Option<i32>,

    /// Accept reduced priority to reduce rate limiting
    #[arg(long)]
    pub reduced_priority: bool,

    /// Show changes without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

/// Shared batch input sources.
#[derive(Debug, Args)]
pub struct BatchInput {
    /// Item identifier(s)
    #[arg()]
    pub identifiers: Vec<String>,

    /// Read identifiers from file (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long)]
    pub search: Option<String>,
}
```

### Step 4: Define the MetadataCommand enum

```rust
#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    /// Bulk export metadata to stdout or file
    #[command(
        long_about = "Export metadata for multiple items. Outputs JSONL to stdout by default, \
            or writes to a file in CSV, TSV, XLSX, or JSONL format (inferred from extension).",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Export search results as JSONL</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
             \n\n  <dim># Export to CSV file</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.csv</bold>\n"
        ),
    )]
    Export(ExportArgs),

    /// Set or replace metadata field values
    #[command(
        long_about = "Set metadata fields to new values. Replaces existing values. \
            Use -m/--metadata to specify field:value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Set title</dim>\n  <bold>$ ia metadata modify myitem -m \"title:New Title\"</bold>\
             \n\n  <dim># Set multiple fields</dim>\n  <bold>$ ia metadata modify myitem -m \"title:X\" -m \"date:2024\"</bold>\
             \n\n  <dim># Batch modify via search</dim>\n  <bold>$ ia metadata modify --search \"collection:test\" -m \"subject:updated\"</bold>\n"
        ),
    )]
    Modify(ModifyArgs),

    /// Append text to string metadata fields
    #[command(
        long_about = "Append text to the end of string metadata fields. \
            Use -m/--metadata to specify field:value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Append to description</dim>\n  <bold>$ ia metadata append myitem -m \"description:Additional info.\"</bold>\n"
        ),
    )]
    Append(AppendArgs),

    /// Append values to list metadata fields
    #[command(
        name = "append-list",
        long_about = "Append values to list-type metadata fields (e.g., subject, collection). \
            Use -m/--metadata to specify field:value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Add a subject tag</dim>\n  <bold>$ ia metadata append-list myitem -m \"subject:new-tag\"</bold>\n"
        ),
    )]
    AppendList(AppendListArgs),

    /// Insert values at a position in list metadata fields
    #[command(
        long_about = "Insert values at a specific index in list-type metadata fields. \
            Use -m/--metadata with field[index]:value syntax.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Insert at beginning of subject list</dim>\n  <bold>$ ia metadata insert myitem -m \"subject[0]:first-tag\"</bold>\n"
        ),
    )]
    Insert(InsertArgs),

    /// Remove values from metadata fields
    #[command(
        long_about = "Remove values from metadata fields. For list fields, removes the matching \
            value. For string fields, removes the field entirely. \
            Use -m/--metadata to specify field:value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Remove a subject tag</dim>\n  <bold>$ ia metadata remove myitem -m \"subject:old-tag\"</bold>\
             \n\n  <dim># Delete a field entirely</dim>\n  <bold>$ ia metadata remove myitem -m \"description:REMOVE_TAG\"</bold>\n"
        ),
    )]
    Remove(RemoveArgs),

    /// Bulk write metadata from a spreadsheet or data file
    #[command(
        long_about = "Import metadata changes from a CSV, TSV, XLSX, ODS, or JSONL file. \
            The file must have an 'identifier' column. By default, all columns are treated as \
            modify operations. Use column prefixes for other operations: append:field, \
            append-list:field, insert:field[N], remove:field.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Import from CSV</dim>\n  <bold>$ ia metadata import data.csv</bold>\
             \n\n  <dim># Preview changes</dim>\n  <bold>$ ia metadata import data.xlsx --dry-run</bold>\n"
        ),
    )]
    Import(ImportArgs),
}
```

### Step 5: Define per-subcommand arg structs

```rust
#[derive(Debug, Args)]
pub struct ExportArgs {
    #[command(flatten)]
    pub input: BatchInput,

    /// Output file (format inferred from extension: .csv, .tsv, .xlsx, .jsonl)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Output as JSONL (default when no -o)
    #[arg(long)]
    pub json: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub struct ModifyArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

#[derive(Debug, Args)]
pub struct AppendArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

#[derive(Debug, Args)]
pub struct AppendListArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

#[derive(Debug, Args)]
pub struct InsertArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to spreadsheet or data file (CSV, TSV, XLSX, ODS, JSONL)
    pub file: PathBuf,

    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Optimistic concurrency check (repeatable, field:expected_value)
    #[arg(long)]
    pub expect: Vec<String>,

    /// Task priority (default: -5 for batch)
    #[arg(long)]
    pub priority: Option<i32>,

    /// Accept reduced priority to reduce rate limiting
    #[arg(long)]
    pub reduced_priority: bool,

    /// Show changes without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}
```

### Step 6: Update MetadataArgs with optional subcommand

```rust
#[derive(Args)]
#[command(
    long_about = "Read or modify Internet Archive item metadata. Shows metadata as JSON \
        by default. Use subcommands for write operations, bulk export, or bulk import.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Show item metadata</dim>\n  <bold>$ ia metadata nasa</bold>\
         \n\n  <dim># Check if item exists</dim>\n  <bold>$ ia metadata nasa --exists</bold>\
         \n\n  <dim># Modify metadata</dim>\n  <bold>$ ia metadata modify nasa -m \"title:New\"</bold>\
         \n\n  <dim># Bulk export</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
         \n\n  <dim># Bulk import</dim>\n  <bold>$ ia metadata import data.csv</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct MetadataArgs {
    /// Item identifier
    #[arg()]
    pub identifier: Option<String>,

    /// Check if item exists (exit code 0/1)
    #[arg(short = 'e', long)]
    pub exists: bool,

    /// List available file formats
    #[arg(short = 'F', long)]
    pub formats: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<MetadataCommand>,
}
```

### Step 7: Implement `run()` dispatch

```rust
pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    match args.command {
        Some(MetadataCommand::Export(sub)) => run_export(client, sub, quiet).await,
        Some(MetadataCommand::Modify(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Set, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::Append(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Append, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::AppendList(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::AppendList, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::Insert(sub)) => {
            // Parse index from -m values (field[N]:value syntax)
            run_write_insert(client, sub.input, sub.write, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::Remove(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Remove, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::Import(sub)) => {
            run_import(client, sub, quiet, jobs, joblog_path).await
        }
        None => {
            // Bare read mode
            let identifier = args.identifier.ok_or_else(|| {
                anyhow::anyhow!("identifier required. Run 'ia metadata --help' for usage.")
            })?;
            run_read(client, &identifier, args.exists, args.formats, args.pretty, args.json, quiet).await
        }
    }
}
```

### Step 8: Implement inner functions

Refactor the existing `run_read()`, `run_write()`, `run_spreadsheet()`, `run_dry_run()`, `record_modify_outcome()`, and `collect_identifiers()` functions to work with the new arg structs. The core logic is the same — just wire the new structs to the existing functions:

- `run_read()` — existing logic, receives individual args instead of the old `MetadataArgs`
- `run_write()` — existing `run_write()` logic, takes `BatchInput`, `WriteOpts`, and `MetadataOp`
- `run_write_insert()` — like `run_write()` but parses `field[N]:value` from metadata values
- `run_export()` — new function for bulk read. Currently this is the "bulk read" path in the old `run_read()`. Extract and extend with `-o` file output.
- `run_import()` — existing `run_spreadsheet()` logic, but reads column prefixes to determine per-column `MetadataOp`

### Step 9: Implement column prefix parsing for `import`

In `run_import()`, parse column prefixes from the spreadsheet:

```rust
fn parse_column_op(column_name: &str) -> (MetadataOp, String) {
    if let Some(field) = column_name.strip_prefix("append-list:") {
        (MetadataOp::AppendList, field.to_string())
    } else if let Some(field) = column_name.strip_prefix("append:") {
        (MetadataOp::Append, field.to_string())
    } else if let Some(field) = column_name.strip_prefix("remove:") {
        (MetadataOp::Remove, field.to_string())
    } else if let Some(rest) = column_name.strip_prefix("insert:") {
        // Parse "insert:field[N]" → (Insert(N), field)
        if let Some((field, idx)) = ia_core::metadata::parse_indexed_key(rest) {
            (MetadataOp::Insert(idx), field)
        } else {
            (MetadataOp::Set, rest.to_string())
        }
    } else {
        // Default: modify (set)
        (MetadataOp::Set, column_name.to_string())
    }
}
```

### Step 10: Run tests, verify they pass.

```
cargo test -p ia-cli --test cli metadata_
cargo test -p ia-core -p ia-cli
cargo clippy -p ia-core -p ia-cli -- -D warnings
```

### Step 11: Commit.

```
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "refactor: restructure ia metadata into subcommands

Split ia metadata into focused subcommands:
- export: bulk read to stdout/file (JSONL, CSV, TSV, XLSX)
- modify: set/replace field values
- append: append to string fields
- append-list: append to list fields
- insert: insert at index in list fields
- remove: remove values from fields
- import: bulk write from spreadsheet with column prefix convention

Bare 'ia metadata ID' still works for reading (soft migration).
All write subcommands share -m/--metadata, --target, --expect,
--priority, --dry-run, --json via flattened WriteOpts struct.

Import uses column prefixes for mixed operations:
append:field, append-list:field, insert:field[N], remove:field.
Unprefix columns default to modify (set).

Closes #N"
```

---

## Task 5: Update help text and documentation

**Files:**
- Modify: `ia-cli/src/main.rs` (top-level help if needed)
- Modify: `docs/usage.md` (if it exists)
- Modify: MEMORY.md
- Verify: all `long_about` and `after_long_help` text is accurate

### Step 1: Review all help text

Run `--help` for every modified command and subcommand:

```
ia --help
ia search --help
ia search scrape --help
ia search advanced --help
ia search fts --help
ia metadata --help
ia metadata export --help
ia metadata modify --help
ia metadata append --help
ia metadata append-list --help
ia metadata insert --help
ia metadata remove --help
ia metadata import --help
ia ai --help
ia ai undo --help
```

Verify each is accurate, clear, and follows the layered help convention (terse `-h`, detailed `--help` with examples).

### Step 2: Update MEMORY.md

Update the Key CLI Files section to reflect new subcommand structures.

### Step 3: Commit.

```
git add -A
git commit -m "docs: update help text and MEMORY.md for subcommand restructuring"
```

---

## Task 6: Final verification and PR

### Step 1: Full test suite.

```
cargo test -p ia-core -p ia-cli
cargo clippy -p ia-core -p ia-cli -- -D warnings
```

### Step 2: Verify no regressions.

Run a representative sample of commands manually:
```
ia search "test" --help
ia metadata --help
ia ai --help
```

### Step 3: Create PR.

```
gh pr create --title "refactor: CLI subcommand restructuring" --body "$(cat <<'EOF'
## Summary
- Restructure `ia metadata` into 7 subcommands (export, modify, append, append-list, insert, remove, import) + bare read
- Restructure `ia search` into 3 backend subcommands (scrape, advanced, fts) + bare default
- Extract `ia ai undo` as subcommand
- Fix FTS `!L` prefix bug for literal text search
- Follows sub-subcommand pattern established by `ia config`

Bare forms preserved for soft migration:
- `ia metadata ID` → read
- `ia search 'query'` → scrape
- `ia ai ID` → analysis pipeline

Closes #N
Closes #N
Closes #N
Closes #N
Closes #N

## Test plan
- [ ] `cargo test -p ia-core -p ia-cli` — all tests pass
- [ ] `cargo clippy -p ia-core -p ia-cli -- -D warnings` — zero warnings
- [ ] `ia search --help` shows scrape/advanced/fts subcommands
- [ ] `ia metadata --help` shows all 7 subcommands
- [ ] `ia ai --help` shows undo subcommand
- [ ] `ia search 'test'` still works (soft migration)
- [ ] `ia metadata <id>` still works (soft migration)
- [ ] `ia search fts --help` shows --dsl, --scope, --size, --from
- [ ] `ia metadata modify --help` shows -m/--metadata but NOT --exists

EOF
)"
```
