# Metadata Export Redesign — Implementation Plan

**Goal:** Make `ia md export` accept files (CSV, TSV, XLSX, ODS, JSONL, plain text) as positional args, and extend bare `ia md` to accept multiple identifiers with concurrent JSONL output.

**Architecture:** Split identifier collection into two paths — `collect_identifiers_from_export` (files + search + stdin) for `export`, and the existing `collect_identifiers_from_batch` (IDs + --itemlist + --search + stdin) for write commands. Add a `read_identifiers_from_file` helper in `ia-core/src/spreadsheet.rs` that extracts just the `identifier` column. Extend bare `ia md id1 id2 ...` to support multiple IDs with `-j` concurrency.

**Tech Stack:** Rust, clap, tokio, reqwest, serde_json

---

## Chunk 1: Core identifier extraction + export CLI changes

### Task 1: Add `read_identifiers_from_file` to `ia-core/src/spreadsheet.rs`

**Files:**
- Modify: `ia-core/src/spreadsheet.rs`

This function reads any supported file format and returns just the identifiers. For files with a recognized spreadsheet extension (`.csv`, `.tsv`, `.xlsx`, `.ods`, `.jsonl`), it uses `read_spreadsheet()` and extracts the `identifier` column. For unrecognized extensions or no extension, it falls back to plain text (one ID per line, skipping empty lines and `#` comments).

- [ ] **Step 1: Write failing tests for `read_identifiers_from_file`**

```rust
// In ia-core/src/spreadsheet.rs, add to existing #[cfg(test)] mod tests:

#[test]
fn read_identifiers_from_csv() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("items.csv");
    std::fs::write(&path, "identifier,title\nnasa,NASA\nmars,Mars\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa", "mars"]);
}

#[test]
fn read_identifiers_from_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("items.jsonl");
    std::fs::write(&path, "{\"identifier\":\"nasa\"}\n{\"identifier\":\"mars\"}\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa", "mars"]);
}

#[test]
fn read_identifiers_from_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ids.txt");
    std::fs::write(&path, "nasa\nmars\n# comment\n\napollo\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa", "mars", "apollo"]);
}

#[test]
fn read_identifiers_from_extensionless_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("myids");
    std::fs::write(&path, "nasa\nmars\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa", "mars"]);
}

#[test]
fn read_identifiers_from_tsv() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("items.tsv");
    std::fs::write(&path, "identifier\ttitle\nnasa\tNASA\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa"]);
}

#[test]
fn read_identifiers_file_not_found() {
    let path = std::path::Path::new("/nonexistent/file.csv");
    let err = read_identifiers_from_file(path).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("No such file") || msg.contains("not found"), "got: {msg}");
}

#[test]
fn read_identifiers_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dupes.txt");
    std::fs::write(&path, "nasa\nmars\nnasa\n").unwrap();

    let ids = read_identifiers_from_file(&path).unwrap();
    assert_eq!(ids, vec!["nasa", "mars"]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core read_identifiers_from -- --nocapture`
Expected: compile error — `read_identifiers_from_file` not defined

- [ ] **Step 3: Implement `read_identifiers_from_file`**

Add to `ia-core/src/spreadsheet.rs`, just after the `read_spreadsheet` function:

```rust
/// File extensions recognized as structured spreadsheet formats.
const SPREADSHEET_EXTENSIONS: &[&str] = &["csv", "tsv", "xlsx", "ods", "xls", "jsonl", "ndjson"];

/// Read identifiers from a file. Spreadsheet formats (.csv, .tsv, .xlsx, .ods,
/// .jsonl) are parsed and the `identifier` column is extracted. Unrecognized
/// extensions (including no extension) are read as plain text — one identifier
/// per line, skipping blanks and `#` comments. Duplicate identifiers are removed
/// (preserving first occurrence order).
pub fn read_identifiers_from_file(path: &Path) -> Result<Vec<String>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let ids = if SPREADSHEET_EXTENSIONS.contains(&ext.as_str()) {
        let records = read_spreadsheet(path)?;
        records.into_iter().map(|(id, _)| id).collect()
    } else {
        // Plain text: one identifier per line
        let content = std::fs::read_to_string(path)?;
        content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    };

    // Deduplicate preserving order
    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core read_identifiers_from -- --nocapture`
Expected: all 7 tests PASS

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/spreadsheet.rs
git commit -m "feat: add read_identifiers_from_file to spreadsheet module

Extracts identifiers from any file format — CSV/TSV/XLSX/ODS/JSONL
use the identifier column, unrecognized extensions fall back to plain
text (one ID per line). Deduplicates preserving order."
```

