# CLI Subcommand Review Fixes — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all issues surfaced in the PR #189 code review: search parameter bugs, missing FTS num_found, export file output, missing tests, and error handling.

**Architecture:** Changes span ia-core (search opts, FTS num_found, spreadsheet writer) and ia-cli (search subcommands, metadata export, ai error handling). All changes are additive — no breaking API changes.

**Tech Stack:** Rust, clap 4, reqwest, csv, serde_json, wiremock (tests). New dep: `rust_xlsxwriter` for XLSX export.

**Worktree:** `~/github/internetarchivecanada/ia/.claude/worktrees/refactor-cli-subcommands/`

---

## Task 1: Add `rows` field to SearchOpts and fix `advanced()` page size

**Files:**
- Modify: `ia-core/src/search.rs:22-36` (SearchOpts struct)
- Modify: `ia-core/src/search.rs:158-247` (advanced() function)
- Test: `ia-core/src/search.rs` (tests module)

**Context:** The advanced search API uses `rows` as the page-size parameter, not `count`. Python library defaults to 50 rows. The Rust code hardcodes `page_size = 500`. The `count` field is for limiting total results yielded, not page size.

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn advanced_search_uses_rows_param() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/advancedsearch.php"))
        .and(query_param("rows", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": {
                "numFound": 1,
                "docs": [{"identifier": "item1", "title": "Test"}]
            }
        })))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let opts = SearchOpts {
        rows: 50,
        ..Default::default()
    };
    let results: Vec<Result<SearchResult>> =
        advanced(&client, "test", &opts).collect().await;

    assert_eq!(results.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/github/internetarchivecanada/ia/.claude/worktrees/refactor-cli-subcommands; cargo test -p ia-core advanced_search_uses_rows_param -- --nocapture`
Expected: FAIL — `rows` field doesn't exist on SearchOpts yet.

**Step 3: Add `rows` field and update `advanced()`**

In `SearchOpts`:
```rust
pub struct SearchOpts {
    pub fields: Vec<String>,
    pub sorts: Vec<String>,
    pub count: usize,
    /// Page size for advanced search (0 = use default of 50).
    pub rows: usize,
    pub timeout: Option<u64>,
    pub params: Vec<(String, String)>,
    pub dsl: bool,
}
```

In `advanced()`, replace:
```rust
let page_size = 500usize;
```
with:
```rust
let rows = if opts.rows > 0 { opts.rows } else { 50 };
```

And update the query param from `("rows", &page_size.to_string())` to `("rows", &rows.to_string())`.

Also update the pagination end check from `if yielded >= body.response.num_found as usize` to `if yielded >= body.response.num_found as usize`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core advanced_search_uses_rows_param -- --nocapture`
Expected: PASS

**Step 5: Run all tests to check for regressions**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All pass (fix any compilation errors from adding `rows` field — will need `rows: 0` or `..Default::default()` in existing tests).

**Step 6: Commit**

```
git add ia-core/src/search.rs
git commit -m "fix(ia-core): add rows field to SearchOpts, default advanced to 50

The advanced search API uses 'rows' as the page-size parameter.
Previously hardcoded to 500, now configurable via SearchOpts.rows
with a default of 50 (matching Python library behavior).

Refs #185"
```

---

## Task 2: Add `fts_num_found()` function to ia-core

**Files:**
- Modify: `ia-core/src/search.rs` (add new function after `num_found()`)
- Test: `ia-core/src/search.rs` (tests module)

**Context:** Python library gets FTS count via `GET fts_url?q=query` → `hits.total`. The Rust code was missing this, causing the CLI to bail on `--num-found` for FTS. The `num_found()` function currently only supports scrape.

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn fts_num_found_returns_total() {
    let mock_server = MockServer::start().await;

    // FTS num_found uses GET with query params (not POST with JSON body)
    Mock::given(method("GET"))
        .and(path("/ia-pub-fts-api"))
        .and(query_param("q", "!L test query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": {
                "total": 1234,
                "hits": []
            }
        })))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let count = fts_num_found(&client, "test query", false).await.unwrap();
    assert_eq!(count, 1234);
}

