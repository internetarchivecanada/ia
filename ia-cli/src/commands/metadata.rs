use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, MetadataOp, ModifyRequest,
    ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS, REMOVE_TAG,
};
use ia_core::rate_limit::RateLimiter;
use ia_core::search::SearchOpts;
use ia_core::{IaClient, IaError};

// ─── Shared arg structs ──────────────────────────────────────────────────────

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

// ─── Subcommands ─────────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    /// Bulk export metadata to stdout or file
    #[command(
        long_about = "Export metadata for multiple items. Outputs JSONL to stdout by default, \
            or writes to a file in CSV, TSV, XLSX, or JSONL format (inferred from extension).\n\n\
            In file mode, multi-value fields are expanded into indexed columns: \
            subject[0], subject[1], etc. Single-element arrays use a bare column name. \
            These columns round-trip correctly with 'ia metadata import'.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Export search results as JSONL</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
             \n\n  <dim># Export to CSV file</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.csv</bold>\
             \n\n  <dim># Export to XLSX for editing, then re-import</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.xlsx</bold>\
             \n  <bold>$ ia metadata import data.xlsx --dry-run</bold>\n"
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
    Modify(WriteSubArgs),

    /// Append text to string metadata fields
    #[command(
        long_about = "Append text to the end of string metadata fields. \
            Use -m/--metadata to specify field:value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Append to description</dim>\n  <bold>$ ia metadata append myitem -m \"description:Additional info.\"</bold>\n"
        ),
    )]
    Append(WriteSubArgs),

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
    AppendList(WriteSubArgs),

    /// Insert values at a position in list metadata fields
    #[command(
        long_about = "Insert values at a specific index in list-type metadata fields. \
            Use -m/--metadata with field[index]:value syntax.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Insert at beginning of subject list</dim>\n  <bold>$ ia metadata insert myitem -m \"subject[0]:first-tag\"</bold>\n"
        ),
    )]
    Insert(WriteSubArgs),

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
    Remove(WriteSubArgs),

    /// Bulk write metadata from a spreadsheet or data file
    #[command(
        long_about = "Import metadata changes from a CSV, TSV, XLSX, ODS, or JSONL file. \
            The file must have an 'identifier' column. All other columns are metadata fields.\n\n\
            By default, columns are treated as modify (set/replace) operations. \
            Use column prefixes for other operations:\n\
            \x20 append:field       — append text to string field\n\
            \x20 append-list:field  — append value to list field\n\
            \x20 insert:field[N]    — insert at index N in list\n\
            \x20 remove:field       — remove value from field\n\n\
            Multi-value fields use indexed columns: subject[0], subject[1], etc. \
            These are merged into an array and set as a whole (not per-index). \
            A single indexed column (e.g. only subject[0]) is treated as a bare field. \
            Use REMOVE_TAG as a value to delete entries or entire fields.\n\n\
            Empty cells are skipped — they do not modify the field.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Import from CSV</dim>\n  <bold>$ ia metadata import data.csv</bold>\
             \n\n  <dim># Preview changes</dim>\n  <bold>$ ia metadata import data.xlsx --dry-run</bold>\
             \n\n  <dim># CSV with mixed operations</dim>\n  <bold>$ cat data.csv</bold>\
             \n  <dim>identifier,title,append-list:subject,remove:subject</dim>\
             \n  <dim>myitem,New Title,astronomy,old_tag</dim>\n"
        ),
    )]
    Import(ImportArgs),
}

// ─── Per-subcommand arg structs ──────────────────────────────────────────────

/// Shared args for all write subcommands (modify, append, append-list, insert, remove).
#[derive(Debug, Args)]
pub struct WriteSubArgs {
    #[command(flatten)]
    pub input: BatchInput,

    #[command(flatten)]
    pub write: WriteOpts,
}

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

// ─── Top-level struct ────────────────────────────────────────────────────────

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

