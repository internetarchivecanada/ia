# CLI Help Text Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve CLI help text with colored output, layered -h/--help, descriptive long_about text, real-world examples, and clearer option descriptions for a broad audience (developers, librarians, archivists, students).

**Architecture:** Pure clap attributes approach. A shared `Styles` constant colors structural elements. `long_about` and `after_long_help` (via `color-print` crate's `cstr!` macro) provide the extended help tier. All changes are to existing files — no new modules.

**Tech Stack:** clap 4 (Styles API, derive attributes), color-print (compile-time ANSI codes)

**Design doc:** `docs/plans/2026-02-23-cli-help-design.md`

---

### Task 1: Add `color-print` dependency

**Files:**
- Modify: `ia-cli/Cargo.toml`

**Step 1: Add the dependency**

In `ia-cli/Cargo.toml`, add under `[dependencies]`:

```toml
color-print = "0.3"
```

**Step 2: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add ia-cli/Cargo.toml Cargo.lock
git commit -m "chore: add color-print dependency for colored help text"
```

---

### Task 2: Define shared clap Styles and apply to root command

**Files:**
- Modify: `ia-cli/src/main.rs`

**Step 1: Write a test for colored help output**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn help_output_contains_examples_section() {
    ia().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"));
}

#[test]
fn short_help_omits_examples() {
    ia().arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:").not());
}
```

Run: `cargo test -p ia-cli --test cli help_output_contains_examples_section short_help_omits_examples`
Expected: FAIL (no examples yet)

**Step 2: Define the Styles constant and update the root command**

In `ia-cli/src/main.rs`, add at the top (after imports):

```rust
use color_print::cstr;

const STYLES: clap::builder::Styles = clap::builder::Styles::styled()
    .header(
        clap::builder::styling::AnsiColor::Green
            .on_default()
            .bold(),
    )
    .usage(
        clap::builder::styling::AnsiColor::Green
            .on_default()
            .bold(),
    )
    .literal(
        clap::builder::styling::AnsiColor::Cyan
            .on_default()
            .bold(),
    )
    .placeholder(clap::builder::styling::AnsiColor::Cyan.on_default());
```

Update the `#[command(...)]` on `Cli` to:

```rust
#[derive(Parser)]
#[command(
    name = "ia",
    version,
    about = "Internet Archive command-line tool",
    long_about = "A command-line tool for interacting with the Internet Archive (archive.org).\n\
        Download files, search for items, view and edit metadata, and list file contents.",
    styles = STYLES,
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Download all files from an item</dim>\n  <bold>$ ia download nasa</bold>\
         \n\n  <dim># Search for items in a collection</dim>\n  <bold>$ ia search \"collection:nasa\"</bold>\
         \n\n  <dim># View metadata for an item</dim>\n  <bold>$ ia metadata nasa</bold>\n"
    ),
)]
struct Cli {
```

**Step 3: Update subcommand one-liners in the Commands enum**

```rust
#[derive(Subcommand)]
enum Commands {
    /// Download files from one or more items
    Download(commands::download::DownloadArgs),
    /// List files in an item with filtering and formatting
    #[command(alias = "ls")]
    List(commands::list::ListArgs),
    /// Read or modify item metadata
    Metadata(commands::metadata::MetadataArgs),
    /// Search the Internet Archive
    Search(commands::search::SearchArgs),
    /// Show job log summary and failed operations
    Status(commands::status::StatusArgs),
    /// Generate shell completions for bash, zsh, fish, etc.
    Completions(commands::completions::CompletionsArgs),
}
```

**Step 4: Run the tests**

Run: `cargo test -p ia-cli --test cli`
Expected: all tests pass, including the two new ones

**Step 5: Visually verify**

Run: `cargo run -p ia-cli -- --help`
Check: green headers, cyan flags, examples section at bottom

Run: `cargo run -p ia-cli -- -h`
Check: short output, no examples section

**Step 6: Commit**

```bash
git add ia-cli/src/main.rs ia-cli/tests/cli.rs
git commit -m "feat: add colored styles and long help to main command"
```

---

### Task 3: Add long_about and examples to download subcommand

**Files:**
- Modify: `ia-cli/src/commands/download.rs`
- Modify: `ia-cli/tests/cli.rs`

**Step 1: Write a test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn download_long_help_has_examples() {
    ia().args(["download", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia download"));
}

#[test]
fn download_short_help_omits_examples() {
    ia().args(["download", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:").not());
}
```

Run: `cargo test -p ia-cli --test cli download_long_help_has_examples download_short_help_omits_examples`
Expected: FAIL

**Step 2: Add long_about and after_long_help to DownloadArgs**

In `ia-cli/src/commands/download.rs`, add `use color_print::cstr;` at the top, then update the struct:

```rust
#[derive(Args)]
#[command(
    long_about = "Download files from the Internet Archive. Downloads all files from one or more \
        items, with options to filter by format, glob pattern, or source type. Supports batch \
        downloads via search queries or item lists.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Download all files from an item</dim>\n  <bold>$ ia download nasa</bold>\
         \n\n  <dim># Download only MP4 files</dim>\n  <bold>$ ia download nasa --glob \"*.mp4\"</bold>\
         \n\n  <dim># Batch download items matching a search query</dim>\n  <bold>$ ia download --search \"collection:nasa AND mediatype:movies\"</bold>\n"
    ),
)]
pub struct DownloadArgs {
```

**Step 3: Improve terse option help strings**

Update these doc comments in `DownloadArgs`:

```rust
    /// Filter by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    source: Option<FileSource>,

    /// Exclude by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    exclude_source: Option<FileSource>,

    /// Download items matching a search query (downloads each result)
    #[arg(short = 's', long)]
    search: Option<String>,

    /// Concurrent items for batch/search (use -j/--jobs for concurrent files)
    #[arg(long, default_value = "2")]
    pub items: usize,
```

**Step 4: Run tests**

Run: `cargo test -p ia-cli --test cli`
Expected: all pass

**Step 5: Commit**

```bash
git add ia-cli/src/commands/download.rs ia-cli/tests/cli.rs
git commit -m "feat: add long help and examples to download command"
```

---

### Task 4: Add long_about and examples to search subcommand

**Files:**
- Modify: `ia-cli/src/commands/search.rs`
- Modify: `ia-cli/tests/cli.rs`

**Step 1: Write a test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn search_long_help_has_examples() {
    ia().args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia search"));
}
```

Run: `cargo test -p ia-cli --test cli search_long_help_has_examples`
Expected: FAIL

**Step 2: Add long_about and after_long_help to SearchArgs**

In `ia-cli/src/commands/search.rs`, add `use color_print::cstr;` at the top, then update:

```rust
#[derive(Args)]
#[command(
    long_about = "Search the Internet Archive. Returns matching items using the scrape API by \
        default, or the full-text search backend with --fts. Results can be formatted as JSON, \
        filtered to specific fields, or output as a plain identifier list.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Search for items in a collection</dim>\n  <bold>$ ia search \"collection:nasa\"</bold>\
         \n\n  <dim># Get just the identifiers (useful for piping)</dim>\n  <bold>$ ia search \"mediatype:audio\" --itemlist</bold>\
         \n\n  <dim># Full-text search with JSON output</dim>\n  <bold>$ ia search \"apollo 11\" --fts --json</bold>\n"
    ),
)]
pub struct SearchArgs {
```

**Step 3: Run tests**

Run: `cargo test -p ia-cli --test cli`
Expected: all pass

**Step 4: Commit**

```bash
git add ia-cli/src/commands/search.rs ia-cli/tests/cli.rs
git commit -m "feat: add long help and examples to search command"
```

---

### Task 5: Add long_about and examples to metadata subcommand

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/tests/cli.rs`

**Step 1: Write a test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_long_help_has_examples() {
    ia().args(["metadata", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia metadata"));
}
```

Run: `cargo test -p ia-cli --test cli metadata_long_help_has_examples`
Expected: FAIL

**Step 2: Add long_about and after_long_help to MetadataArgs**

In `ia-cli/src/commands/metadata.rs`, add `use color_print::cstr;` at the top, then update:

```rust
#[derive(Args)]
#[command(
    long_about = "Read or modify item metadata. By default, displays the full metadata JSON for \
        an item. Use --modify, --append, --remove, and related flags to update metadata fields. \
        Supports bulk operations via --itemlist, --search, or --spreadsheet.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># View metadata for an item</dim>\n  <bold>$ ia metadata nasa</bold>\
         \n\n  <dim># Set a metadata field</dim>\n  <bold>$ ia metadata nasa --modify=\"description:Updated description\"</bold>\
         \n\n  <dim># Bulk update from a spreadsheet</dim>\n  <bold>$ ia metadata --spreadsheet updates.csv</bold>\n"
    ),
)]
pub struct MetadataArgs {
```

**Step 3: Improve terse option help strings**

Update these doc comments in `MetadataArgs`:

```rust
    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Optimistic concurrency check: fail if field doesn't match expected value
    #[arg(long, value_name = "K:V")]
    pub expect: Vec<String>,

    /// Use search results as input (queries archive.org, modifies each result)
    #[arg(long)]
    pub search: Option<String>,
```

**Step 4: Run tests**

Run: `cargo test -p ia-cli --test cli`
Expected: all pass

**Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: add long help and examples to metadata command"
```

---

### Task 6: Add long_about and examples to list, status, and completions

**Files:**
- Modify: `ia-cli/src/commands/list.rs`
- Modify: `ia-cli/src/commands/status.rs`
- Modify: `ia-cli/src/commands/completions.rs`
- Modify: `ia-cli/tests/cli.rs`

**Step 1: Write tests**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn list_long_help_has_examples() {
    ia().args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia list"));
}

#[test]
fn status_long_help_has_examples() {
    ia().args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia status"));
}

#[test]
fn completions_long_help_has_examples() {
    ia().args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("ia completions"));
}
```

Run: `cargo test -p ia-cli --test cli list_long_help_has_examples status_long_help_has_examples completions_long_help_has_examples`
Expected: FAIL

**Step 2: Update list.rs**

Add `use color_print::cstr;` at the top, then:

```rust
#[derive(Args)]
#[command(
    long_about = "List files in an Internet Archive item. Displays a table of files with name, \
        size, and format by default. Use --columns to customize output, --glob to filter, or \
        --all for full file metadata as JSON.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># List files in an item</dim>\n  <bold>$ ia list nasa</bold>\
         \n\n  <dim># Show only original files with download URLs</dim>\n  <bold>$ ia list nasa --source original --location</bold>\n"
    ),
)]
pub struct ListArgs {
```

Also improve the `--source` help:

```rust
    /// Filter by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    pub source: Option<FileSource>,
```

**Step 3: Update status.rs**

Add `use color_print::cstr;` at the top, then:

```rust
#[derive(Args)]
#[command(
    long_about = "Show a summary of a job log file. Displays total operations, success/failure/skip \
        counts, and lists any failed files with error messages.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># View job log summary</dim>\n  <bold>$ ia status --joblog downloads.jsonl</bold>\n"
    ),
)]
pub struct StatusArgs {
```

**Step 4: Update completions.rs**

Add `use color_print::cstr;` at the top, then:

```rust
#[derive(Args)]
#[command(
    long_about = "Generate shell completion scripts. Prints a completion script to stdout \u2014 \
        redirect it to the appropriate file for your shell.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Generate completions for fish</dim>\n  <bold>$ ia completions fish > ~/.config/fish/completions/ia.fish</bold>\
         \n\n  <dim># Generate completions for bash</dim>\n  <bold>$ ia completions bash > ~/.local/share/bash-completion/completions/ia</bold>\n"
    ),
)]
pub struct CompletionsArgs {
```

**Step 5: Run tests**

Run: `cargo test -p ia-cli --test cli`
Expected: all pass

**Step 6: Commit**

```bash
git add ia-cli/src/commands/list.rs ia-cli/src/commands/status.rs ia-cli/src/commands/completions.rs ia-cli/tests/cli.rs
git commit -m "feat: add long help and examples to list, status, and completions"
```

---

### Task 7: Update CLAUDE.md with help text maintenance rule

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Add the convention**

Under the `## Development Workflow` section in `CLAUDE.md`, add:

```markdown
- **ALWAYS update help text**: When adding or modifying CLI flags, subcommands, or behaviors, update the corresponding `about`, `long_about`, `after_long_help`, and option-level help strings. Help text is user-facing documentation — it must stay accurate.
```

**Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add help text maintenance rule to CLAUDE.md"
```

---

### Task 8: Final verification and visual review

**Step 1: Run the full test suite**

Run: `cargo test --workspace`
Expected: all tests pass (existing + new)

Run: `cargo clippy --workspace`
Expected: no warnings

**Step 2: Visual spot check of every command**

Run each and visually confirm colored output, examples, and layering:

```bash
cargo run -p ia-cli -- --help
cargo run -p ia-cli -- -h
cargo run -p ia-cli -- download --help
cargo run -p ia-cli -- download -h
cargo run -p ia-cli -- search --help
cargo run -p ia-cli -- metadata --help
cargo run -p ia-cli -- list --help
cargo run -p ia-cli -- status --help
cargo run -p ia-cli -- completions --help
```

**Step 3: Verify piped output strips ANSI codes**

Run: `cargo run -p ia-cli -- --help | cat`
Expected: no visible ANSI escape sequences (clap auto-strips)

**Step 4: Update MEMORY.md**

Add a note about the help text conventions and `color-print` usage.

**Step 5: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final cleanup after help text redesign"
```