#[tokio::test]
async fn fts_num_found_dsl_mode_skips_prefix() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ia-pub-fts-api"))
        .and(query_param("q", "raw dsl query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": {
                "total": 42,
                "hits": []
            }
        })))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let count = fts_num_found(&client, "raw dsl query", true).await.unwrap();
    assert_eq!(count, 42);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core fts_num_found -- --nocapture`
Expected: FAIL — function doesn't exist.

**Step 3: Implement `fts_num_found()`**

```rust
/// Count total FTS results matching a query without fetching items.
///
/// Uses GET to the FTS endpoint (different from the POST used by search).
/// Reads `hits.total` from the response.
pub async fn fts_num_found(client: &IaClient, query: &str, dsl: bool) -> Result<u64> {
    let base_url = format!("{}://be-api.us.archive.org/ia-pub-fts-api", client.protocol());
    let q = if dsl {
        query.to_string()
    } else {
        format!("!L {query}")
    };

    let resp = client
        .http()
        .get(&base_url)
        .query(&[("q", &q)])
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(IaError::Http {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body: FtsResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
    Ok(body.hits.and_then(|h| h.total).unwrap_or(0))
}
```

NOTE: The mock server for FTS uses a different host (be-api.us.archive.org). For wiremock tests, the client's `protocol()` and host need to be overridden. The FTS function currently hardcodes the host. To make it testable, we need to make the FTS base URL configurable on IaClient, OR pass the mock server's URI into the function. Check how the existing FTS tests handle this — if there are none, we may need to add a `fts_url()` method to IaClient.

Actually, looking at the existing `fts()` function, it hardcodes: `format!("{}://be-api.us.archive.org/ia-pub-fts-api", client.protocol())`. This means the FTS endpoint is NOT routable through the mock server by default. We need a way to override the FTS host for testing.

**Option A:** Add `fts_host` to `IaConfig` (optional, defaults to `be-api.us.archive.org`). Use it in both `fts()` and `fts_num_found()`.

**Option B:** Add a helper method `client.fts_url(path)` that returns the FTS base URL, overridable in tests via config.

Go with **Option A** — add `fts_host: Option<String>` to the general config section. When set, both `fts()` and `fts_num_found()` use it. The mock_config helper sets it to the mock server host.

```rust
// In IaClient or config:
pub fn fts_base_url(&self) -> String {
    let host = self.config().general.fts_host
        .as_deref()
        .unwrap_or("be-api.us.archive.org");
    format!("{}://{host}/ia-pub-fts-api", self.protocol())
}
```

Update `fts()` and `fts_num_found()` to use `client.fts_base_url()`.

Update `mock_config()` in tests:
```rust
fn mock_config(server_uri: &str) -> crate::config::IaConfig {
    let mut config = crate::config::IaConfig::default();
    let host = server_uri
        .strip_prefix("http://")
        .or_else(|| server_uri.strip_prefix("https://"))
        .unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.fts_host = Some(host.to_string());
    config.general.secure = false;
    config
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core fts_num_found -- --nocapture`
Expected: PASS

**Step 5: Commit**

```
git add ia-core/src/search.rs ia-core/src/config.rs
git commit -m "feat(ia-core): add fts_num_found() for FTS result counting

Implements FTS num_found via GET to the FTS endpoint with query params,
matching Python library behavior. Reads hits.total from the response.

Also adds fts_host config option and fts_base_url() method to IaClient
for testability — FTS uses a different host than the main API.

Refs #185"
```

---

## Task 3: Add FTS `!L` prefix unit tests

**Files:**
- Modify: `ia-core/src/search.rs` (tests module)

**Context:** The plan specified `fts_prepends_literal_prefix` and `fts_dsl_mode_skips_prefix` tests but they were never written. These verify the `!L` prefix behavior added in the refactor.

**Step 1: Write the tests**

```rust
#[tokio::test]
async fn fts_prepends_literal_prefix() {
    let mock_server = MockServer::start().await;

    // Verify that the POST body contains "!L" prefix
    Mock::given(method("POST"))
        .and(path("/ia-pub-fts-api"))
        .and(wiremock::matchers::body_string_contains("!L test query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": { "total": 1, "hits": [
                {"_id": "item1|hash", "_source": {}, "fields": {"identifier": ["item1"]}}
            ]},
            "_scroll_id": ""
        })))
        .mount(&mock_server)
        .await;

    let mut config = mock_config(&mock_server.uri());
    config.general.fts_host = Some(config.general.host.clone());
    let client = IaClient::from_config(config).unwrap();
    let opts = SearchOpts::default(); // dsl=false by default
    let results: Vec<Result<SearchResult>> =
        fts(&client, "test query", &opts).collect().await;

    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn fts_dsl_mode_skips_prefix() {
    let mock_server = MockServer::start().await;

    // In DSL mode, query should NOT have !L prefix
    Mock::given(method("POST"))
        .and(path("/ia-pub-fts-api"))
        .and(wiremock::matchers::body_string_contains("raw dsl"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hits": { "total": 1, "hits": [
                {"_id": "item1|hash", "_source": {}, "fields": {"identifier": ["item1"]}}
            ]},
            "_scroll_id": ""
        })))
        .mount(&mock_server)
        .await;

    let mut config = mock_config(&mock_server.uri());
    config.general.fts_host = Some(config.general.host.clone());
    let client = IaClient::from_config(config).unwrap();
    let opts = SearchOpts {
        dsl: true,
        ..Default::default()
    };
    let results: Vec<Result<SearchResult>> =
        fts(&client, "raw dsl", &opts).collect().await;

    assert_eq!(results.len(), 1);
}
```

NOTE: Need to verify `body_string_contains` is available in wiremock 0.6. If not, use a custom matcher or check via request inspection.

**Step 2: Run tests**

Run: `cargo test -p ia-core fts_prepends_literal_prefix fts_dsl_mode_skips_prefix -- --nocapture`
Expected: PASS (the prefix logic already works, we're just adding test coverage)

**Step 3: Commit**

```
git add ia-core/src/search.rs
git commit -m "test(ia-core): add FTS !L prefix unit tests