### Task 2: Redesign `ExportArgs` — positional args are files, not identifiers

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

Replace `BatchInput` in `ExportArgs` with file-based input. Positional args become `files: Vec<PathBuf>`, keep `--search`, add stdin support.

- [ ] **Step 1: Write failing CLI tests for new export behavior**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_export_accepts_file_positional_arg() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("items.csv");
    std::fs::write(&csv_path, "identifier,title\ntest-item-nonexistent,Test\n").unwrap();

    // Should fail with auth/network error, NOT a parse error or "unknown argument"
    ia().args(["metadata", "export", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unknown argument").not()
                .and(predicate::str::contains("unexpected argument").not()),
        );
}

#[test]
fn metadata_export_file_not_found_error() {
    ia().args(["metadata", "export", "/nonexistent/file.csv"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("No such file")));
}

#[test]
fn metadata_export_no_input_shows_help() {
    // No files, no search, terminal stdin → helpful error
    ia().args(["metadata", "export"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("No input")
                .or(predicate::str::contains("no input"))
                .or(predicate::str::contains("Pass files")),
        );
}

#[test]
fn metadata_export_plain_text_itemlist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ids.txt");
    std::fs::write(&path, "test-nonexistent-id\n").unwrap();

    // Should fail with auth/network error, not a parse error
    ia().args(["metadata", "export", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("parse error").not());
}

#[test]
fn metadata_export_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    let csv1 = dir.path().join("a.csv");
    let csv2 = dir.path().join("b.csv");
    std::fs::write(&csv1, "identifier\nid1\n").unwrap();
    std::fs::write(&csv2, "identifier\nid2\n").unwrap();

    // Should accept multiple file args without parse error
    ia().args([
        "metadata", "export",
        csv1.to_str().unwrap(),
        csv2.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("unexpected argument").not());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli metadata_export_accepts_file -- --nocapture`
Expected: FAIL (current code treats file path as identifier)

- [ ] **Step 3: Modify `ExportArgs` and `run_export`**

In `ia-cli/src/commands/metadata.rs`:

**Replace `ExportArgs`:**

```rust
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Input files (CSV, TSV, XLSX, ODS, JSONL, or plain text with one ID per line)
    #[arg()]
    pub files: Vec<PathBuf>,

    /// Use search results as input
    #[arg(long)]
    pub search: Option<String>,

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
```

**Add `collect_identifiers_from_export`:**

```rust
/// Collect identifiers from export sources (files, --search, stdin).
/// Positional args are always files. stdin is line-per-ID.
async fn collect_identifiers_from_export(
    args: &ExportArgs,
    client: &IaClient,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();

    // Read identifiers from each file
    for path in &args.files {
        let file_ids = ia_core::spreadsheet::read_identifiers_from_file(path)
            .context(format!("failed to read identifiers from {}", path.display()))?;
        ids.extend(file_ids);
    }

    // Search
    if let Some(ref query) = args.search {
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // stdin fallback: when no files and no search, read from stdin if piped
    if ids.is_empty()
        && args.files.is_empty()
        && args.search.is_none()
        && !std::io::stdin().is_terminal()
    {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.context("failed to read from stdin")?;
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed);
            }
        }
    }

    // Deduplicate preserving order
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));

    // Better error when no input at all
    if ids.is_empty() && args.files.is_empty() && args.search.is_none() {
        bail!(
            "No input provided. Pass files, --search, or pipe identifiers via stdin.\n\
             Examples:\n  \
             ia metadata export items.csv\n  \
             ia metadata export --search \"collection:nasa\"\n  \
             echo id1 | ia metadata export"
        );
    }

    Ok(ids)
}
```

**Update `run_export` to use the new collector:**

Change line 706 from:
```rust
let identifiers = collect_identifiers_from_batch(&args.input, client).await?;
```
to:
```rust
let identifiers = collect_identifiers_from_export(&args, client).await?;
```

**Update the export `long_about` and `after_long_help`:**

```rust
/// Bulk export metadata to stdout or file
#[command(
    long_about = "Export metadata for multiple items. Reads identifiers from files \
        (CSV, TSV, XLSX, ODS, JSONL, or plain text), search results, or stdin.\n\n\
        For spreadsheet files, the 'identifier' column is extracted. Plain text files \
        and stdin are read as one identifier per line.\n\n\
        Outputs JSONL to stdout by default, or writes to a file in CSV, TSV, XLSX, \
        or JSONL format (inferred from extension).",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Export from a CSV file</dim>\n  <bold>$ ia metadata export items.csv</bold>\
         \n\n  <dim># Export from a plain text ID list</dim>\n  <bold>$ ia metadata export ids.txt</bold>\
         \n\n  <dim># Export search results as JSONL</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
         \n\n  <dim># Export to CSV file</dim>\n  <bold>$ ia metadata export items.csv -o data.csv</bold>\
         \n\n  <dim># Pipe identifiers from another command</dim>\n  <bold>$ ia search \"collection:nasa\" -f identifier | ia metadata export</bold>\
         \n\n  <dim># Export to XLSX for editing, then re-import</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.xlsx</bold>\
         \n  <bold>$ ia metadata import data.xlsx --dry-run</bold>\n"
    ),
)]
Export(ExportArgs),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli metadata_export -- --nocapture`
Expected: all new and existing export tests PASS

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: export positional args are files, not identifiers

ExportArgs now takes files (CSV/TSV/XLSX/ODS/JSONL/plain text) as
positional args instead of bare identifiers. Identifiers are extracted
from the 'identifier' column for spreadsheets, or one-per-line for
plain text. stdin still works when piped with no file args."
```