// ─── Main dispatch ───────────────────────────────────────────────────────────

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
            run_write(client, sub.input, sub.write, MetadataOp::Set, quiet, jobs, joblog_path)
                .await
        }
        Some(MetadataCommand::Append(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::Append,
                quiet,
                jobs,
                joblog_path,
            )
            .await
        }
        Some(MetadataCommand::AppendList(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::AppendList,
                quiet,
                jobs,
                joblog_path,
            )
            .await
        }
        Some(MetadataCommand::Insert(sub)) => {
            run_write_insert(client, sub.input, sub.write, quiet, jobs, joblog_path).await
        }
        Some(MetadataCommand::Remove(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::Remove,
                quiet,
                jobs,
                joblog_path,
            )
            .await
        }
        Some(MetadataCommand::Import(sub)) => {
            run_import(client, sub, quiet, jobs, joblog_path).await
        }
        None => {
            // Bare read mode
            let identifier = args.identifier.ok_or_else(|| {
                anyhow::anyhow!("identifier required. Run 'ia metadata --help' for usage.")
            })?;
            run_read(client, &identifier, args.exists, args.formats, args.pretty, args.json)
                .await
        }
    }
}

// ─── Read ────────────────────────────────────────────────────────────────────

async fn run_read(
    client: &IaClient,
    identifier: &str,
    exists: bool,
    formats: bool,
    pretty: bool,
    json: bool,
) -> Result<()> {
    if exists {
        let item_exists = client
            .item_exists(identifier)
            .await
            .context(format!("failed to check existence of {identifier}"))?;
        if json {
            println!(
                "{}",
                serde_json::json!({"identifier": identifier, "exists": item_exists})
            );
            if !item_exists {
                std::process::exit(1);
            }
        } else if !item_exists {
            std::process::exit(1);
        }
        return Ok(());
    }

    let item = client
        .get_item(identifier)
        .await
        .context(format!("failed to fetch metadata for {identifier}"))?;

    if formats {
        let mut fmts: Vec<String> = item
            .files
            .iter()
            .filter_map(|f| f.format.clone())
            .collect();
        fmts.sort();
        fmts.dedup();
        if json {
            println!("{}", serde_json::to_string(&fmts).unwrap_or_default());
        } else {
            for fmt in fmts {
                println!("{fmt}");
            }
        }
        return Ok(());
    }

    let output = if pretty {
        serde_json::to_string_pretty(&item)?
    } else {
        serde_json::to_string(&item)?
    };
    println!("{output}");

    Ok(())
}

// ─── Export ──────────────────────────────────────────────────────────────────