Verifies that fts() prepends !L by default for literal text matching
and skips it when dsl=true for raw Elasticsearch DSL queries.

Refs #185"
```

---

## Task 4: Fix search CLI — `--num-found` for all backends, extract helper

**Files:**
- Modify: `ia-cli/src/commands/search.rs`

**Context:** Three issues combined:
1. FTS `--num-found` bails instead of working (now we have `fts_num_found()`)
2. Duplicate `num_found` logic in `run_scrape()` and `run_advanced()` (S-1)
3. `--num-found` stays in `SharedSearchArgs` (all three backends support it)

**Step 1: Extract `handle_num_found()` helper and implement FTS support**

```rust
/// Shared --num-found handler. Prints count and exits.
async fn handle_num_found(
    client: &IaClient,
    query: &str,
    backend: NumFoundBackend,
    json: bool,
) -> Result<()> {
    let count = match backend {
        NumFoundBackend::Scrape => ia_core::search::num_found(client, query).await?,
        NumFoundBackend::Advanced => ia_core::search::num_found(client, query).await?,
        NumFoundBackend::Fts { dsl } => ia_core::search::fts_num_found(client, query, dsl).await?,
    };
    if json {
        println!("{}", serde_json::json!({"num_found": count}));
    } else {
        println!("{count}");
    }
    Ok(())
}

enum NumFoundBackend {
    Scrape,
    Advanced,
    Fts { dsl: bool },
}
```

**Step 2: Update `run_scrape()`, `run_advanced()`, `run_fts()`**

Replace the duplicated num_found blocks in `run_scrape()` and `run_advanced()` with:
```rust
if shared.num_found {
    return handle_num_found(client, &query, NumFoundBackend::Scrape, shared.json).await;
}
```

Replace the bail in `run_fts()` with:
```rust
if args.shared.num_found {
    return handle_num_found(
        client,
        &args.query,
        NumFoundBackend::Fts { dsl: args.dsl },
        args.shared.json,
    ).await;
}
```

**Step 3: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: PASS

**Step 4: Commit**

```
git add ia-cli/src/commands/search.rs
git commit -m "fix(ia-cli): implement --num-found for FTS, extract shared helper

FTS --num-found now works via fts_num_found() (GET to FTS endpoint,
reads hits.total). Previously bailed with 'not supported'.

Extracted handle_num_found() helper to eliminate duplicate num_found
logic between run_scrape() and run_advanced().

