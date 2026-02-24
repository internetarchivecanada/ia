use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Args;
use color_print::cstr;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, MetadataOp, ModifyRequest,
    ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS,
};
use ia_core::rate_limit::RateLimiter;
use ia_core::search::SearchOpts;
use ia_core::{IaClient, IaError};

#[derive(Args)]
#[command(
    long_about = "Read or modify item metadata. By default, displays the full metadata JSON for \
        an item. Use --modify, --append, --remove, and related flags to update metadata fields. \
        Supports bulk operations via --itemlist, --search, or --spreadsheet.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># View metadata for an item</dim>\n  <bold>$ ia metadata nasa</bold>\
         \n\n  <dim># Set a metadata field</dim>\n  <bold>$ ia metadata nasa --modify=\"description:Updated description\"</bold>\
         \n\n  <dim># Bulk update from a spreadsheet</dim>\n  <bold>$ ia metadata --spreadsheet updates.csv</bold>\
         \n\n  <dim># Machine-readable metadata output</dim>\n  <bold>$ ia metadata nasa --json</bold>\n"
    ),
)]
pub struct MetadataArgs {
    /// Item identifier(s)
    #[arg()]
    pub identifiers: Vec<String>,

    /// Check if item exists (exit code 0=yes, 1=no)
    #[arg(short = 'e', long)]
    pub exists: bool,

    /// List available file formats
    #[arg(short = 'F', long)]
    pub formats: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,

    /// Output results as JSON (structured output for scripts/agents)
    #[arg(long)]
    pub json: bool,

    // --- Write flags (mutually exclusive via "write_op" group) ---
    /// Set field to value: --modify="field:value" (repeatable)
    #[arg(short = 'm', long = "modify", value_name = "K:V", group = "write_op")]
    pub modify: Vec<String>,

    /// Append to string field: --append="field:value" (repeatable)
    #[arg(short = 'a', long = "append", value_name = "K:V", group = "write_op")]
    pub append: Vec<String>,

    /// Append to list field: --append-list="field:value" (repeatable)
    #[arg(short = 'A', long = "append-list", value_name = "K:V", group = "write_op")]
    pub append_list: Vec<String>,

    /// Insert at index: --insert="field[N]:value" (repeatable)
    #[arg(short = 'I', long = "insert", value_name = "K[N]:V", group = "write_op")]
    pub insert: Vec<String>,

    /// Remove value: --remove="field:value" (repeatable)
    #[arg(short = 'r', long = "remove", value_name = "K:V", group = "write_op")]
    pub remove: Vec<String>,

    // --- Write options ---
    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Optimistic concurrency check: fail if field doesn't match expected value
    #[arg(long, value_name = "K:V")]
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

    // --- Bulk input ---
    /// Read identifiers from file (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input (queries archive.org, modifies each result)
    #[arg(long)]
    pub search: Option<String>,

    /// Bulk from file (CSV, TSV, XLSX, ODS, JSONL)
    #[arg(short = 's', long)]
    pub spreadsheet: Option<PathBuf>,
}

pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Determine if this is a write operation
    let is_write = !args.modify.is_empty()
        || !args.append.is_empty()
        || !args.append_list.is_empty()
        || !args.insert.is_empty()
        || !args.remove.is_empty()
        || args.spreadsheet.is_some();

    if !is_write {
        return run_read(client, &args).await;
    }

    run_write(client, &args, quiet, jobs, joblog_path).await
}