## Chunk 2: Multi-ID bare mode + error messages

### Task 3: Extend bare `ia md` to accept multiple identifiers

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/tests/cli.rs`

Change `MetadataArgs.identifier` from `Option<String>` to `Vec<String>`. When multiple IDs are given, output JSONL. Single ID preserves current JSON behavior. Single-item flags (`--exists`, `--formats`) error with multiple IDs.

- [ ] **Step 1: Write failing tests**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_bare_multiple_ids_accepted() {
    // Multiple positional IDs should be accepted (will fail with network error, not parse error)
    ia().args(["metadata", "id1", "id2", "id3"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn metadata_bare_exists_rejects_multiple_ids() {
    ia().args(["metadata", "id1", "id2", "--exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("single identifier"));
}

#[test]
fn metadata_bare_formats_rejects_multiple_ids() {
    ia().args(["metadata", "id1", "id2", "--formats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("single identifier"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli metadata_bare_multiple -- --nocapture`
Expected: FAIL (clap rejects multiple positional args)

- [ ] **Step 3: Implement multi-ID bare mode**

In `ia-cli/src/commands/metadata.rs`:

**Change `MetadataArgs`:**

```rust
pub struct MetadataArgs {
    /// Item identifier(s)
    #[arg()]
    pub identifiers: Vec<String>,

    // ... rest unchanged
}
```

**Update the `None` branch in `run()` (bare read dispatch):**

```rust
None => {
    if continuations.is_some() {
        bail!("compound operations (+) require a write subcommand (modify, append, etc.)");
    }
    if args.identifiers.is_empty() {
        bail!("identifier required. Run 'ia metadata --help' for usage.");
    }
    if args.identifiers.len() > 1 && (args.exists || args.formats) {
        let flag = if args.exists { "--exists" } else { "--formats" };
        bail!("{flag} can only be used with a single identifier");
    }
    if args.identifiers.len() == 1 {
        run_read(
            client,
            &args.identifiers[0],
            args.exists,
            args.formats,
            args.pretty,
            args.json,
        )
        .await
    } else {
        run_read_multi(
            client,
            &args.identifiers,
            args.pretty,
            args.json,
            ctx.quiet,
            ctx.jobs,
        )
        .await
    }
}
```

**Add `run_read_multi`:**

```rust
async fn run_read_multi(
    client: &IaClient,
    identifiers: &[String],
    pretty: bool,
    json: bool,
    quiet: u8,
    jobs: usize,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(jobs));
    let client = Arc::new(client.clone());
    let mut set = JoinSet::new();

    // Spawn concurrent fetches preserving identifier order via index
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
            Ok::<_, anyhow::Error>((idx, item))
        });
    }

    // Collect results and sort by original order
    let mut results: Vec<(usize, ia_core::ItemMetadata)> = Vec::new();
    while let Some(result) = set.join_next().await {
        results.push(result??);
    }
    results.sort_by_key(|(idx, _)| *idx);

    // Output JSONL
    for (_, item) in &results {
        let output = if pretty || json {
            serde_json::to_string_pretty(item)?
        } else {
            serde_json::to_string(item)?
        };
        println!("{output}");
    }

    if quiet < 2 {
        eprintln!("{} item(s) exported", results.len());
    }

    Ok(())
}
```