Refs #185"
```

---

## Task 5: Fix advanced search CLI — `--rows` flag, single-page default

**Files:**
- Modify: `ia-cli/src/commands/search.rs` (AdvancedArgs, run_advanced)

**Context:** Advanced search should default to 50 rows (one page) matching Python behavior. Currently hardcodes 500 via `opts.count = 500` workaround.

**Step 1: Add `--rows` flag to AdvancedArgs**

```rust
#[derive(Debug, Args)]
pub struct AdvancedArgs {
    pub query: String,

    /// Results per page (default: 50)
    #[arg(short = 'r', long, default_value = "50")]
    pub rows: usize,

    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}
```

**Step 2: Update `run_advanced()` to use rows**

```rust
async fn run_advanced(
    client: &IaClient,
    query: String,
    rows: usize,
    sort: Vec<String>,
    field: Vec<String>,
    shared: SharedSearchArgs,
    quiet: u8,
) -> Result<()> {
    if shared.num_found {
        return handle_num_found(client, &query, NumFoundBackend::Advanced, shared.json).await;
    }

    let mut opts = build_search_opts(&field, &sort, &shared);
    opts.rows = rows;
    // Single page: limit total results to rows (one page)
    if opts.count == 0 {
        opts.count = rows;
    }
    let stream = ia_core::search::advanced(client, &query, &opts);
    run_output(stream, &shared, &field, &query, quiet).await
}
```

Update the dispatch in `run()`:
```rust
Some(SearchCommand::Advanced(sub)) => {
    run_advanced(client, sub.query, sub.rows, sub.sort, sub.field, sub.shared, quiet).await
}
```

**Step 3: Add CLI test for --rows flag**

```rust
#[test]
fn search_advanced_help_has_rows() {
    ia().args(["search", "advanced", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--rows"));
}
```

**Step 4: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: PASS

**Step 5: Commit**

```
git add ia-cli/src/commands/search.rs ia-cli/tests/cli.rs
git commit -m "fix(ia-cli): add --rows flag to advanced search, default 50

Advanced search now uses --rows (default 50) for page size, matching
Python library behavior. Single-page by default: count is set to rows
unless explicitly overridden.

Previously hardcoded page_size=500 and used count=500 workaround.

Refs #185"
```

---

## Task 6: Fix `fields_requested` — use it for field filtering in output

**Files:**
- Modify: `ia-cli/src/commands/search.rs` (run_output function)

**Context:** The `fields_requested` parameter in `run_output()` is accepted but suppressed with `let _ = fields_requested`. It should filter which fields are displayed in non-JSON, non-itemlist output mode.

**Step 1: Remove `let _ = fields_requested` and use it**

In `run_output()`, update the pretty-print branch:

```rust
} else if quiet == 0 && !item.fields.is_empty() {
    print!("{}", style(&item.identifier).bold());
    for (key, value) in &item.fields {
        // If specific fields were requested, only show those
        if !fields_requested.is_empty()
            && !fields_requested.iter().any(|f| f == key || f == "identifier")
        {
            continue;
        }
        let display = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        print!("  {}={}", style(key).dim(), display);
    }
    println!();
}
```

Also filter in JSON mode — only include requested fields:

```rust
if shared.json {
    let mut obj = if fields_requested.is_empty() {
        item.fields.clone()
    } else {
        item.fields
            .iter()
            .filter(|(k, _)| fields_requested.iter().any(|f| f.as_str() == k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    obj.insert(
        "identifier".to_string(),
        serde_json::Value::String(item.identifier),
    );
    println!("{}", serde_json::to_string(&obj)?);
}
```

**Step 2: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: PASS

**Step 3: Commit**

```
git add ia-cli/src/commands/search.rs
git commit -m "fix(ia-cli): use fields_requested for output filtering in search

Previously the fields_requested parameter was accepted but suppressed.
Now filters output to only requested fields in both JSON and pretty
print modes. The API-level field filtering (fl param) still controls
what's fetched; this adds client-side filtering for display.

Refs #184"
```

---

## Task 7: Fix metadata bare read test to verify error message

**Files:**
- Modify: `ia-cli/tests/cli.rs`

**Step 1: Update test**

```rust
#[test]
fn metadata_bare_read_still_requires_identifier() {
    ia().args(["metadata"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identifier"));
}
```

**Step 2: Run test**

Run: `cargo test -p ia-cli metadata_bare_read -- --nocapture`
Expected: PASS (the error message already says "identifier required")

NOTE: Check if the error goes to stderr or if clap sends it elsewhere. The dispatch code returns `anyhow::anyhow!("identifier required...")` which should end up on stderr.

**Step 3: Commit**

```
git add ia-cli/tests/cli.rs
git commit -m "test(ia-cli): verify metadata bare read error message

Refs #186"
```

---

## Task 8: Add `parse_column_op()` unit tests

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (add tests module or use ia-cli/tests/)

**Context:** `parse_column_op()` handles column prefix parsing for import. Needs unit tests for edge cases.

**Step 1: Add tests**

Since `parse_column_op` is a private function in metadata.rs, add a `#[cfg(test)]` module at the bottom of metadata.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_column_op_default_is_set() {
        let (op, field) = parse_column_op("title");
        assert!(matches!(op, MetadataOp::Set));
        assert_eq!(field, "title");
    }

    #[test]
    fn parse_column_op_append_prefix() {
        let (op, field) = parse_column_op("append:description");
        assert!(matches!(op, MetadataOp::Append));
        assert_eq!(field, "description");
    }

    #[test]
    fn parse_column_op_append_list_prefix() {
        let (op, field) = parse_column_op("append-list:subject");
        assert!(matches!(op, MetadataOp::AppendList));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_remove_prefix() {
        let (op, field) = parse_column_op("remove:subject");
        assert!(matches!(op, MetadataOp::Remove));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_insert_with_index() {
        let (op, field) = parse_column_op("insert:subject[2]");
        assert!(matches!(op, MetadataOp::Insert(2)));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_insert_without_index_defaults_to_zero() {
        let (op, field) = parse_column_op("insert:subject");
        assert!(matches!(op, MetadataOp::Insert(0)));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_empty_field_after_prefix() {
        // Edge case: "append:" with no field name
        let (op, field) = parse_column_op("append:");
        assert!(matches!(op, MetadataOp::Append));
        assert_eq!(field, "");
    }

    #[test]
    fn parse_column_op_colon_in_field_name() {
        // A field name that happens to contain a colon but doesn't match a prefix
        let (op, field) = parse_column_op("some:random:field");
        assert!(matches!(op, MetadataOp::Set));
        assert_eq!(field, "some:random:field");
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p ia-cli parse_column_op -- --nocapture`
Expected: PASS

**Step 3: Commit**

```
git add ia-cli/src/commands/metadata.rs
git commit -m "test(ia-cli): add parse_column_op unit tests

Covers all prefix types, edge cases (empty field name, missing index,
colon in field name).

Refs #186"
```

---

## Task 9: Fix `unwrap_or_default()` in ai.rs undo JSON output

**Files:**
- Modify: `ia-cli/src/commands/ai.rs:255-259`

**Step 1: Replace `unwrap_or_default()` with `?`**

Change:
```rust
if undo_args.json {
    println!(
        "{}",
        serde_json::to_string(&summary).unwrap_or_default()
    );
}
```

To:
```rust
if undo_args.json {
    println!("{}", serde_json::to_string(&summary)?);
}
```

Also fix the same pattern in the pipeline summary output at the end of `run()`:
```rust
if json_output {
    println!("{}", serde_json::to_string(&summary)?);
}
```

**Step 2: Run tests**

Run: `cargo test -p ia-cli`
Expected: PASS

**Step 3: Commit**

```
git add ia-cli/src/commands/ai.rs
git commit -m "fix(ia-cli): propagate serialization errors in ai command

Replace unwrap_or_default() with ? to avoid silently producing empty
output on serialization failure.

Refs #187"
```

---

## Task 10: Implement spreadsheet write module in ia-core

**Files:**
- Modify: `ia-core/src/spreadsheet.rs` (add write functions)
- Modify: `ia-core/Cargo.toml` (add rust_xlsxwriter dependency)
- Test: `ia-core/src/spreadsheet.rs` (tests module)

**Context:** Export needs to write CSV, TSV, XLSX, and JSONL. CSV/TSV use existing `csv` crate. XLSX needs `rust_xlsxwriter`. JSONL uses `serde_json`. ODS writing is not supported (ODS import still works via calamine).

**NOTE:** Adding `rust_xlsxwriter` requires user approval per CLAUDE.md rules. Ask before adding.

**Step 1: Write failing tests**

```rust
#[test]
fn write_csv() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let records = vec![
        ("nasa".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "NASA Images".to_string());
            m.insert("date".to_string(), "2024-01-01".to_string());
            m
        }),
        ("mars".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "Mars Rover".to_string());
            m
        }),
    ];

    write_spreadsheet(&path, &records).unwrap();

    // Read back and verify
    let read_back = read_spreadsheet(&path).unwrap();
    assert_eq!(read_back.len(), 2);
    assert_eq!(read_back[0].0, "nasa");
    assert_eq!(read_back[0].1.get("title").unwrap(), "NASA Images");
    assert_eq!(read_back[1].0, "mars");
    assert_eq!(read_back[1].1.get("title").unwrap(), "Mars Rover");
}

#[test]
fn write_tsv() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.tsv");
    let records = vec![
        ("item1".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "Test".to_string());
            m
        }),
    ];

    write_spreadsheet(&path, &records).unwrap();
    let read_back = read_spreadsheet(&path).unwrap();
    assert_eq!(read_back.len(), 1);
    assert_eq!(read_back[0].1.get("title").unwrap(), "Test");
}

#[test]
fn write_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.jsonl");
    let records = vec![
        ("nasa".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "NASA".to_string());
            m
        }),
    ];

    write_spreadsheet(&path, &records).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let line: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(line["identifier"], "nasa");
    assert_eq!(line["title"], "NASA");
}