/// Read mode: existing behavior for metadata display, --exists, --formats.
async fn run_read(client: &IaClient, args: &MetadataArgs) -> Result<()> {
    let identifier = args
        .identifiers
        .first()
        .context("at least one identifier is required")?;

    if args.exists {
        let exists = client
            .item_exists(identifier)
            .await
            .context(format!("failed to check existence of {identifier}"))?;
        if args.json {
            println!("{}", serde_json::json!({"identifier": identifier, "exists": exists}));
            if !exists {
                std::process::exit(1);
            }
        } else if !exists {
            std::process::exit(1);
        }
        return Ok(());
    }

    let item = client
        .get_item(identifier)
        .await
        .context(format!("failed to fetch metadata for {identifier}"))?;

    if args.formats {
        let mut formats: Vec<String> = item
            .files
            .iter()
            .filter_map(|f| f.format.clone())
            .collect();
        formats.sort();
        formats.dedup();
        if args.json {
            println!("{}", serde_json::to_string(&formats).unwrap_or_default());
        } else {
            for fmt in formats {
                println!("{fmt}");
            }
        }
        return Ok(());
    }

    let json = if args.pretty {
        serde_json::to_string_pretty(&item)?
    } else {
        serde_json::to_string(&item)?
    };
    println!("{json}");

    Ok(())
}

/// Write mode: parse changes, collect identifiers, call modify().
async fn run_write(
    client: &IaClient,
    args: &MetadataArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Spreadsheet mode: read records and process each as a modify() call
    if let Some(ref spreadsheet_path) = args.spreadsheet {
        return run_spreadsheet(client, args, spreadsheet_path, quiet, jobs, joblog_path).await;
    }

    // Parse changes and determine operation
    let (raw_changes, op) = parse_write_flags(args)?;

    // Normalize changes into groups. For Insert, each arg gets its own group
    // with its own index (e.g., collection[0]:featured → field="collection", index=0).
    // For other ops, all changes are in a single group.
    let change_groups: Vec<(Vec<(String, serde_json::Value)>, MetadataOp)> =
        if matches!(op, MetadataOp::Insert(_)) {
            raw_changes
                .iter()
                .map(|s| {
                    let (key, value) = parse_key_value(s)
                        .context(format!("invalid key:value format: {s:?}"))?;
                    let (field, index) =
                        parse_indexed_key(&key).unwrap_or((key, 0));
                    Ok((vec![(field, json!(value))], MetadataOp::Insert(index)))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            let changes: Vec<(String, serde_json::Value)> = raw_changes
                .iter()
                .map(|s| {
                    let (key, value) = parse_key_value(s)
                        .context(format!("invalid key:value format: {s:?}"))?;
                    Ok((key, json!(value)))
                })
                .collect::<Result<Vec<_>>>()?;
            vec![(changes, op)]
        };

    // Warn about immutable/admin-only fields
    for (changes, _) in &change_groups {
        for (key, _) in changes {
            let field = parse_indexed_key(key)
                .map(|(f, _)| f)
                .unwrap_or_else(|| key.clone());
            if IMMUTABLE_FIELDS.contains(&field.as_str()) {
                eprintln!("warning: field {field:?} cannot be modified (immutable)");
            }
            if ADMIN_ONLY_FIELDS.contains(&field.as_str()) {
                eprintln!("warning: field {field:?} typically requires IA admin access");
            }
        }
    }

    // Parse --expect values
    let expect: Option<HashMap<String, serde_json::Value>> = if !args.expect.is_empty() {
        let mut map = HashMap::new();
        for s in &args.expect {
            let (key, value) =
                parse_key_value(s).context(format!("invalid expect key:value: {s:?}"))?;
            map.insert(key, json!(value));
        }
        Some(map)
    } else {
        None
    };

    // Capture json flag before borrowing args in async tasks
    let json = args.json;

    // Collect identifiers from all sources
    let identifiers = collect_identifiers(args, client).await?;
    if identifiers.is_empty() {
        bail!("no identifiers provided");
    }

    // Open joblog writer
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let priority = args
        .priority
        .unwrap_or(if identifiers.len() > 1 { -5 } else { 0 });

    // Dry-run: sequential, no concurrency needed
    if args.dry_run {
        if !json && quiet == 0 {
            println!("Dry run -- no changes will be applied\n");
        }
        let mut total_dry_run_changes = 0usize;
        for identifier in &identifiers {
            for (changes, batch_op) in &change_groups {
                total_dry_run_changes += run_dry_run(
                    client,
                    identifier,
                    changes,
                    batch_op,
                    &args.target,
                    expect.as_ref(),
                    quiet,
                    json,
                )
                .await?;
            }
        }
        if !json && quiet == 0 {
            println!(
                "\n{} item(s), {} change(s)",
                identifiers.len(),
                total_dry_run_changes
            );
        }
        return Ok(());
    }

    let file_target = if args.target == "metadata" {
        String::new()
    } else {
        args.target.clone()
    };

    // Concurrent batch processing with JoinSet + Semaphore + RateLimiter
    let semaphore = Arc::new(Semaphore::new(jobs));
    let rate_limiter = RateLimiter::new();

    let mut set = JoinSet::new();
    let total_count = identifiers.len();

    for identifier in identifiers {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let change_groups = change_groups.clone();
        let target = args.target.clone();
        let expect = expect.clone();
        let reduced_priority = args.reduced_priority;

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let start = std::time::Instant::now();
            let mut batch_error = None;
            let mut last_task_id = None;

            for (changes, batch_op) in &change_groups {
                let req = ModifyRequest {
                    identifier: identifier.clone(),
                    changes: changes.clone(),
                    op: batch_op.clone(),
                    target: target.clone(),
                    expect: expect.clone(),
                    priority: Some(priority),
                    reduced_priority,
                };

                // Retry loop for 429 rate limiting
                loop {
                    rl.wait_if_paused().await;
                    match ia_core::metadata::modify(&client, &req).await {
                        Ok(resp) => {
                            last_task_id = resp.task_id;
                            break;
                        }
                        Err(IaError::RateLimited { retry_after }) => {
                            rl.pause_for(retry_after, |secs| {
                                eprintln!(
                                    "Rate limited. Pausing all workers for {secs}s..."
                                );
                            })
                            .await;
                            // retry
                        }
                        Err(e) => {
                            batch_error = Some(e);
                            break;
                        }
                    }
                }

                if batch_error.is_some() {
                    break;
                }
            }

            let elapsed_ms = start.elapsed().as_millis() as u64;
            let outcome = match batch_error {
                None => Ok(last_task_id),
                Some(e) => Err(e.to_string()),
            };

            (identifier, outcome, elapsed_ms)
        });
    }

    // Collect results — output is clean because we process one at a time
    let mut error_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) =
            result.context("task panicked")?;

        if record_modify_outcome(
            &identifier,
            &outcome,
            elapsed_ms,
            &file_target,
            quiet,
            joblog.as_ref(),
            json,
        ) {
            error_count += 1;
        }
    }

    if error_count > 0 {
        if json {
            std::process::exit(1);
        }
        bail!("{error_count} of {total_count} item(s) failed");
    }

    Ok(())
}

