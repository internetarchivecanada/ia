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

use comfy_table::{Cell, Color, Table};

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, ChangeGroup,
    CompoundModifyRequest, MetadataOp, ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS, REMOVE_TAG,
};
use ia_core::metadata::{fetch_schema, SchemaField};
use ia_core::rate_limit::RateLimiter;
use ia_core::search::SearchOpts;
use ia_core::{IaClient, IaError};

// ─── Filter enums ────────────────────────────────────────────────────────────

/// Filter values for --defined-by flag
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DefinedByFilter {
    Uploader,
    #[value(name = "ia-admin")]
    IaAdmin,
    #[value(name = "ia-software")]
    IaSoftware,
    #[value(name = "user-admin")]
    UserAdmin,
}

impl DefinedByFilter {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Uploader => value == "uploader",
            Self::IaAdmin => value == "IA admin",
            Self::IaSoftware => value == "IA software",
            Self::UserAdmin => value == "user admin",
        }
    }
}

/// Filter values for --edit-access flag
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum EditAccessFilter {
    Uploader,
    #[value(name = "ia-admin")]
    IaAdmin,
    #[value(name = "ia-software")]
    IaSoftware,
    #[value(name = "user-admin")]
    UserAdmin,
    #[value(name = "not-editable")]
    NotEditable,
}

impl EditAccessFilter {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Uploader => value == "uploader",
            Self::IaAdmin => value == "IA admin",
            Self::IaSoftware => value == "IA software",
            Self::UserAdmin => value == "user admin",
            Self::NotEditable => value == "not editable",
        }
    }
}

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
            Use -m/--metadata to specify field:value pairs.\n\n\
            Chain multiple operations with + to apply them in a single request:\n  \
            ia metadata modify ID -m field:val + remove -m field:val\n\n\
            Valid operations after +: modify, append, append-list, insert, remove.\n\
            Shared options (--target, --dry-run, --json, etc.) go before the first +.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Set title</dim>\n  <bold>$ ia metadata modify myitem -m \"title:New Title\"</bold>\
             \n\n  <dim># Set multiple fields</dim>\n  <bold>$ ia metadata modify myitem -m \"title:X\" -m \"date:2024\"</bold>\
             \n\n  <dim># Batch modify via search</dim>\n  <bold>$ ia metadata modify --search \"collection:test\" -m \"subject:updated\"</bold>\
             \n\n  <dim># Compound: set title and remove a subject in one request</dim>\
             \n  <bold>$ ia metadata modify myitem -m \"title:New\" + remove -m \"subject:old-tag\"</bold>\
             \n\n  <dim># Compound: modify + insert at position + append-list</dim>\
             \n  <bold>$ ia metadata modify myitem -m \"title:New\" + insert -m \"collection[0]:featured\" + append-list -m \"subject:physics\"</bold>\n"
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

    /// Look up Internet Archive metadata field definitions
    #[command(
        long_about = "Look up Internet Archive metadata field definitions. Shows a table of \
            all user-facing fields by default, or detailed info for a specific field.\n\n\
            The schema is fetched live from the ia-metadata item on archive.org.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># List all user-facing metadata fields</dim>\n  <bold>$ ia metadata schema</bold>\
             \n\n  <dim># Look up a specific field</dim>\n  <bold>$ ia metadata schema title</bold>\
             \n\n  <dim># Show file-level schema</dim>\n  <bold>$ ia metadata schema --files</bold>\
             \n\n  <dim># Show required fields only</dim>\n  <bold>$ ia metadata schema --required</bold>\
             \n\n  <dim># Include internal fields</dim>\n  <bold>$ ia metadata schema --internal</bold>\
             \n\n  <dim># Machine-readable output</dim>\n  <bold>$ ia metadata schema --json</bold>\n"
        ),
    )]
    Schema(SchemaArgs),
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

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// Field name to look up (shows detailed view)
    pub field: Option<String>,

    /// Show file-level schema instead of item-level
    #[arg(short = 'f', long)]
    pub files: bool,

    /// Include internal-use-only fields (hidden by default)
    #[arg(long)]
    pub internal: bool,

    /// Only show required or recommended fields
    #[arg(long)]
    pub required: bool,

    /// Only show repeatable fields
    #[arg(long)]
    pub repeatable: bool,

    /// Filter by who defines the field
    #[arg(long)]
    pub defined_by: Option<DefinedByFilter>,

    /// Filter by who can edit the field
    #[arg(long)]
    pub edit_access: Option<EditAccessFilter>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Top-level struct ────────────────────────────────────────────────────────