#[test]
fn write_xlsx() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.xlsx");
    let records = vec![
        ("nasa".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "NASA".to_string());
            m
        }),
    ];

    write_spreadsheet(&path, &records).unwrap();
    // Verify we can read it back with calamine
    let read_back = read_spreadsheet(&path).unwrap();
    assert_eq!(read_back.len(), 1);
    assert_eq!(read_back[0].0, "nasa");
}

#[test]
fn write_csv_round_trip() {
    // Test that write → read produces identical data
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("round.csv");
    let original = vec![
        ("a".to_string(), {
            let mut m = HashMap::new();
            m.insert("x".to_string(), "1".to_string());
            m.insert("y".to_string(), "2".to_string());
            m
        }),
        ("b".to_string(), {
            let mut m = HashMap::new();
            m.insert("x".to_string(), "3".to_string());
            m
        }),
    ];

    write_spreadsheet(&path, &original).unwrap();
    let read_back = read_spreadsheet(&path).unwrap();

    assert_eq!(read_back.len(), 2);
    assert_eq!(read_back[0].0, "a");
    assert_eq!(read_back[0].1.get("x").unwrap(), "1");
    assert_eq!(read_back[0].1.get("y").unwrap(), "2");
    assert_eq!(read_back[1].0, "b");
    assert_eq!(read_back[1].1.get("x").unwrap(), "3");
    assert!(read_back[1].1.get("y").is_none()); // Missing value not present
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core write_csv write_tsv write_jsonl write_xlsx write_csv_round_trip -- --nocapture`
Expected: FAIL — `write_spreadsheet` doesn't exist.

**Step 3: Implement `write_spreadsheet()`**

```rust
/// Write records to a spreadsheet file.
/// Format auto-detected by file extension: .csv, .tsv, .xlsx, .jsonl
pub fn write_spreadsheet(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => write_csv(path, records, b','),
        "tsv" => write_csv(path, records, b'\t'),
        "xlsx" => write_xlsx(path, records),
        "jsonl" | "ndjson" => write_jsonl(path, records),
        other => Err(IaError::Config(format!(
            "unsupported export format: .{other} (supported: .csv, .tsv, .xlsx, .jsonl)"
        ))),
    }
}