/// Parse write flags into (raw_changes, MetadataOp).
fn parse_write_flags(args: &MetadataArgs) -> Result<(Vec<String>, MetadataOp)> {
    if !args.modify.is_empty() {
        Ok((args.modify.clone(), MetadataOp::Set))
    } else if !args.append.is_empty() {
        Ok((args.append.clone(), MetadataOp::Append))
    } else if !args.append_list.is_empty() {
        Ok((args.append_list.clone(), MetadataOp::AppendList))
    } else if !args.insert.is_empty() {
        // Each --insert arg may have a different index (e.g., collection[0], subject[2]).
        // The actual per-arg index is parsed in run_write() when building change_groups.
        Ok((args.insert.clone(), MetadataOp::Insert(0)))
    } else if !args.remove.is_empty() {
        Ok((args.remove.clone(), MetadataOp::Remove))
    } else {
        bail!("no write operation specified")
    }
}

/// Dry-run: fetch metadata, compute patch, display without writing.
/// Returns the number of non-test patch operations.
#[allow(clippy::too_many_arguments)]
async fn run_dry_run(
    client: &IaClient,
    identifier: &str,
    changes: &[(String, serde_json::Value)],
    op: &MetadataOp,
    target: &str,
    expect: Option<&HashMap<String, serde_json::Value>>,
    quiet: u8,
    json: bool,
) -> Result<usize> {
    let url = client.url(&format!("/metadata/{identifier}"));
    let resp = client.http().get(&url).send().await?;
    let item: serde_json::Value = resp.json().await.map_err(|e| anyhow::anyhow!("{e}"))?;

    let source = extract_target_metadata(&item, target, identifier)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let patch = ia_core::metadata::compute_patch(&source, changes, op, expect, identifier)?;

    let change_count = patch
        .iter()
        .filter(|p| p["op"].as_str() != Some("test"))
        .count();

    if json {
        if patch.is_empty() {
            println!("{}", serde_json::json!({
                "item": identifier,
                "status": "no_changes",
                "dry_run": true,
            }));
        } else {
            let changes: Vec<serde_json::Value> = patch.iter()
                .filter(|p| p["op"].as_str() != Some("test"))
                .cloned()
                .collect();
            println!("{}", serde_json::json!({
                "item": identifier,
                "status": "would_modify",
                "dry_run": true,
                "changes": changes,
            }));
        }
    } else if quiet == 0 {
        if patch.is_empty() {
            println!("  {identifier}: no changes");
        } else {
            println!("  {identifier}:");
            for p in &patch {
                let op_type = p["op"].as_str().unwrap_or("?");
                let path = p["path"].as_str().unwrap_or("?");
                if op_type == "test" {
                    continue;
                }
                if let Some(value) = p.get("value") {
                    println!("    {path}: {op_type} -> {value}");
                } else {
                    println!("    {path}: {op_type}");
                }
            }
        }
    }

    Ok(change_count)
}