Note: `IaClient` needs to be `Clone`. Check if it already is — it should be since it wraps `reqwest::Client` which is `Clone`. If not, use `Arc<IaClient>` in the caller.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli metadata_bare -- --nocapture`
Expected: all tests PASS

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: bare ia md accepts multiple identifiers with concurrency

ia md id1 id2 id3 now outputs JSONL with -j concurrency support.
Single-item flags (--exists, --formats) error when multiple IDs given."
```

### Task 4: Add helpful error message when bare mode gets a file-like identifier

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/tests/cli.rs`

When `run_read` gets a 404 / null metadata for an identifier that looks like a file path, suggest `ia md export` instead.

- [ ] **Step 1: Write failing test**

Add to `ia-cli/tests/cli.rs`:

```rust
#[test]
fn metadata_bare_file_like_identifier_hints_export() {
    // Create a file that exists with a spreadsheet extension
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("items.csv");
    std::fs::write(&csv_path, "identifier\nnasa\n").unwrap();

    // Running bare mode with a file path should hint at export
    // (will fail with network error but error message should contain hint)
    ia().args(["metadata", csv_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ia metadata export").or(
            predicate::str::contains("ia md export"),
        ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli metadata_bare_file_like -- --nocapture`
Expected: FAIL

- [ ] **Step 3: Add file-path hint to bare mode**

In the `None` branch of `run()`, after checking identifiers are non-empty but before calling `run_read` / `run_read_multi`, add a check when there's a single identifier:

```rust
// Hint: if the identifier looks like an existing file, suggest export
if args.identifiers.len() == 1 {
    let path = std::path::Path::new(&args.identifiers[0]);
    if path.exists() && path.is_file() {
        bail!(
            "\"{}\" appears to be a file. To export metadata from a file, use:\n  \
             ia metadata export {}",
            args.identifiers[0],
            args.identifiers[0],
        );
    }
}
```

Place this check right after the `--exists`/`--formats` validation, before calling `run_read`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ia-cli metadata_bare_file_like -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat: hint at 'ia md export' when bare mode gets a file path

When a single identifier matches an existing file on disk, bail with
a helpful message suggesting 'ia metadata export <file>' instead."
```

### Task 5: Add concurrency to `run_export`

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

Currently `run_export` fetches items sequentially in a `for` loop. Add `-j` concurrency support (the `jobs` parameter is already passed through `run()` but `run_export` only receives `quiet`).

- [ ] **Step 1: Update `run_export` signature and dispatch**

Change `run_export` to accept `jobs`:

```rust
async fn run_export(client: &IaClient, args: ExportArgs, quiet: u8, jobs: usize) -> Result<()> {
```

Update the call site in `run()`:

```rust
Some(MetadataCommand::Export(sub)) => {
    if continuations.is_some() {
        bail!("compound operations (+) cannot be used with export");
    }
    run_export(client, sub, ctx.quiet, ctx.jobs).await
}
```

- [ ] **Step 2: Replace sequential loop with concurrent fetch**

Replace the sequential `for identifier in &identifiers` loop with semaphore-bounded concurrent fetches (same pattern as `run_read_multi`), preserving order for deterministic output. The file-mode collection into `records: Vec<SpreadsheetRecord>` stays the same — just the fetch becomes concurrent.

```rust
let semaphore = Arc::new(Semaphore::new(jobs));
let client = Arc::new(client.clone());
let mut set = JoinSet::new();

for (idx, identifier) in identifiers.iter().enumerate() {
    let sem = semaphore.clone();
    let client = client.clone();
    let id = identifier.clone();
    set.spawn(async move {
        let _permit = sem.acquire().await?;
        let item = client
            .get_item(&id)
            .await
            .context(format!("failed to fetch metadata for {id}"))?;
        Ok::<_, anyhow::Error>((idx, id, item))
    });
}

let mut results: Vec<(usize, String, _)> = Vec::new();
while let Some(result) = set.join_next().await {
    results.push(result??);
}
results.sort_by_key(|(idx, _, _)| *idx);
```

Then iterate over `results` for output (stdout JSONL or file-mode record collection).

- [ ] **Step 3: Run all tests**

Run: `cargo test -p ia-cli metadata_export -- --nocapture`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "feat: add -j concurrency to metadata export

run_export now uses semaphore-bounded concurrent fetches matching
the run_read_multi pattern. Order is preserved for deterministic output."
```

### Task 6: Run full CI and verify

- [ ] **Step 1: Run `just ci`**

Run: `just ci`
Expected: fmt-check, check, test, doc all pass

- [ ] **Step 2: Fix any issues**

If clippy/fmt/doc warnings appear, fix them.

- [ ] **Step 3: Final commit if needed**

```bash
git commit -m "fix: address clippy/fmt warnings from export redesign"
```