#[derive(Args)]
#[command(
    long_about = "Read or modify Internet Archive item metadata. Shows metadata as JSON \
        by default. Use subcommands for write operations, bulk export, or bulk import.\n\n\
        Chain multiple write operations with + for a single HTTP request:\n  \
        ia metadata modify ID -m field:val + remove -m field:val",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Show item metadata</dim>\n  <bold>$ ia metadata nasa</bold>\
         \n\n  <dim># Check if item exists</dim>\n  <bold>$ ia metadata nasa --exists</bold>\
         \n\n  <dim># Modify metadata</dim>\n  <bold>$ ia metadata modify nasa -m \"title:New\"</bold>\
         \n\n  <dim># Compound operations (single request)</dim>\
         \n  <bold>$ ia metadata modify nasa -m \"title:New\" + remove -m \"subject:old\"</bold>\
         \n\n  <dim># Bulk export</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
         \n\n  <dim># Bulk import</dim>\n  <bold>$ ia metadata import data.csv</bold>\
         \n\n  <dim># Browse metadata field definitions</dim>\n  <bold>$ ia metadata schema</bold>\
         \n  <bold>$ ia metadata schema title</bold>\n"
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

// ─── Runtime context ─────────────────────────────────────────────────────────

/// Runtime context for write operations — groups parameters that are
/// always passed together through the write call chain.
struct WriteContext {
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
}

// ─── Main dispatch ───────────────────────────────────────────────────────────