/// Spreadsheet mode: read records from file and modify each item.
async fn run_spreadsheet(
    client: &IaClient,
    args: &MetadataArgs,
    spreadsheet_path: &Path,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Capture json flag before borrowing args in async tasks
    let json = args.json;

    let records = ia_core::spreadsheet::read_spreadsheet(spreadsheet_path)
        .context(format!(
            "failed to read spreadsheet: {}",
            spreadsheet_path.display()
        ))?;

    // Determine op: use explicit write flag if given, else default to Set.
    // Note: the flag VALUES are ignored in spreadsheet mode — only the op type
    // matters. All field data comes from the spreadsheet records.
    let op = if !args.insert.is_empty() {
        bail!("--insert cannot be combined with --spreadsheet (CSV columns cannot encode per-field indices)");
    } else if !args.modify.is_empty() {
        MetadataOp::Set
    } else if !args.append.is_empty() {
        MetadataOp::Append
    } else if !args.append_list.is_empty() {
        MetadataOp::AppendList
    } else if !args.remove.is_empty() {
        MetadataOp::Remove
    } else {
        MetadataOp::Set // default when no flag
    };

    // Warn if write flag values are provided alongside --spreadsheet
    // (only the op mode is used; field data comes from the spreadsheet)
    let has_flag_values = !args.modify.is_empty()
        || !args.append.is_empty()
        || !args.append_list.is_empty()
        || !args.remove.is_empty();
    if has_flag_values && quiet == 0 {
        eprintln!(
            "warning: write flag values are ignored in spreadsheet mode \
             (only the operation type is used; field data comes from the spreadsheet)"
        );
    }

    let priority = args.priority.unwrap_or(-5);

    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Build (identifier, changes) pairs, filtering empty records
    let work_items: Vec<(String, Vec<(String, serde_json::Value)>)> = records
        .iter()
        .filter_map(|(identifier, fields)| {
            let changes: Vec<(String, serde_json::Value)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            if changes.is_empty() {
                None
            } else {
                Some((identifier.clone(), changes))
            }
        })
        .collect();

    let item_count = work_items.len();

    // Dry-run: sequential
    if args.dry_run {
        if !json && quiet == 0 {
            println!("Dry run -- no changes will be applied\n");
        }
        let mut total_changes = 0usize;
        for (identifier, changes) in &work_items {
            total_changes += run_dry_run(
                client,
                identifier,
                changes,
                &op,
                &args.target,
                None,
                quiet,
                json,
            )
            .await?;
        }
        if !json && quiet == 0 {
            println!("\n{} item(s), {} change(s)", item_count, total_changes);
        }
        return Ok(());
    }

    let file_target = if args.target == "metadata" {
        String::new()
    } else {
        args.target.clone()
    };

    // Concurrent batch processing with JoinSet + Semaphore + RateLimiter
    let semaphore = Arc::new(Semaphore::new(jobs));
    let rate_limiter = RateLimiter::new();

    let mut set = JoinSet::new();

    for (identifier, changes) in work_items {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let op = op.clone();
        let target = args.target.clone();
        let reduced_priority = args.reduced_priority;

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let start = std::time::Instant::now();
            let req = ModifyRequest {
                identifier: identifier.clone(),
                changes,
                op,
                target,
                expect: None,
                priority: Some(priority),
                reduced_priority,
            };

            // Retry loop for 429 rate limiting
            let outcome = loop {
                rl.wait_if_paused().await;
                match ia_core::metadata::modify(&client, &req).await {
                    Ok(resp) => break Ok(resp.task_id),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |secs| {
                            eprintln!(
                                "Rate limited. Pausing all workers for {secs}s..."
                            );
                        })
                        .await;
                        // retry
                    }
                    Err(e) => break Err(e.to_string()),
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
            (identifier, outcome, elapsed_ms)
        });
    }

    // Collect results — output is clean because we process one at a time
    let mut error_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) =
            result.context("task panicked")?;

        if record_modify_outcome(
            &identifier,
            &outcome,
            elapsed_ms,
            &file_target,
            quiet,
            joblog.as_ref(),
            json,
        ) {
            error_count += 1;
        }
    }

    if error_count > 0 {
        if json {
            std::process::exit(1);
        }
        bail!("{error_count} of {item_count} item(s) failed");
    }

    Ok(())
}