fn write_csv(path: &Path, records: &[SpreadsheetRecord], delimiter: u8) -> Result<()> {
    // Collect all unique field names (sorted for deterministic output)
    let mut all_fields: Vec<String> = records
        .iter()
        .flat_map(|(_, fields)| fields.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    all_fields.sort();

    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .map_err(|e| IaError::Config(format!("failed to create CSV writer: {e}")))?;

    // Header row
    let mut header = vec!["identifier".to_string()];
    header.extend(all_fields.iter().cloned());
    writer
        .write_record(&header)
        .map_err(|e| IaError::Config(format!("failed to write CSV header: {e}")))?;

    // Data rows
    for (identifier, fields) in records {
        let mut row = vec![identifier.clone()];
        for field in &all_fields {
            row.push(fields.get(field).cloned().unwrap_or_default());
        }
        writer
            .write_record(&row)
            .map_err(|e| IaError::Config(format!("CSV write error: {e}")))?;
    }

    writer
        .flush()
        .map_err(|e| IaError::Config(format!("CSV flush error: {e}")))?;

    Ok(())
}

fn write_jsonl_file(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;

    for (identifier, fields) in records {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "identifier".to_string(),
            serde_json::Value::String(identifier.clone()),
        );
        for (key, value) in fields {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
        let line = serde_json::to_string(&obj)
            .map_err(|e| IaError::Config(format!("JSON serialization error: {e}")))?;
        writeln!(file, "{line}")?;
    }

    Ok(())
}

fn write_xlsx(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    use rust_xlsxwriter::Workbook;

    let mut all_fields: Vec<String> = records
        .iter()
        .flat_map(|(_, fields)| fields.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    all_fields.sort();

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();

    // Header row
    sheet.write_string(0, 0, "identifier")
        .map_err(|e| IaError::Config(format!("XLSX write error: {e}")))?;
    for (col, field) in all_fields.iter().enumerate() {
        sheet.write_string(0, (col + 1) as u16, field)
            .map_err(|e| IaError::Config(format!("XLSX write error: {e}")))?;
    }

    // Data rows
    for (row_idx, (identifier, fields)) in records.iter().enumerate() {
        let row = (row_idx + 1) as u32;
        sheet.write_string(row, 0, identifier)
            .map_err(|e| IaError::Config(format!("XLSX write error: {e}")))?;
        for (col, field) in all_fields.iter().enumerate() {
            if let Some(value) = fields.get(field) {
                sheet.write_string(row, (col + 1) as u16, value)
                    .map_err(|e| IaError::Config(format!("XLSX write error: {e}")))?;
            }
        }
    }

    workbook.save(path)
        .map_err(|e| IaError::Config(format!("XLSX save error: {e}")))?;

    Ok(())
}
```

**Step 4: Add `rust_xlsxwriter` to Cargo.toml**

In `ia-core/Cargo.toml`:
```toml
rust_xlsxwriter = "0.79"
```

NOTE: This needs user approval. Justification: XLSX export for `ia metadata export -o file.xlsx`, symmetric with calamine-based XLSX import.

**Step 5: Run tests**

Run: `cargo test -p ia-core write_csv write_tsv write_jsonl write_xlsx write_csv_round_trip -- --nocapture`
Expected: PASS

**Step 6: Commit**

```
git add ia-core/src/spreadsheet.rs ia-core/Cargo.toml Cargo.lock
git commit -m "feat(ia-core): add write_spreadsheet() for CSV/TSV/XLSX/JSONL export

Symmetric with read_spreadsheet() — format detected by file extension.
Uses existing csv crate for CSV/TSV, serde_json for JSONL.
Adds rust_xlsxwriter for XLSX export (new dependency).
ODS export not supported (ODS import still works via calamine).

Refs #186"
```

---

## Task 11: Implement `ia metadata export -o` file output

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs` (run_export function)
- Test: `ia-cli/tests/cli.rs`

**Context:** `run_export()` currently bails on `-o`. Now that ia-core has `write_spreadsheet()`, implement the full export pipeline.

**Step 1: Rewrite `run_export()`**

```rust
async fn run_export(client: &IaClient, args: ExportArgs, quiet: u8) -> Result<()> {
    let identifiers = collect_identifiers_from_batch(&args.input, client).await?;
    if identifiers.is_empty() {
        bail!("no identifiers to export");
    }

    // Fetch metadata for all items
    let mut records: Vec<ia_core::spreadsheet::SpreadsheetRecord> = Vec::new();
    for identifier in &identifiers {
        let item = client
            .get_item(identifier)
            .await
            .context(format!("failed to fetch metadata for {identifier}"))?;

        // Flatten metadata fields into a string HashMap
        let metadata_json = serde_json::to_value(&item.metadata)?;
        let mut fields = std::collections::HashMap::new();
        if let serde_json::Value::Object(map) = metadata_json {
            for (key, value) in map {
                if key == "identifier" {
                    continue;
                }
                let s = match &value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(arr) => {
                        // Join array values with "; " for spreadsheet output
                        arr.iter()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    }
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                if !s.is_empty() {
                    fields.insert(key, s);
                }
            }
        }

        // For JSONL/stdout: output immediately (streaming)
        if args.output.is_none() {
            let output = if args.pretty {
                serde_json::to_string_pretty(&item)?
            } else {
                serde_json::to_string(&item)?
            };
            println!("{output}");
        }

        records.push((identifier.clone(), fields));
    }

    // Write to file if -o specified
    if let Some(ref path) = args.output {
        ia_core::spreadsheet::write_spreadsheet(path, &records)
            .context(format!("failed to write export file: {}", path.display()))?;

        if quiet < 2 {
            eprintln!(
                "{} item(s) exported to {}",
                records.len(),
                path.display()
            );
        }
    } else if quiet < 2 && !args.json && !args.pretty {
        eprintln!("{} item(s) exported", identifiers.len());
    }

    Ok(())
}
```

**Step 2: Add CLI test**

```rust
#[test]
fn metadata_export_help_has_output_formats() {
    ia().args(["metadata", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".csv"))
        .stdout(predicate::str::contains(".xlsx"))
        .stdout(predicate::str::contains(".jsonl"));
}
```

**Step 3: Run tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: PASS

**Step 4: Commit**

```
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
git commit -m "feat(ia-cli): implement metadata export -o file output

ia metadata export -o data.csv now writes to CSV, TSV, XLSX, or JSONL
(format detected by file extension). Symmetric with import.

Array metadata values are joined with '; ' for spreadsheet columns.
Streaming JSONL still works when no -o is specified.

Refs #186"
```

---

## Task 12: Final cleanup — update help text and run full test suite

**Files:**
- Modify: `ia-cli/src/commands/search.rs` (help text for --rows, FTS num_found)
- Modify: `ia-cli/src/commands/metadata.rs` (export help with formats)

**Step 1: Update help text**

- FTS `long_about`: mention that `--num-found` is supported
- Advanced `long_about`: mention `--rows` flag and default 50
- Export `long_about`: update to list supported formats for `-o`
- Remove "not yet implemented" from export

**Step 2: Run full test suite and clippy**

Run: `cargo test -p ia-core -p ia-cli`
Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: All pass, zero warnings.

**Step 3: Commit**

```
git add ia-cli/src/commands/search.rs ia-cli/src/commands/metadata.rs
git commit -m "docs(ia-cli): update help text for search and metadata changes

Updated long_about and examples for advanced search (--rows), FTS
(--num-found supported), and metadata export (format list).

Refs #184, #185, #186"
```

---

## Summary of all changes

| Issue | Fix | Files |
|-------|-----|-------|
| I-1: `fields_requested` unused | Use for output filtering | search.rs (CLI) |
| I-2: FTS `--num-found` bails | Implement via `fts_num_found()` GET endpoint | search.rs (core + CLI) |
| I-3: Advanced rows=500, wrong default | Add `rows` to SearchOpts, default 50 | search.rs (core + CLI) |
| I-4: Bare metadata test weak | Assert error message | cli.rs |
| S-1: Duplicate num_found logic | Extract `handle_num_found()` | search.rs (CLI) |
| S-2: Export -o not implemented | Full CSV/TSV/XLSX/JSONL export | spreadsheet.rs, metadata.rs |
| S-3: No `parse_column_op` tests | Add unit tests | metadata.rs |
| S-4: `unwrap_or_default()` | Replace with `?` | ai.rs |
| S-5: No FTS prefix tests | Add wiremock tests | search.rs (core) |

**New dependency:** `rust_xlsxwriter` (XLSX export). Needs approval.