pub async fn run(
    client: &IaClient,
    args: MetadataArgs,
    continuations: Option<Vec<(String, Vec<String>)>>,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let ctx = WriteContext {
        quiet,
        jobs,
        joblog_path,
    };

    match args.command {
        Some(MetadataCommand::Export(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with export");
            }
            run_export(client, sub, ctx.quiet).await
        }
        Some(MetadataCommand::Modify(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Set, continuations, &ctx).await
        }
        Some(MetadataCommand::Append(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Append, continuations, &ctx).await
        }
        Some(MetadataCommand::AppendList(sub)) => {
            run_write(
                client, sub.input, sub.write, MetadataOp::AppendList, continuations, &ctx,
            )
            .await
        }
        Some(MetadataCommand::Insert(sub)) => {
            run_write_insert(client, sub.input, sub.write, continuations, &ctx).await
        }
        Some(MetadataCommand::Remove(sub)) => {
            run_write(client, sub.input, sub.write, MetadataOp::Remove, continuations, &ctx).await
        }
        Some(MetadataCommand::Import(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with import");
            }
            run_import(client, sub, &ctx).await
        }
        Some(MetadataCommand::Schema(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with schema");
            }
            run_schema(client, sub).await
        }
        None => {
            if continuations.is_some() {
                bail!("compound operations (+) require a write subcommand (modify, append, etc.)");
            }
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

// ─── Schema ──────────────────────────────────────────────────────────────────

fn filter_schema_fields<'a>(fields: &'a [SchemaField], args: &SchemaArgs) -> Vec<&'a SchemaField> {
    fields
        .iter()
        .filter(|f| args.internal || f.internal_use_only != "Yes")
        .filter(|f| !args.required || f.required == "Yes" || f.required == "Recommended")
        .filter(|f| !args.repeatable || f.repeatable == "Yes")
        .filter(|f| {
            args.defined_by
                .as_ref()
                .map_or(true, |db| db.matches(&f.defined_by))
        })
        .filter(|f| {
            args.edit_access
                .as_ref()
                .map_or(true, |ea| ea.matches(&f.edit_access))
        })
        .collect()
}

fn print_schema_detail(field: &SchemaField) {
    println!("{}", field.field);
    println!("  Label:           {}", field.label);
    println!("  Required:        {}", field.required);
    println!("  Repeatable:      {}", field.repeatable);
    println!("  Internal:        {}", field.internal_use_only);
    println!("  Defined by:      {}", field.defined_by);
    println!("  Edit access:     {}", field.edit_access);
    if !field.definition.is_empty() {
        println!("  Definition:      {}", field.definition);
    }
    if !field.accepted_values.is_empty() {
        println!("  Accepted values: {}", field.accepted_values);
    }
    if !field.usage_notes.is_empty() {
        println!("  Usage notes:     {}", field.usage_notes);
    }
    if !field.example.is_empty() {
        println!("  Example:         {}", field.example.join(", "));
    }
}

fn print_schema_table(fields: &[&SchemaField]) {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(vec![
        Cell::new("FIELD").fg(Color::Cyan),
        Cell::new("LABEL").fg(Color::Cyan),
        Cell::new("REQUIRED").fg(Color::Cyan),
        Cell::new("REPEATABLE").fg(Color::Cyan),
    ]);
    for f in fields {
        table.add_row(vec![&f.field, &f.label, &f.required, &f.repeatable]);
    }
    println!("{table}");
}

async fn run_schema(client: &IaClient, args: SchemaArgs) -> Result<()> {
    let data = fetch_schema(client).await?;
    let source = if args.files {
        &data.files_schema
    } else {
        &data.metadata_schema
    };

    // Single-field detail mode
    if let Some(ref field_name) = args.field {
        let found = source.iter().find(|f| f.field == *field_name);
        match found {
            Some(field) => {
                if args.json {
                    let json = serde_json::to_string_pretty(field)?;
                    println!("{json}");
                } else {
                    print_schema_detail(field);
                }
            }
            None => {
                let suggestions: Vec<&str> = source
                    .iter()
                    .filter(|f| {
                        f.field.contains(field_name.as_str())
                            || field_name.contains(&f.field)
                    })
                    .map(|f| f.field.as_str())
                    .take(5)
                    .collect();
                let mut msg = format!("field '{}' not found in schema", field_name);
                if !suggestions.is_empty() {
                    msg.push_str(&format!(
                        ". Did you mean: {}?",
                        suggestions.join(", ")
                    ));
                }
                bail!("{msg}");
            }
        }
        return Ok(());
    }

    let filtered = filter_schema_fields(source, &args);

    if args.json {
        let json = serde_json::to_string_pretty(&filtered)?;
        println!("{json}");
    } else {
        print_schema_table(&filtered);
    }

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

async fn run_write(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    op: MetadataOp,
    continuations: Option<Vec<(String, Vec<String>)>>,
    ctx: &WriteContext,
) -> Result<()> {
    if write.metadata.is_empty() && continuations.is_none() {
        bail!("no -m/--metadata values specified");
    }

    // Parse primary changes
    let changes: Vec<(String, serde_json::Value)> = write
        .metadata
        .iter()
        .map(|s| {
            let (key, value) =
                parse_key_value(s).context(format!("invalid key:value format: {s:?}"))?;
            Ok((key, json!(value)))
        })
        .collect::<Result<Vec<_>>>()?;

    // Build change groups: primary + continuations
    let mut change_groups = if changes.is_empty() {
        vec![]
    } else {
        vec![ChangeGroup { changes, op }]
    };
    change_groups.extend(build_continuation_groups(continuations)?);

    run_write_inner(client, input, write, change_groups, ctx).await
}

async fn run_write_insert(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    continuations: Option<Vec<(String, Vec<String>)>>,
    ctx: &WriteContext,
) -> Result<()> {
    if write.metadata.is_empty() && continuations.is_none() {
        bail!("no -m/--metadata values specified");
    }

    // Each --metadata arg gets its own group with its own index
    let mut change_groups: Vec<ChangeGroup> = write
        .metadata
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
        .collect::<Result<Vec<_>>>()?;

    // Add continuations
    change_groups.extend(build_continuation_groups(continuations)?);

    run_write_inner(client, input, write, change_groups, ctx).await
}

async fn run_write_inner(
    client: &IaClient,
    input: BatchInput,
    write: WriteOpts,
    change_groups: Vec<ChangeGroup>,
    ctx: &WriteContext,
) -> Result<()> {
    // Warn about immutable/admin-only fields
    for group in &change_groups {
        for (key, _) in &group.changes {
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

    let joblog = ctx
        .joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let priority = write
        .priority
        .unwrap_or(if identifiers.len() > 1 { -5 } else { 0 });

    // Dry-run: uses compute_compound_patch for a combined diff
    if write.dry_run {
        if !json && ctx.quiet == 0 {
            println!("Dry run -- no changes will be applied\n");
        }
        let mut total_dry_run_changes = 0usize;
        for identifier in &identifiers {
            total_dry_run_changes += run_dry_run_compound(
                client,
                identifier,
                &change_groups,
                &write.target,
                expect.as_ref(),
                ctx.quiet,
                json,
            )
            .await?;
        }
        if !json && ctx.quiet == 0 {
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

    // Concurrent batch processing — single POST per item via modify_compound
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
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

            let compound_req = CompoundModifyRequest {
                identifier: identifier.clone(),
                groups: change_groups,
                target,
                expect,
                priority: Some(priority),
                reduced_priority,
            };

            let outcome = loop {
                rl.wait_if_paused().await;
                match ia_core::metadata::modify_compound(&client, &compound_req).await {
                    Ok(resp) => break Ok(resp.task_id),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |secs| {
                            eprintln!("Rate limited. Pausing all workers for {secs}s...");
                        })
                        .await;
                    }
                    Err(e) => break Err(e.to_string()),
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
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
            ctx.quiet,
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

async fn run_import(client: &IaClient, args: ImportArgs, ctx: &WriteContext) -> Result<()> {
    let json = args.json;

    let records = ia_core::spreadsheet::read_spreadsheet(&args.file).context(format!(
        "failed to read spreadsheet: {}",
        args.file.display()
    ))?;

    let priority = args.priority.unwrap_or(-5);

    let joblog = ctx
        .joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Build (identifier, change_groups) pairs from records.
    // Each record's columns are parsed for operation prefixes.
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
                if last.op == op {
                    last.changes.push((field, value.clone()));
                    continue;
                }
            }
            groups.push(ChangeGroup {
                changes: vec![(field, value.clone())],
                op,
            });
        }

        if !groups.is_empty() {
            work_items.push((identifier.clone(), groups));
        }
    }

    let item_count = work_items.len();

    // Dry-run
    if args.dry_run {
        if !json && ctx.quiet == 0 {
            println!("Dry run -- no changes will be applied\n");
        }
        let mut total_changes = 0usize;
        for (identifier, groups) in &work_items {
            total_changes += run_dry_run_compound(
                client,
                identifier,
                groups,
                &args.target,
                None,
                ctx.quiet,
                json,
            )
            .await?;
        }
        if !json && ctx.quiet == 0 {
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
    let semaphore = Arc::new(Semaphore::new(ctx.jobs));
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

            let compound_req = CompoundModifyRequest {
                identifier: identifier.clone(),
                groups,
                target,
                expect: None,
                priority: Some(priority),
                reduced_priority,
            };

            let outcome = loop {
                rl.wait_if_paused().await;
                match ia_core::metadata::modify_compound(&client, &compound_req).await {
                    Ok(resp) => break Ok(resp.task_id),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |secs| {
                            eprintln!("Rate limited. Pausing all workers for {secs}s...");
                        })
                        .await;
                    }
                    Err(e) => break Err(e.to_string()),
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
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
            ctx.quiet,
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

/// Dry-run: fetch metadata, compute compound patch, display without writing.
/// Returns the number of non-test patch operations.
async fn run_dry_run_compound(
    client: &IaClient,
    identifier: &str,
    groups: &[ChangeGroup],
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

    let patch =
        ia_core::metadata::compute_compound_patch(&source, groups, expect, identifier)?;

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

// ─── Compound arg splitting ───────────────────────────────────────────────────

/// Valid operation names for compound continuations.
const VALID_OPS: &[&str] = &["modify", "append", "append-list", "insert", "remove"];

/// Shared options that must appear only in the first segment (before any +).
/// These are write-subcommand options that apply to the entire operation.
const SHARED_OPTIONS: &[&str] = &[
    "--target",
    "--expect",
    "--priority",
    "--reduced-priority",
    "--dry-run",
    "--json",
    "--pretty",
    "--itemlist",
    "--search",
];

/// Result of splitting compound args: the primary args (for clap) and continuations.
#[derive(Debug)]
pub struct CompoundSplit {
    /// Args for the primary segment (everything before first +), for clap to parse.
    pub primary_args: Vec<String>,
    /// Continuation segments: (op_name, change_strings).
    pub continuations: Vec<(String, Vec<String>)>,
}

/// Split argv-style args on bare `+` tokens.
///
/// Returns `Ok(None)` if no `+` found (single-op mode).
/// Returns the primary args (for clap) and parsed continuations.
fn split_compound_args(args: &[String]) -> Result<Option<CompoundSplit>> {
    // Check if any bare + exists
    if !args.iter().any(|a| a == "+") {
        return Ok(None);
    }

    // Split into segments
    let mut segments: Vec<Vec<String>> = vec![vec![]];
    for arg in args {
        if arg == "+" {
            segments.push(vec![]);
        } else if let Some(last) = segments.last_mut() {
            last.push(arg.clone());
        }
    }

    // Validate: no empty continuations
    for (i, seg) in segments.iter().enumerate().skip(1) {
        if seg.is_empty() {
            if i == segments.len() - 1 {
                bail!("expected operation after + (trailing + with no operation)");
            } else {
                bail!("expected operation after + (empty continuation segment)");
            }
        }
    }

    let primary_args = segments[0].clone();

    // Parse continuations
    let mut continuations = Vec::new();
    for seg in &segments[1..] {
        let op_name = &seg[0];

        // Validate operation name
        if !VALID_OPS.contains(&op_name.as_str()) {
            bail!(
                "unknown operation {op_name:?} after +. \
                 Valid operations: {}",
                VALID_OPS.join(", ")
            );
        }

        // Single-pass: extract -m values and validate no shared options.
        // The shared-options check is interleaved with -m parsing so that
        // a -m *value* that happens to match a shared option name (e.g.,
        // `-m "field:--dry-run"`) is correctly consumed as a value rather
        // than rejected as a misplaced flag.
        let mut changes = Vec::new();
        let mut iter = seg[1..].iter();
        while let Some(arg) = iter.next() {
            if arg == "-m" || arg == "--metadata" {
                let value = iter.next().ok_or_else(|| {
                    anyhow::anyhow!("{arg} requires a value in {op_name} continuation")
                })?;
                changes.push(value.clone());
            } else if let Some(value) = arg.strip_prefix("-m=") {
                changes.push(value.to_string());
            } else if let Some(value) = arg.strip_prefix("--metadata=") {
                changes.push(value.to_string());
            } else if SHARED_OPTIONS.contains(&arg.as_str()) {
                bail!(
                    "{arg} must appear in the first operation segment (before any +)"
                );
            } else {
                bail!(
                    "unexpected argument {arg:?} in {op_name} continuation \
                     (only -m/--metadata is allowed after +)"
                );
            }
        }

        continuations.push((op_name.clone(), changes));
    }

    Ok(Some(CompoundSplit {
        primary_args,
        continuations,
    }))
}

/// Parse a continuation segment into one or more ChangeGroups.
///
/// For insert operations, each `-m` arg gets its own ChangeGroup with its own
/// index, matching the behavior of `run_write_insert`. For all other ops, all
/// changes are collected into a single ChangeGroup.
fn parse_continuation_groups(
    op_name: &str,
    raw_changes: &[String],
) -> Result<Vec<ia_core::metadata::write::ChangeGroup>> {
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

/// Pre-scan raw argv for compound metadata operations.
/// Returns filtered argv (with + segments removed) and parsed continuations.
#[derive(Debug)]
pub struct CompoundFromArgv {
    pub filtered_argv: Vec<String>,
    pub continuations: Vec<(String, Vec<String>)>,
}

/// Find the position of "metadata" when it appears as the CLI subcommand
/// (first positional arg after the binary name and global flags).
///
/// `value_taking_flags` lists global flags that consume the next argv element
/// as a value (e.g., `-j`, `--host`). This is derived dynamically from clap's
/// `Cli::command()` in main.rs so it stays in sync automatically.
///
/// Returns `None` if the subcommand is anything other than "metadata",
/// preventing false positives like `ia download metadata`.
fn find_metadata_subcommand_pos(args: &[String], value_taking_flags: &[String]) -> Option<usize> {
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        // Flags that consume a value: skip the next arg too
        if value_taking_flags.iter().any(|f| f == arg) {
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

pub fn extract_compound_from_argv(
    raw_args: &[String],
    value_taking_flags: &[String],
) -> Result<Option<CompoundFromArgv>> {
    let Some(meta_pos) = find_metadata_subcommand_pos(raw_args, value_taking_flags) else {
        return Ok(None);
    };

    let args_after_metadata = &raw_args[meta_pos + 1..];
    let Some(split) = split_compound_args(args_after_metadata)? else {
        return Ok(None);
    };

    // Reconstruct filtered argv: everything up to and including "metadata" + primary args
    let mut filtered = raw_args[..=meta_pos].to_vec();
    filtered.extend(split.primary_args);

    Ok(Some(CompoundFromArgv {
        filtered_argv: filtered,
        continuations: split.continuations,
    }))
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

#[cfg(test)]
mod compound_tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn split_no_plus_returns_none() {
        let a = args("my-item -m title:New");
        let result = split_compound_args(&a).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn split_single_continuation() {
        let a = args("modify my-item -m title:New + insert -m collection[0]:featured");
        let result = split_compound_args(&a).unwrap().unwrap();
        assert_eq!(result.continuations.len(), 1);
        assert_eq!(result.continuations[0].0, "insert");
        assert_eq!(result.continuations[0].1, vec!["collection[0]:featured"]);
    }

    #[test]
    fn split_multiple_continuations() {
        let a = args("modify my-item -m title:New + insert -m x:y + remove -m z:w");
        let result = split_compound_args(&a).unwrap().unwrap();
        assert_eq!(result.continuations.len(), 2);
        assert_eq!(result.continuations[0].0, "insert");
        assert_eq!(result.continuations[1].0, "remove");
    }

    #[test]
    fn split_trailing_plus_errors() {
        let a = args("modify my-item -m title:New +");
        let result = split_compound_args(&a);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("expected operation after"), "{msg}");
    }

    #[test]
    fn split_empty_continuation_errors() {
        let a = args("modify my-item -m title:New + + remove -m x:y");
        let result = split_compound_args(&a);
        assert!(result.is_err());
    }

    #[test]
    fn split_shared_option_in_continuation_errors() {
        let a = args("modify my-item -m title:New + insert --dry-run -m x:y");
        let result = split_compound_args(&a);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--dry-run"), "{msg}");
        assert!(msg.contains("first operation segment"), "{msg}");
    }

    #[test]
    fn split_shared_option_target_in_continuation_errors() {
        let a = args("modify my-item -m title:New + insert --target files/x -m y:z");
        let result = split_compound_args(&a);
        assert!(result.is_err());
    }

    #[test]
    fn split_invalid_op_name_errors() {
        let a = args("modify my-item -m title:New + download -m x:y");
        let result = split_compound_args(&a);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unknown operation"), "{msg}");
    }

    #[test]
    fn split_shared_option_as_metadata_value_is_allowed() {
        // A metadata *value* that matches a shared option name must not be
        // rejected — the single-pass parser consumes it as -m's value.
        let a = args("modify my-item -m title:New + modify -m field:--dry-run");
        let result = split_compound_args(&a).unwrap().unwrap();
        assert_eq!(result.continuations[0].1, vec!["field:--dry-run"]);
    }

    #[test]
    fn split_continuation_with_long_metadata_flag() {
        let a = args("modify my-item -m title:New + append --metadata desc:more");
        let result = split_compound_args(&a).unwrap().unwrap();
        assert_eq!(result.continuations[0].1, vec!["desc:more"]);
    }

    #[test]
    fn parse_continuation_modify() {
        let groups =
            parse_continuation_groups("modify", &["title:New".to_string()]).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::Set));
        assert_eq!(groups[0].changes[0].0, "title");
    }

    #[test]
    fn parse_continuation_insert_with_index() {
        let groups = parse_continuation_groups(
            "insert",
            &["collection[0]:featured".to_string()],
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::Insert(0)));
    }

    #[test]
    fn parse_continuation_remove() {
        let groups =
            parse_continuation_groups("remove", &["subject:old".to_string()]).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::Remove));
    }

    #[test]
    fn parse_continuation_no_changes_errors() {
        let result = parse_continuation_groups("modify", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_continuation_append_list() {
        let groups = parse_continuation_groups(
            "append-list",
            &["subject:physics".to_string()],
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::AppendList));
    }

    #[test]
    fn parse_continuation_insert_multi_index_produces_separate_groups() {
        let groups = parse_continuation_groups(
            "insert",
            &[
                "collection[0]:featured".to_string(),
                "subject[2]:physics".to_string(),
            ],
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
        let groups = parse_continuation_groups(
            "modify",
            &["title:New".to_string(), "date:2024".to_string()],
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].changes.len(), 2);
    }

    // ─── find_metadata_subcommand_pos tests ────────────────────────────────

    /// Test-only flag list — tests exercise the algorithm (skipping flags,
    /// finding the first positional). Production uses dynamic derivation
    /// from Cli::command() so this list never needs manual sync.
    fn test_value_flags() -> Vec<String> {
        ["-c", "--config-file", "-H", "--host", "--user-agent-suffix",
         "-j", "--jobs", "--joblog"]
            .iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_subcommand_bare_metadata() {
        let a = args("ia metadata modify test -m title:New");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), Some(1));
    }

    #[test]
    fn find_subcommand_with_flags() {
        let a = args("ia -d -j 4 metadata modify test -m title:New");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), Some(4));
    }

    #[test]
    fn find_subcommand_download_returns_none() {
        let a = args("ia download test-item");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), None);
    }

    #[test]
    fn find_subcommand_host_metadata_skips_flag_value() {
        let a = args("ia --host metadata metadata modify test -m title:New");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), Some(3));
    }

    #[test]
    fn find_subcommand_config_file_skips_flag_value() {
        let a = args("ia -c /path/to/config metadata modify test -m title:New");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), Some(3));
    }

    // ─── extract_compound_from_argv tests ────────────────────────────────

    #[test]
    fn extract_compound_no_metadata_returns_none() {
        let a = args("ia download test-item");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn extract_compound_metadata_no_plus_returns_none() {
        let a = args("ia metadata modify test-item -m title:New");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn extract_compound_metadata_with_plus() {
        let a = args("ia metadata modify test-item -m title:New + remove -m x:y");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap().unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia metadata modify test-item -m title:New")
        );
        assert_eq!(result.continuations.len(), 1);
        assert_eq!(result.continuations[0].0, "remove");
    }

    #[test]
    fn extract_compound_download_metadata_not_detected() {
        let a = args("ia download metadata + remove -m x:y");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap();
        assert!(
            result.is_none(),
            "should not detect compound ops for non-metadata subcommand"
        );
    }

    #[test]
    fn extract_compound_host_metadata_not_detected() {
        let a = args("ia --host metadata metadata modify test -m title:New + remove -m x:y");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap().unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia --host metadata metadata modify test -m title:New")
        );
    }

    #[test]
    fn extract_compound_flags_before_metadata() {
        let a = args("ia -d -j 4 metadata modify test -m title:New + remove -m x:y");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap().unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia -d -j 4 metadata modify test -m title:New")
        );
    }

    #[test]
    fn extract_compound_list_metadata_not_detected() {
        let a = args("ia list metadata");
        let result = extract_compound_from_argv(&a, &test_value_flags()).unwrap();
        assert!(result.is_none());
    }
}