/// Record the outcome of a modify() call: print output and write joblog entry.
/// Returns `true` if the outcome was an error.
fn record_modify_outcome(
    identifier: &str,
    outcome: &Result<Option<u64>, String>,
    elapsed_ms: u64,
    file_target: &str,
    quiet: u8,
    joblog: Option<&JoblogWriter>,
    json: bool,
) -> bool {
    match outcome {
        Ok(task_id) => {
            if json {
                println!("{}", serde_json::json!({
                    "item": identifier,
                    "status": "ok",
                    "task_id": task_id.unwrap_or(0),
                    "elapsed_ms": elapsed_ms,
                }));
            } else if quiet == 0 {
                println!(
                    "{identifier}: success (task_id: {})",
                    task_id.unwrap_or(0)
                );
            }
            if let Some(jl) = joblog {
                let entry = JoblogEntry::new("modify", identifier, file_target)
                    .ok(0, elapsed_ms);
                jl.write(&entry);
            }
            false
        }
        Err(e) => {
            if json {
                println!("{}", serde_json::json!({
                    "item": identifier,
                    "status": "error",
                    "error": {"code": "metadata_write", "message": e},
                    "elapsed_ms": elapsed_ms,
                }));
            } else if quiet < 2 {
                eprintln!("error: {identifier}: {e}");
            }
            if let Some(jl) = joblog {
                let entry = JoblogEntry::new("modify", identifier, file_target)
                    .error(e, 0);
                jl.write(&entry);
            }
            true
        }
    }
}

/// Collect identifiers from all sources (positional, --itemlist, --search, stdin).
async fn collect_identifiers(args: &MetadataArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = args.identifiers.clone();

    if let Some(ref path) = args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    if let Some(ref query) = args.search {
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // Read from stdin if no identifiers and no other input sources
    if ids.is_empty()
        && args.itemlist.is_none()
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

    Ok(ids)
}