async fn run_export(client: &IaClient, args: ExportArgs, quiet: u8) -> Result<()> {
    let identifiers = collect_identifiers_from_batch(&args.input, client).await?;
    if identifiers.is_empty() {
        bail!("no identifiers to export");
    }

    // When writing to a file (-o), we collect all records into memory first so we can
    // compute a unified column set across all items. For large exports this may use
    // significant memory; stdout mode streams items one at a time.
    let mut records: Vec<ia_core::spreadsheet::SpreadsheetRecord> = Vec::new();

    for identifier in &identifiers {
        let item = client
            .get_item(identifier)
            .await
            .context(format!("failed to fetch metadata for {identifier}"))?;

        if args.output.is_none() {
            // Stdout mode: output immediately (streaming)
            let output = if args.pretty {
                serde_json::to_string_pretty(&item)?
            } else {
                serde_json::to_string(&item)?
            };
            println!("{output}");
        } else {
            // File mode: flatten metadata into tabular columns.
            // Multi-value fields expand into indexed columns:
            //   subject: ["science", "nasa"] → subject[0]="science", subject[1]="nasa"
            // Single-value fields use the bare field name:
            //   title: "Apollo 11" → title="Apollo 11"
            let metadata_json = serde_json::to_value(&item.metadata)?;
            let mut fields = HashMap::new();
            if let serde_json::Value::Object(map) = metadata_json {
                for (key, value) in map {
                    if key == "identifier" {
                        continue;
                    }
                    match &value {
                        serde_json::Value::String(s) => {
                            if !s.is_empty() {
                                fields.insert(key, s.clone());
                            }
                        }
                        serde_json::Value::Array(arr) if arr.len() == 1 => {
                            // Single-element array: use bare field name
                            let s = match &arr[0] {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            if !s.is_empty() {
                                fields.insert(key, s);
                            }
                        }
                        serde_json::Value::Array(arr) => {
                            for (i, elem) in arr.iter().enumerate() {
                                let s = match elem {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                if !s.is_empty() {
                                    fields.insert(format!("{key}[{i}]"), s);
                                }
                            }
                        }
                        serde_json::Value::Null => {}
                        other => {
                            let s = other.to_string();
                            if !s.is_empty() {
                                fields.insert(key, s);
                            }
                        }
                    }
                }
            }
            records.push((identifier.clone(), fields));
        }
    }

    // Write to file if -o specified
    if let Some(ref path) = args.output {
        ia_core::spreadsheet::write_spreadsheet(path, &records)
            .context(format!("failed to write export file: {}", path.display()))?;

        if quiet < 2 {
            eprintln!("{} item(s) exported to {}", records.len(), path.display());
        }
    } else if quiet < 2 && !args.json && !args.pretty {
        eprintln!("{} item(s) exported", identifiers.len());
    }

    Ok(())
}

// ─── Write ───────────────────────────────────────────────────────────────────

async fn run_write(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    op: MetadataOp,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    if write.metadata.is_empty() {
        bail!("no -m/--metadata values specified");
    }

    // Parse changes
    let changes: Vec<(String, serde_json::Value)> = write
        .metadata
        .iter()
        .map(|s| {
            let (key, value) =
                parse_key_value(s).context(format!("invalid key:value format: {s:?}"))?;
            Ok((key, json!(value)))
        })
        .collect::<Result<Vec<_>>>()?;

    // Build change groups (single group for non-Insert ops)
    let change_groups = vec![(changes, op)];

    run_write_inner(client, input, write, change_groups, quiet, jobs, joblog_path).await
}

async fn run_write_insert(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    if write.metadata.is_empty() {
        bail!("no -m/--metadata values specified");
    }

    // Each --metadata arg gets its own group with its own index
    let change_groups: Vec<(Vec<(String, serde_json::Value)>, MetadataOp)> = write
        .metadata
        .iter()
        .map(|s| {
            let (key, value) =
                parse_key_value(s).context(format!("invalid key:value format: {s:?}"))?;
            let (field, index) = parse_indexed_key(&key).unwrap_or((key, 0));
            Ok((vec![(field, json!(value))], MetadataOp::Insert(index)))
        })
        .collect::<Result<Vec<_>>>()?;

    run_write_inner(client, input, write, change_groups, quiet, jobs, joblog_path).await
}

#[allow(clippy::too_many_arguments)]
async fn run_write_inner(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    change_groups: Vec<(Vec<(String, serde_json::Value)>, MetadataOp)>,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
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
    let expect: Option<HashMap<String, serde_json::Value>> = if !write.expect.is_empty() {
        let mut map = HashMap::new();
        for s in &write.expect {
            let (key, value) =
                parse_key_value(s).context(format!("invalid expect key:value: {s:?}"))?;
            map.insert(key, json!(value));
        }
        Some(map)
    } else {
        None
    };

    let json = write.json;

    // Collect identifiers
    let identifiers = collect_identifiers_from_batch(&input, client).await?;
    if identifiers.is_empty() {
        bail!("no identifiers provided");
    }

    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let priority = write
        .priority
        .unwrap_or(if identifiers.len() > 1 { -5 } else { 0 });

    // Dry-run
    if write.dry_run {
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
                    &write.target,
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

    let file_target = if write.target == "metadata" {
        String::new()
    } else {
        write.target.clone()
    };

    // Concurrent batch processing
    let semaphore = Arc::new(Semaphore::new(jobs));
    let rate_limiter = RateLimiter::new();
    let mut set = JoinSet::new();
    let total_count = identifiers.len();

    for identifier in identifiers {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let change_groups = change_groups.clone();
        let target = write.target.clone();
        let expect = expect.clone();
        let reduced_priority = write.reduced_priority;

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = std::time::Instant::now();
            let mut batch_error = None;
            let mut last_task_id = None;

            'groups: for (changes, batch_op) in &change_groups {
                let req = ModifyRequest {
                    identifier: identifier.clone(),
                    changes: changes.clone(),
                    op: batch_op.clone(),
                    target: target.clone(),
                    expect: expect.clone(),
                    priority: Some(priority),
                    reduced_priority,
                };

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
                        }
                        Err(e) => {
                            batch_error = Some(e);
                            break 'groups;
                        }
                    }
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

    let mut error_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) = result.context("task panicked")?;

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

// ─── Import ──────────────────────────────────────────────────────────────────

/// Parse column prefixes for import mode.
fn parse_column_op(column_name: &str) -> Result<(MetadataOp, String)> {
    let (op, field) = if let Some(field) = column_name.strip_prefix("append-list:") {
        (MetadataOp::AppendList, field.to_string())
    } else if let Some(field) = column_name.strip_prefix("append:") {
        (MetadataOp::Append, field.to_string())
    } else if let Some(field) = column_name.strip_prefix("remove:") {
        (MetadataOp::Remove, field.to_string())
    } else if let Some(rest) = column_name.strip_prefix("insert:") {
        if let Some((field, idx)) = parse_indexed_key(rest) {
            (MetadataOp::Insert(idx), field)
        } else {
            (MetadataOp::Insert(0), rest.to_string())
        }
    } else {
        // Default: modify (set)
        return Ok((MetadataOp::Set, column_name.to_string()));
    };

    if field.is_empty() {
        bail!("column prefix '{column_name}' has no field name after the colon");
    }

    Ok((op, field))
}

/// Merge indexed bare columns (e.g. `subject[0]`, `subject[1]`) into single
/// array-valued entries. Matches the Python `ia` CSV convention where
/// multi-value fields use indexed columns that combine into an array on import.
///
/// Non-indexed columns and prefixed columns (e.g. `append:subject`) pass
/// through unchanged. Merged entries appear at the position of their first
/// indexed column.
fn merge_indexed_columns(
    fields: &HashMap<String, String>,
) -> Vec<(String, serde_json::Value)> {
    let mut resolved: Vec<(String, serde_json::Value)> = Vec::new();
    let mut indexed: HashMap<String, Vec<(usize, String)>> = HashMap::new();

    for (col_name, value) in fields {
        // Only bare columns (no op-prefix) can be indexed
        if !col_name.contains(':') {
            if let Some((base_field, idx)) = parse_indexed_key(col_name) {
                indexed
                    .entry(base_field)
                    .or_default()
                    .push((idx, value.clone()));
                continue;
            }
        }
        resolved.push((col_name.clone(), json!(value)));
    }

    // Append merged indexed fields as array values.
    // - Single index (e.g. only subject[0]) → bare scalar (same as unindexed)
    // - REMOVE_TAG entries are filtered out; if all are REMOVE_TAG, remove field
    for (base_field, mut entries) in indexed {
        entries.sort_by_key(|(idx, _)| *idx);
        let values: Vec<serde_json::Value> = entries
            .into_iter()
            .filter(|(_, v)| v != REMOVE_TAG)
            .map(|(_, v)| json!(v))
            .collect();
        match values.len() {
            0 => {
                // All entries were REMOVE_TAG → delete the field
                resolved.push((base_field, json!(REMOVE_TAG)));
            }
            1 => {
                // Single value → bare scalar (matches export of single-element arrays)
                resolved.push((base_field, values.into_iter().next().unwrap()));
            }
            _ => {
                resolved.push((base_field, serde_json::Value::Array(values)));
            }
        }
    }

    resolved
}

async fn run_import(
    client: &IaClient,
    args: ImportArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let json = args.json;

    let records = ia_core::spreadsheet::read_spreadsheet(&args.file).context(format!(
        "failed to read spreadsheet: {}",
        args.file.display()
    ))?;

    let priority = args.priority.unwrap_or(-5);

    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Build (identifier, change_groups) pairs from records.
    // Each record's columns are parsed for operation prefixes.
    type ChangeGroup = (Vec<(String, serde_json::Value)>, MetadataOp);
    let mut work_items: Vec<(String, Vec<ChangeGroup>)> = Vec::new();
    for (identifier, fields) in &records {
        if fields.is_empty() {
            continue;
        }

        let resolved = merge_indexed_columns(fields);

        // Build change groups from column prefixes.
        // Group consecutive same-op columns together for efficiency,
        // but each distinct op gets its own group.
        let mut groups: Vec<ChangeGroup> = Vec::new();
        for (col_name, value) in &resolved {
            let (op, field) = parse_column_op(col_name)?;
            // Try to merge with last group if same op
            if let Some(last) = groups.last_mut() {
                if last.1 == op {
                    last.0.push((field, value.clone()));
                    continue;
                }
            }
            groups.push((vec![(field, value.clone())], op));
        }

        if !groups.is_empty() {
            work_items.push((identifier.clone(), groups));
        }
    }

    let item_count = work_items.len();

    // Dry-run
    if args.dry_run {
        if !json && quiet == 0 {
            println!("Dry run -- no changes will be applied\n");
        }
        let mut total_changes = 0usize;
        for (identifier, groups) in &work_items {
            for (changes, op) in groups {
                total_changes += run_dry_run(
                    client,
                    identifier,
                    changes,
                    op,
                    &args.target,
                    None,
                    quiet,
                    json,
                )
                .await?;
            }
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

    // Concurrent batch processing
    let semaphore = Arc::new(Semaphore::new(jobs));
    let rate_limiter = RateLimiter::new();
    let mut set = JoinSet::new();

    for (identifier, groups) in work_items {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let target = args.target.clone();
        let reduced_priority = args.reduced_priority;

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = std::time::Instant::now();
            let mut batch_error = None;
            let mut last_task_id = None;

            'groups: for (changes, op) in &groups {
                let req = ModifyRequest {
                    identifier: identifier.clone(),
                    changes: changes.clone(),
                    op: op.clone(),
                    target: target.clone(),
                    expect: None,
                    priority: Some(priority),
                    reduced_priority,
                };

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
                        }
                        Err(e) => {
                            batch_error = Some(e);
                            break 'groups;
                        }
                    }
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

    let mut error_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) = result.context("task panicked")?;

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

// ─── Shared helpers ──────────────────────────────────────────────────────────

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
            println!(
                "{}",
                serde_json::json!({
                    "item": identifier,
                    "status": "no_changes",
                    "dry_run": true,
                })
            );
        } else {
            let changes: Vec<serde_json::Value> = patch
                .iter()
                .filter(|p| p["op"].as_str() != Some("test"))
                .cloned()
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "item": identifier,
                    "status": "would_modify",
                    "dry_run": true,
                    "changes": changes,
                })
            );
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
                println!(
                    "{}",
                    serde_json::json!({
                        "item": identifier,
                        "status": "ok",
                        "task_id": task_id.unwrap_or(0),
                        "elapsed_ms": elapsed_ms,
                    })
                );
            } else if quiet == 0 {
                println!(
                    "{identifier}: success (task_id: {})",
                    task_id.unwrap_or(0)
                );
            }
            if let Some(jl) = joblog {
                let entry =
                    JoblogEntry::new("modify", identifier, file_target).ok(0, elapsed_ms);
                jl.write(&entry);
            }
            false
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "item": identifier,
                        "status": "error",
                        "error": {"code": "metadata_write", "message": e},
                        "elapsed_ms": elapsed_ms,
                    })
                );
            } else if quiet < 2 {
                eprintln!("error: {identifier}: {e}");
            }
            if let Some(jl) = joblog {
                let entry = JoblogEntry::new("modify", identifier, file_target).error(e, 0);
                jl.write(&entry);
            }
            true
        }
    }
}

