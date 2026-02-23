use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use futures::StreamExt;
use serde_json::json;

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, MetadataOp, ModifyRequest,
    ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS,
};
use ia_core::search::SearchOpts;
use ia_core::IaClient;

#[derive(Args)]
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
    /// Target: "metadata" (default) or "files/filename"
    #[arg(long, default_value = "metadata")]
    pub target: String,

    /// Server-side concurrency check: --expect="field:expected_value" (repeatable)
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

    /// Use search results as input
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

    run_write(client, &args, quiet, joblog_path).await
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
        if !exists {
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
        for fmt in formats {
            println!("{fmt}");
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
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Spreadsheet mode: read records and process each as a modify() call
    if let Some(ref spreadsheet_path) = args.spreadsheet {
        return run_spreadsheet(client, args, spreadsheet_path, quiet, joblog_path).await;
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

    // Process each identifier
    let mut error_count = 0usize;

    if args.dry_run && quiet == 0 {
        println!("Dry run -- no changes will be applied\n");
    }

    let mut total_dry_run_changes = 0usize;

    for identifier in &identifiers {
        if args.dry_run {
            for (changes, batch_op) in &change_groups {
                total_dry_run_changes += run_dry_run(
                    client,
                    identifier,
                    changes,
                    batch_op,
                    &args.target,
                    expect.as_ref(),
                    quiet,
                )
                .await?;
            }
            continue;
        }

        let start = std::time::Instant::now();
        let file_target = if args.target == "metadata" {
            ""
        } else {
            &args.target
        };

        // Process all change groups for this identifier.
        // For non-insert ops, there is exactly one group. For insert ops,
        // each --insert arg is a separate group with its own index.
        let mut batch_error = None;
        let mut last_task_id = None;

        for (changes, batch_op) in &change_groups {
            let req = ModifyRequest {
                identifier: identifier.clone(),
                changes: changes.clone(),
                op: batch_op.clone(),
                target: args.target.clone(),
                expect: expect.clone(),
                priority: Some(priority),
                reduced_priority: args.reduced_priority,
            };
            let result = ia_core::metadata::modify(client, &req).await;

            match result {
                Ok(resp) => {
                    last_task_id = resp.task_id;
                }
                Err(e) => {
                    batch_error = Some(e);
                    break;
                }
            }
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let outcome = match batch_error {
            None => Ok(last_task_id),
            Some(e) => Err(e.to_string()),
        };

        if record_modify_outcome(
            identifier,
            &outcome,
            elapsed_ms,
            file_target,
            quiet,
            joblog.as_ref(),
        ) {
            error_count += 1;
        }
    }

    if args.dry_run && quiet == 0 {
        println!(
            "\n{} item(s), {} change(s)",
            identifiers.len(),
            total_dry_run_changes
        );
    }

    if error_count > 0 {
        bail!(
            "{error_count} of {} item(s) failed",
            identifiers.len()
        );
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
async fn run_dry_run(
    client: &IaClient,
    identifier: &str,
    changes: &[(String, serde_json::Value)],
    op: &MetadataOp,
    target: &str,
    expect: Option<&HashMap<String, serde_json::Value>>,
    quiet: u8,
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

    if quiet == 0 {
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
    joblog_path: Option<PathBuf>,
) -> Result<()> {
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

    if args.dry_run && quiet == 0 {
        println!("Dry run -- no changes will be applied\n");
    }

    let mut error_count = 0usize;
    let mut item_count = 0usize;
    let mut total_changes = 0usize;

    for (identifier, fields) in &records {
        let changes: Vec<(String, serde_json::Value)> = fields
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();

        if changes.is_empty() {
            continue;
        }

        item_count += 1;

        if args.dry_run {
            total_changes += run_dry_run(
                client,
                identifier,
                &changes,
                &op,
                &args.target,
                None,
                quiet,
            )
            .await?;
            continue;
        }

        let start = std::time::Instant::now();
        let req = ModifyRequest {
            identifier: identifier.clone(),
            changes,
            op: op.clone(),
            target: args.target.clone(),
            expect: None,
            priority: Some(priority),
            reduced_priority: args.reduced_priority,
        };
        let result = ia_core::metadata::modify(client, &req).await;

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let file_target = if args.target == "metadata" {
            ""
        } else {
            &args.target
        };
        let outcome = result
            .as_ref()
            .map(|r| r.task_id)
            .map_err(|e| e.to_string());

        if record_modify_outcome(
            identifier,
            &outcome,
            elapsed_ms,
            file_target,
            quiet,
            joblog.as_ref(),
        ) {
            error_count += 1;
        }
    }

    if args.dry_run && quiet == 0 {
        println!("\n{} item(s), {} change(s)", item_count, total_changes);
    }

    if error_count > 0 {
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
) -> bool {
    match outcome {
        Ok(task_id) => {
            if quiet == 0 {
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
            if quiet < 2 {
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