/// Collect identifiers from BatchInput sources (positional, --itemlist, --search, stdin).
async fn collect_identifiers_from_batch(
    input: &BatchInput,
    client: &IaClient,
) -> Result<Vec<String>> {
    let mut ids = input.identifiers.clone();

    if let Some(ref path) = input.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    if let Some(ref query) = input.search {
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // Read from stdin if no identifiers and no other input sources
    if ids.is_empty()
        && input.itemlist.is_none()
        && input.search.is_none()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ia_core::metadata::write::MetadataOp;

    #[test]
    fn parse_column_op_default_is_set() {
        let (op, field) = parse_column_op("title").unwrap();
        assert_eq!(op, MetadataOp::Set);
        assert_eq!(field, "title");
    }

    #[test]
    fn parse_column_op_append_prefix() {
        let (op, field) = parse_column_op("append:description").unwrap();
        assert_eq!(op, MetadataOp::Append);
        assert_eq!(field, "description");
    }

    #[test]
    fn parse_column_op_append_list_prefix() {
        let (op, field) = parse_column_op("append-list:subject").unwrap();
        assert_eq!(op, MetadataOp::AppendList);
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_remove_prefix() {
        let (op, field) = parse_column_op("remove:subject").unwrap();
        assert_eq!(op, MetadataOp::Remove);
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_insert_with_index() {
        let (op, field) = parse_column_op("insert:subject[2]").unwrap();
        assert_eq!(op, MetadataOp::Insert(2));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_insert_without_index() {
        let (op, field) = parse_column_op("insert:subject").unwrap();
        assert_eq!(op, MetadataOp::Insert(0));
        assert_eq!(field, "subject");
    }

    #[test]
    fn parse_column_op_empty_field_after_prefix_errors() {
        assert!(parse_column_op("append:").is_err());
        assert!(parse_column_op("remove:").is_err());
        assert!(parse_column_op("append-list:").is_err());
        assert!(parse_column_op("insert:").is_err());
    }

    #[test]
    fn parse_column_op_colon_in_field_name() {
        // "some:random:field" doesn't match any known prefix, so it falls through to Set
        let (op, field) = parse_column_op("some:random:field").unwrap();
        assert_eq!(op, MetadataOp::Set);
        assert_eq!(field, "some:random:field");
    }

    #[test]
    fn merge_indexed_columns_combines_into_array() {
        let fields: HashMap<String, String> = [
            ("title".into(), "Apollo 11".into()),
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "nasa".into()),
            ("description".into(), "Moon landing".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        // 3 entries: title, description, and subject (merged)
        assert_eq!(merged.len(), 3);
        // Find each by field name since HashMap order is non-deterministic
        let subject = merged.iter().find(|(k, _)| k == "subject").unwrap();
        assert_eq!(subject.1, json!(["science", "nasa"]));
        let title = merged.iter().find(|(k, _)| k == "title").unwrap();
        assert_eq!(title.1, json!("Apollo 11"));
        let desc = merged.iter().find(|(k, _)| k == "description").unwrap();
        assert_eq!(desc.1, json!("Moon landing"));
    }

    #[test]
    fn merge_indexed_columns_sorts_by_index() {
        let fields: HashMap<String, String> = [
            ("subject[2]".into(), "history".into()),
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "nasa".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0],
            ("subject".into(), json!(["science", "nasa", "history"]))
        );
    }

    #[test]
    fn merge_indexed_columns_preserves_prefixed_columns() {
        let fields: HashMap<String, String> = [
            ("append:subject".into(), "new-tag".into()),
            ("title".into(), "Test".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|(k, v)| k == "append:subject" && v == &json!("new-tag")));
        assert!(merged.iter().any(|(k, v)| k == "title" && v == &json!("Test")));
    }

    #[test]
    fn merge_indexed_columns_single_index_becomes_scalar() {
        // Single indexed column treated as bare field (matches export behavior)
        let fields: HashMap<String, String> =
            [("subject[0]".into(), "science".into())].into_iter().collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], ("subject".into(), json!("science")));
    }

    #[test]
    fn merge_indexed_columns_remove_tag_filters_entries() {
        let fields: HashMap<String, String> = [
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "REMOVE_TAG".into()),
            ("subject[2]".into(), "nasa".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0],
            ("subject".into(), json!(["science", "nasa"]))
        );
    }

    #[test]
    fn merge_indexed_columns_all_remove_tag_deletes_field() {
        let fields: HashMap<String, String> = [
            ("subject[0]".into(), "REMOVE_TAG".into()),
            ("subject[1]".into(), "REMOVE_TAG".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        // Emits REMOVE_TAG sentinel so MetadataOp::Set removes the field
        assert_eq!(merged[0], ("subject".into(), json!("REMOVE_TAG")));
    }
}
