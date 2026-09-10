use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::{stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use comfy_table::{Cell, Color, Table};

use crate::output::{
    format_bytes, print_retry_summary, BAR_WIDTH, ICON_ERROR, ICON_SUCCESS, PROGRESS_CHARS,
};
use ia_core::identifier::parse_identifier_line;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::metadata::write::{
    extract_target_metadata, parse_indexed_key, parse_key_value, ChangeGroup,
    CompoundModifyRequest, MetadataOp, ADMIN_ONLY_FIELDS, IMMUTABLE_FIELDS,
};
use ia_core::metadata::{
    audit_item, fetch_schema, AuditResult, FindingKind, SchemaField, Severity,
};
use ia_core::rate_limit::RateLimiter;
use ia_core::search::SearchOpts;
use ia_core::{IaClient, IaError};

/// Maximum number of errors to display inline during export.
const MAX_INLINE_ERRORS: usize = 5;

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
    #[arg(conflicts_with_all = ["itemlist", "search"])]
    pub identifiers: Vec<String>,

    /// Read identifiers from file (one per line)
    #[arg(long, conflicts_with_all = ["identifiers", "search"])]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long, conflicts_with_all = ["identifiers", "itemlist"])]
    pub search: Option<String>,

    /// Extra search parameters for --search (KEY:VALUE or KEY=VALUE, repeatable;
    /// e.g. --search-parameter sorts='addeddate desc')
    #[arg(long = "search-parameter", value_name = "PARAMETERS")]
    pub search_parameters: Vec<String>,
}

// ─── Subcommands ─────────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    /// Bulk export metadata to stdout or file
    #[command(
        long_about = "Export metadata for multiple items. Reads identifiers from files \
            (CSV, TSV, XLSX, ODS, JSONL, or plain text), search results, or stdin.\n\n\
            For spreadsheet files, the 'identifier' column is extracted. Plain text files \
            and stdin are read as one identifier per line.\n\n\
            Outputs JSONL to stdout by default, or writes to a file in CSV, TSV, XLSX, \
            or JSONL format (inferred from extension). In file mode, multi-value fields \
            are expanded into indexed columns: subject[0], subject[1], etc.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Export from a CSV file</dim>\n  <bold>$ ia metadata export items.csv</bold>\
             \n\n  <dim># Export from a plain text ID list</dim>\n  <bold>$ ia metadata export --itemlist ids.txt</bold>\
             \n\n  <dim># Export search results as JSONL</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\"</bold>\
             \n\n  <dim># Export to CSV file</dim>\n  <bold>$ ia metadata export items.csv -o data.csv</bold>\
             \n\n  <dim># Pipe identifiers from another command</dim>\n  <bold>$ ia search \"collection:nasa\" -f identifier | ia metadata export</bold>\
             \n\n  <dim># Export to XLSX for editing, then re-import</dim>\n  <bold>$ ia metadata export --search \"collection:nasa\" -o data.xlsx</bold>\
             \n  <bold>$ ia metadata --spreadsheet data.xlsx --dry-run</bold>\n"
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

    /// Bulk write metadata from a spreadsheet (deprecated: use --spreadsheet)
    #[command(hide = true)]
    Import(ImportArgs),

    /// Audit metadata against the IA schema
    #[command(
        long_about = "Audit item metadata against the Internet Archive metadata schema. \
            Reports type mismatches, missing required fields, deprecated fields, \
            and repeatability violations.\n\n\
            Fetches the live schema from archive.org and compares each item's \
            metadata against it.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Audit a single item</dim>\n  <bold>$ ia metadata audit myitem</bold>\
             \n\n  <dim># Audit items from a list</dim>\n  <bold>$ ia metadata audit --itemlist ids.txt</bold>\
             \n\n  <dim># Audit search results</dim>\n  <bold>$ ia metadata audit --search \"collection:test\"</bold>\
             \n\n  <dim># Only check specific fields</dim>\n  <bold>$ ia metadata audit myitem --field date --field collection</bold>\
             \n\n  <dim># Machine-readable output</dim>\n  <bold>$ ia metadata audit myitem --json</bold>\n"
        ),
    )]
    Audit(AuditArgs),

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
    /// Input files (CSV, TSV, XLSX, ODS, JSONL, or plain text with one ID per line)
    #[arg(conflicts_with_all = ["itemlist", "search"])]
    pub files: Vec<PathBuf>,

    /// Read identifiers from file (one per line)
    #[arg(long, conflicts_with_all = ["files", "search"])]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long, conflicts_with_all = ["files", "itemlist"])]
    pub search: Option<String>,

    /// Extra search parameters for --search (KEY:VALUE or KEY=VALUE, repeatable;
    /// e.g. --search-parameter sorts='addeddate desc')
    #[arg(long = "search-parameter", value_name = "PARAMETERS")]
    pub search_parameters: Vec<String>,

    /// Extra query parameters sent with each metadata request (KEY:VALUE or
    /// KEY=VALUE, repeatable; e.g. -p dark_ok=1 to read dark items)
    #[arg(short = 'p', long = "parameters", value_name = "PARAMETERS")]
    pub parameters: Vec<String>,

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
    pub file: Option<PathBuf>,

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

#[derive(Debug, Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub input: BatchInput,

    /// Output file (format inferred from extension: .csv, .tsv, .xlsx, .jsonl)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Only check specific field(s) (repeatable)
    #[arg(long)]
    pub field: Vec<String>,

    /// Only report missing required fields
    #[arg(long)]
    pub required_only: bool,

    /// Output as JSONL
    #[arg(long)]
    pub json: bool,
}

// ─── Top-level struct ────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    long_about = "Read or modify Internet Archive item metadata.\n\n\
        Without -m, reads metadata (JSON output). With -m, modifies metadata \
        (shorthand for 'ia metadata modify').\n\n\
        Use --search or --itemlist for batch operations — both reads and writes.\n\n\
        Chain multiple write operations with + for a single HTTP request:\n  \
        ia metadata ID -m field:val + remove -m field:val",
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
    subcommand_required = false,
)]
pub struct MetadataArgs {
    /// Batch write metadata from a spreadsheet (CSV/TSV/XLSX/ODS/JSONL)
    #[arg(long, conflicts_with_all = ["identifiers", "exists", "formats", "metadata", "itemlist", "search"])]
    pub spreadsheet: Option<PathBuf>,

    /// Modify metadata (shorthand for 'ia metadata modify')
    #[arg(short = 'm', long = "metadata", conflicts_with_all = ["spreadsheet", "exists", "formats"])]
    pub metadata: Vec<String>,

    /// Item identifier(s)
    #[arg()]
    pub identifiers: Vec<String>,

    /// Read identifiers from file (one per line)
    #[arg(long, conflicts_with = "spreadsheet")]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long, conflicts_with = "spreadsheet")]
    pub search: Option<String>,

    /// Extra search parameters for --search (KEY:VALUE or KEY=VALUE, repeatable;
    /// e.g. --search-parameter sorts='addeddate desc')
    #[arg(
        long = "search-parameter",
        value_name = "PARAMETERS",
        conflicts_with = "spreadsheet"
    )]
    pub search_parameters: Vec<String>,

    /// Extra query parameters sent with each metadata request (KEY:VALUE or
    /// KEY=VALUE, repeatable; e.g. -p dark_ok=1 to read dark items)
    #[arg(
        short = 'p',
        long = "parameters",
        value_name = "PARAMETERS",
        conflicts_with = "spreadsheet"
    )]
    pub parameters: Vec<String>,

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

    // ── Write-mode options (hidden from top-level help; see 'ia metadata modify --help') ──
    /// Target: "metadata" (default) or "files/FILENAME"
    #[arg(long, hide = true)]
    pub target: Option<String>,

    /// Optimistic concurrency check (repeatable, field:expected_value)
    #[arg(long, hide = true)]
    pub expect: Vec<String>,

    /// Task priority (default: 0 single, -5 batch)
    #[arg(long, hide = true)]
    pub priority: Option<i32>,

    /// Accept reduced priority to reduce rate limiting
    #[arg(long, hide = true)]
    pub reduced_priority: bool,

    /// Show changes without writing
    #[arg(long)]
    pub dry_run: bool,

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
    jobs: Option<usize>,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let ctx = WriteContext {
        quiet,
        jobs: jobs.unwrap_or(2), // writes use fixed concurrency
        joblog_path,
    };

    // Deprecated `import` subcommand → redirect to --spreadsheet path
    if let Some(MetadataCommand::Import(sub)) = args.command {
        if continuations.is_some() {
            bail!("compound operations (+) cannot be used with --spreadsheet");
        }
        eprintln!(
            "{}: 'ia metadata import' is deprecated, use 'ia metadata --spreadsheet <FILE>' instead",
            style("warning").yellow().bold(),
        );
        return run_import(client, sub, &ctx).await;
    }

    // --spreadsheet: batch metadata import
    if args.spreadsheet.is_some() {
        if continuations.is_some() {
            bail!("compound operations (+) cannot be used with --spreadsheet");
        }
        let import_args = ImportArgs {
            file: args.spreadsheet,
            target: args.target.unwrap_or_else(|| "metadata".into()),
            expect: args.expect,
            priority: args.priority,
            reduced_priority: args.reduced_priority,
            dry_run: args.dry_run,
            json: args.json,
        };
        return run_import(client, import_args, &ctx).await;
    }

    match args.command {
        Some(MetadataCommand::Export(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with export");
            }
            run_export(
                client,
                sub,
                ctx.quiet,
                jobs, // pass Option for adaptive support
                ctx.joblog_path,
            )
            .await
        }
        Some(MetadataCommand::Modify(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::Set,
                continuations,
                &ctx,
            )
            .await
        }
        Some(MetadataCommand::Append(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::Append,
                continuations,
                &ctx,
            )
            .await
        }
        Some(MetadataCommand::AppendList(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::AppendList,
                continuations,
                &ctx,
            )
            .await
        }
        Some(MetadataCommand::Insert(sub)) => {
            run_write_insert(client, sub.input, sub.write, continuations, &ctx).await
        }
        Some(MetadataCommand::Remove(sub)) => {
            run_write(
                client,
                sub.input,
                sub.write,
                MetadataOp::Remove,
                continuations,
                &ctx,
            )
            .await
        }
        Some(MetadataCommand::Import(_)) => unreachable!("handled above"),
        Some(MetadataCommand::Audit(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with audit");
            }
            run_audit(client, sub, ctx.quiet, jobs).await
        }
        Some(MetadataCommand::Schema(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with schema");
            }
            run_schema(client, sub).await
        }
        None => {
            // -m at top level → treat as modify shorthand
            if !args.metadata.is_empty() {
                let input = BatchInput {
                    identifiers: args.identifiers,
                    itemlist: args.itemlist,
                    search: args.search,
                    search_parameters: args.search_parameters,
                };
                let write = WriteOpts {
                    metadata: args.metadata,
                    target: args.target.unwrap_or_else(|| "metadata".into()),
                    expect: args.expect,
                    priority: args.priority,
                    reduced_priority: args.reduced_priority,
                    dry_run: args.dry_run,
                    json: args.json,
                };
                return run_write(client, input, write, MetadataOp::Set, continuations, &ctx).await;
            }

            // No -m: read mode — reject write-only options
            if continuations.is_some() {
                bail!("compound operations (+) require -m or a write subcommand (modify, append, etc.)");
            }
            if args.dry_run {
                bail!("--dry-run requires -m or a write subcommand");
            }
            if !args.expect.is_empty() {
                bail!("--expect requires -m or a write subcommand");
            }
            if args.priority.is_some() {
                bail!("--priority requires -m or a write subcommand");
            }
            if args.reduced_priority {
                bail!("--reduced-priority requires -m or a write subcommand");
            }
            if args.target.is_some() {
                bail!("--target requires -m or a write subcommand");
            }

            // Extra query params sent with each metadata GET (e.g. dark_ok=1).
            let read_params = crate::commands::search::parse_extra_params(&args.parameters)?;

            // Batch read: --search / --itemlist / stdin
            let has_batch_input = args.search.is_some() || args.itemlist.is_some();
            if has_batch_input || (args.identifiers.is_empty() && !std::io::stdin().is_terminal()) {
                let input = BatchInput {
                    identifiers: args.identifiers,
                    itemlist: args.itemlist,
                    search: args.search,
                    search_parameters: args.search_parameters,
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
                    eprintln!(
                        "{}: no identifiers found from {source}",
                        style("warning").yellow().bold()
                    );
                    return Ok(());
                }

                if args.exists {
                    return run_exists_multi(
                        client,
                        &identifiers,
                        args.json,
                        ctx.jobs,
                        &read_params,
                    )
                    .await;
                }
                if args.formats {
                    return run_formats_multi(
                        client,
                        &identifiers,
                        args.json,
                        ctx.jobs,
                        &read_params,
                    )
                    .await;
                }
                return run_read_multi(
                    client,
                    &identifiers,
                    args.pretty,
                    args.json,
                    ctx.quiet,
                    ctx.jobs,
                    &read_params,
                )
                .await;
            }

            if args.identifiers.is_empty() {
                bail!("identifier required. Run 'ia metadata --help' for usage.");
            }
            if args.identifiers.len() > 1 && (args.exists || args.formats) {
                let flag = if args.exists { "--exists" } else { "--formats" };
                bail!("{flag} can only be used with a single identifier");
            }
            // Hint: if the single identifier looks like an existing file, suggest export
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
            if args.identifiers.len() == 1 {
                run_read(
                    client,
                    &args.identifiers[0],
                    args.exists,
                    args.formats,
                    args.pretty,
                    args.json,
                    &read_params,
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
                    &read_params,
                )
                .await
            }
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
    params: &[(String, String)],
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
        .get_item_with_params(identifier, params)
        .await
        .context(format!("failed to fetch metadata for {identifier}"))?;

    if formats {
        let mut fmts: Vec<String> = item.files.iter().filter_map(|f| f.format.clone()).collect();
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

async fn run_read_multi(
    client: &IaClient,
    identifiers: &[String],
    pretty: bool,
    _json: bool,
    quiet: u8,
    jobs: usize,
    params: &[(String, String)],
) -> Result<()> {
    let total = identifiers.len();
    let client = Arc::new(client.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    let params: Arc<Vec<(String, String)>> = Arc::new(params.to_vec());

    let mut stream = stream::iter(identifiers.iter().cloned())
        .map(|id| {
            let client = client.clone();
            let counter = counter.clone();
            let params = params.clone();
            async move {
                let item = client
                    .get_item_with_params(&id, &params)
                    .await
                    .context(format!("failed to fetch metadata for {id}"))?;
                let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                Ok::<_, anyhow::Error>((item, n))
            }
        })
        .buffer_unordered(jobs);

    while let Some(result) = stream.next().await {
        let (item, n) = result?;
        let output = if pretty {
            serde_json::to_string_pretty(&item)?
        } else {
            serde_json::to_string(&item)?
        };
        println!("{output}");
        if quiet == 0 {
            eprint!("\rFetched {n}/{total} items");
        }
    }
    if quiet == 0 && total > 0 {
        eprintln!();
    }

    Ok(())
}

async fn run_exists_multi(
    client: &IaClient,
    identifiers: &[String],
    json: bool,
    jobs: usize,
    params: &[(String, String)],
) -> Result<()> {
    let client = Arc::new(client.clone());
    let params: Arc<Vec<(String, String)>> = Arc::new(params.to_vec());
    let mut any_missing = false;

    let mut stream = stream::iter(identifiers.iter().cloned())
        .map(|id| {
            let client = client.clone();
            let params = params.clone();
            async move {
                let exists = match client.get_item_with_params(&id, &params).await {
                    Ok(_) => true,
                    Err(ia_core::IaError::NotFound(_)) => false,
                    Err(e) => {
                        return Err(anyhow::Error::new(e))
                            .context(format!("failed to check existence of {id}"))
                    }
                };
                Ok::<_, anyhow::Error>((id, exists))
            }
        })
        .buffer_unordered(jobs);

    while let Some(result) = stream.next().await {
        let (id, exists) = result?;
        if !exists {
            any_missing = true;
        }
        if json {
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

async fn run_formats_multi(
    client: &IaClient,
    identifiers: &[String],
    _json: bool,
    jobs: usize,
    params: &[(String, String)],
) -> Result<()> {
    let client = Arc::new(client.clone());
    let params: Arc<Vec<(String, String)>> = Arc::new(params.to_vec());

    let mut stream = stream::iter(identifiers.iter().cloned())
        .map(|id| {
            let client = client.clone();
            let params = params.clone();
            async move {
                let item = client
                    .get_item_with_params(&id, &params)
                    .await
                    .context(format!("failed to fetch metadata for {id}"))?;
                let mut fmts: Vec<String> =
                    item.files.iter().filter_map(|f| f.format.clone()).collect();
                fmts.sort();
                fmts.dedup();
                Ok::<_, anyhow::Error>((id, fmts))
            }
        })
        .buffer_unordered(jobs);

    // Always JSONL — batch output needs per-identifier attribution.
    // Single-item --formats can print one format per line since the
    // identifier is implicit, but batch mode always needs structure.
    while let Some(result) = stream.next().await {
        let (id, fmts) = result?;
        println!("{}", serde_json::json!({"identifier": id, "formats": fmts}));
    }

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
                .is_none_or(|db| db.matches(&f.defined_by))
        })
        .filter(|f| {
            args.edit_access
                .as_ref()
                .is_none_or(|ea| ea.matches(&f.edit_access))
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
    eprintln!(
        "\nRun 'ia metadata schema <field>' for full details including usage notes and examples."
    );
}

async fn run_schema(client: &IaClient, args: SchemaArgs) -> Result<()> {
    let data = match fetch_schema(client).await {
        Ok(data) => data,
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            }
            return Err(e).context("failed to fetch metadata schema");
        }
    };
    let source = if args.files {
        &data.files_schema
    } else {
        &data.metadata_schema
    };

    // Single-field detail mode (case-insensitive lookup)
    if let Some(ref field_name) = args.field {
        let lower_name = field_name.to_lowercase();
        let found = source
            .iter()
            .find(|f| f.field.eq_ignore_ascii_case(&lower_name));
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
                    .filter(|f| f.field.starts_with(&lower_name))
                    .map(|f| f.field.as_str())
                    .take(5)
                    .collect();
                let err = IaError::SchemaFieldNotFound {
                    field: field_name.clone(),
                };
                if args.json {
                    ia_core::write_json_error(&err);
                    std::process::exit(1);
                }
                let mut msg = format!("field '{}' not found in schema", field_name);
                if !suggestions.is_empty() {
                    msg.push_str(&format!(". Did you mean: {}?", suggestions.join(", ")));
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
    } else if filtered.is_empty() {
        eprintln!("no fields match the given filters");
    } else {
        print_schema_table(&filtered);
    }

    Ok(())
}

// ─── Audit ──────────────────────────────────────────────────────────────────

/// Severity icon for terminal output.
fn severity_icon(severity: &Severity) -> &'static str {
    match severity {
        Severity::Error => "✗",
        Severity::Warning => "⚠",
        Severity::Info => "ℹ",
    }
}

/// Style a severity icon with color.
fn styled_severity(severity: &Severity) -> console::StyledObject<&'static str> {
    match severity {
        Severity::Error => style(severity_icon(severity)).red().bold(),
        Severity::Warning => style(severity_icon(severity)).yellow(),
        Severity::Info => style(severity_icon(severity)).dim(),
    }
}

async fn run_audit(
    client: &IaClient,
    args: AuditArgs,
    quiet: u8,
    jobs: Option<usize>,
) -> Result<()> {
    let identifiers = collect_identifiers_from_batch(&args.input, client).await?;
    if identifiers.is_empty() {
        bail!(crate::identifier::empty_input_message(
            args.input.search.as_deref(),
            args.input.itemlist.as_deref(),
            "No input provided. Pass identifiers, --search, or pipe via stdin.\n\
             Examples:\n  \
             ia metadata audit myitem\n  \
             ia metadata audit --search \"collection:test\"\n  \
             ia metadata audit --itemlist ids.txt",
        ));
    }

    // Fetch schema once.
    let schema_data = fetch_schema(client)
        .await
        .context("failed to fetch metadata schema")?;
    let schema = &schema_data.metadata_schema;

    let total = identifiers.len();
    let client = Arc::new(client.clone());
    let concurrency = jobs.unwrap_or(10).max(1);

    // Progress bar for batch operations.
    let pb = if quiet == 0 && total > 1 {
        let pb = ProgressBar::new(total as u64);
        pb.set_style(
            ProgressStyle::with_template(&format!(
                "Auditing metadata...\n  {{bar:{BAR_WIDTH}.cyan/dim}} {{pos}}/{{len}} {{per_sec:.dim}}  ({{elapsed}} elapsed)",
            ))
            .unwrap()
            .progress_chars(PROGRESS_CHARS),
        );
        Some(pb)
    } else {
        None
    };

    // Concurrent fetch + audit.
    let sem = Arc::new(Semaphore::new(concurrency));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<AuditResult, (String, String)>>(64);

    let feeder_client = client.clone();
    let field_filter: Arc<Vec<String>> = Arc::new(args.field.clone());
    let required_only = args.required_only;
    let schema_arc: Arc<Vec<SchemaField>> = Arc::new(schema.clone());

    let feeder = tokio::spawn(async move {
        for identifier in identifiers {
            let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
            let client = feeder_client.clone();
            let tx = tx.clone();
            let schema = schema_arc.clone();
            let field_filter = field_filter.clone();

            tokio::spawn(async move {
                let result = match client.get_item(&identifier).await {
                    Ok(item) => {
                        let mut audit = audit_item(&identifier, &item.metadata, &schema);

                        // Apply field filter.
                        if !field_filter.is_empty() {
                            audit.findings.retain(|f| field_filter.contains(&f.field));
                        }

                        // Apply required-only filter.
                        if required_only {
                            audit
                                .findings
                                .retain(|f| f.kind == FindingKind::MissingRequired);
                        }

                        Ok(audit)
                    }
                    Err(e) => Err((identifier, format!("{e:#}"))),
                };

                drop(permit);
                let _ = tx.send(result).await;
            });
        }
    });

    // Collect results.
    let mut results: Vec<AuditResult> = Vec::new();
    let mut fetch_errors: Vec<(String, String)> = Vec::new();
    let mut items_with_findings = 0usize;
    let mut error_count = 0usize;
    let mut warning_count = 0usize;

    while let Some(result) = rx.recv().await {
        if let Some(ref pb) = pb {
            pb.inc(1);
        }

        match result {
            Ok(audit) => {
                if !audit.is_clean() {
                    items_with_findings += 1;
                    error_count += audit.count(&Severity::Error);
                    warning_count += audit.count(&Severity::Warning);
                }
                results.push(audit);
            }
            Err((id, msg)) => {
                fetch_errors.push((id, msg));
            }
        }
    }

    if let Some(ref pb) = pb {
        pb.finish_and_clear();
    }

    feeder.await?;

    // Sort results by identifier for deterministic output.
    results.sort_by(|a, b| a.identifier.cmp(&b.identifier));

    // Output.
    if let Some(ref path) = args.output {
        write_audit_file(path, &results, &fetch_errors)?;
    }

    if args.json && args.output.is_none() {
        // JSONL to stdout.
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for result in &results {
            serde_json::to_writer(&mut out, result)?;
            writeln!(out)?;
        }
        for (id, msg) in &fetch_errors {
            serde_json::to_writer(
                &mut out,
                &json!({
                    "identifier": id,
                    "error": msg,
                }),
            )?;
            writeln!(out)?;
        }
    } else if args.output.is_none() {
        // Terminal output.
        for result in &results {
            if result.is_clean() {
                continue;
            }
            eprintln!("{}", style(&result.identifier).bold());
            for finding in &result.findings {
                eprintln!(
                    "  {} {}: {}",
                    styled_severity(&finding.severity),
                    style(&finding.field).cyan(),
                    finding.message
                );
            }
            eprintln!();
        }

        for (id, msg) in &fetch_errors {
            eprintln!("{} {} — {}", style(ICON_ERROR).red(), style(id).bold(), msg);
        }
    }

    // Summary.
    if quiet < 2 && total > 1 {
        eprintln!("{}", style("─".repeat(48)).dim());
        eprint!(
            "{} item(s) audited",
            style(results.len() + fetch_errors.len()).bold(),
        );
        if items_with_findings > 0 {
            eprint!(
                " · {} with findings ({} errors, {} warnings)",
                style(items_with_findings).yellow(),
                style(error_count).red(),
                style(warning_count).yellow(),
            );
        } else if fetch_errors.is_empty() {
            eprint!(" · {}", style("all clean").green());
        }
        if !fetch_errors.is_empty() {
            eprint!(" · {} fetch error(s)", style(fetch_errors.len()).red(),);
        }
        eprintln!();
        if let Some(ref path) = args.output {
            eprintln!("  {}", style(path.display()).dim());
        }
    }

    Ok(())
}

/// Convert audit results to spreadsheet records for file output.
///
/// Each finding becomes one row: identifier + {field, severity, message}.
/// This reuses `ia_core::spreadsheet::write_spreadsheet` which handles
/// CSV, TSV, XLSX, and JSONL output.
fn write_audit_file(
    path: &Path,
    results: &[AuditResult],
    fetch_errors: &[(String, String)],
) -> Result<()> {
    let mut records: Vec<ia_core::spreadsheet::SpreadsheetRecord> = Vec::new();

    for result in results {
        for finding in &result.findings {
            let sev = match finding.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            };
            let mut fields = HashMap::new();
            fields.insert("field".to_string(), finding.field.clone());
            fields.insert("severity".to_string(), sev.to_string());
            fields.insert("message".to_string(), finding.message.clone());
            records.push((result.identifier.clone(), fields));
        }
    }

    for (id, msg) in fetch_errors {
        let mut fields = HashMap::new();
        fields.insert("field".to_string(), String::new());
        fields.insert("severity".to_string(), "error".to_string());
        fields.insert("message".to_string(), msg.clone());
        records.push((id.clone(), fields));
    }

    ia_core::spreadsheet::write_spreadsheet(path, &records)
        .context(format!("failed to write audit file: {}", path.display()))
}

// ─── Export ──────────────────────────────────────────────────────────────────

/// Collect identifiers from export sources (files, --search, stdin).
/// Positional args are always files. stdin is line-per-ID.
async fn collect_identifiers_from_export(
    args: &ExportArgs,
    client: &IaClient,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();

    // Read identifiers from each file
    for path in &args.files {
        let file_ids = ia_core::spreadsheet::read_identifiers_from_file(path).context(format!(
            "failed to read identifiers from {}",
            path.display()
        ))?;
        ids.extend(file_ids);
    }

    // --itemlist: plain text file with one identifier per line
    if let Some(ref itemlist_path) = args.itemlist {
        let file_ids =
            ia_core::spreadsheet::read_identifiers_from_file(itemlist_path).context(format!(
                "failed to read identifiers from {}",
                itemlist_path.display()
            ))?;
        ids.extend(file_ids);
    }

    // Search
    if let Some(ref query) = args.search {
        let opts = SearchOpts {
            params: crate::commands::search::parse_extra_params(&args.search_parameters)?,
            ..SearchOpts::default()
        };
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // stdin fallback: when no files, no itemlist, and no search, read from stdin if piped
    if ids.is_empty()
        && args.files.is_empty()
        && args.itemlist.is_none()
        && args.search.is_none()
        && !std::io::stdin().is_terminal()
    {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.context("failed to read from stdin")?;
            if let Some(id) = parse_identifier_line(&line) {
                ids.push(id);
            }
        }
    }

    // Deduplicate preserving order
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));

    // Better error when no input at all (and clearer when --search matched nothing)
    if ids.is_empty() {
        bail!(crate::identifier::empty_input_message(
            args.search.as_deref(),
            args.itemlist.as_deref(),
            "No input provided. Pass files, --search, or pipe identifiers via stdin.\n\
             Examples:\n  \
             ia metadata export items.csv\n  \
             ia metadata export --search \"collection:nasa\"\n  \
             echo id1 | ia metadata export",
        ));
    }

    Ok(ids)
}

/// Read a joblog and return the set of items that completed successfully for the given operation.
fn completed_items_from_joblog(path: &Path, op: &str) -> Result<HashSet<String>> {
    let entries = ia_core::joblog::read(path)
        .with_context(|| format!("failed to read joblog: {}", path.display()))?;
    Ok(entries
        .iter()
        .filter(|e| e.op == op && e.status == "ok")
        .map(|e| e.item.clone())
        .collect())
}

/// Print the export summary footer to stderr.
#[allow(clippy::too_many_arguments)]
fn print_export_summary(
    succeeded: usize,
    failed: usize,
    skipped: usize,
    bytes: u64,
    elapsed_secs: f64,
    errors_shown: usize,
    output_path: Option<&Path>,
    quiet: u8,
) {
    if quiet >= 2 {
        return;
    }

    let total = succeeded + failed;

    // Overflow error count
    let overflow = errors_shown.saturating_sub(MAX_INLINE_ERRORS);
    if overflow > 0 && quiet == 0 {
        eprintln!(
            "  {} {overflow} more error(s) (see joblog)",
            style("...").dim(),
        );
    }

    // Separator
    eprintln!(
        "{}",
        style("────────────────────────────────────────────────────").dim()
    );

    // Items line
    let skip_note = if skipped > 0 {
        format!(
            " {}",
            style(format!("({skipped} previously completed)")).dim()
        )
    } else {
        String::new()
    };
    if failed == 0 {
        eprintln!(
            "{} {} items exported{skip_note}",
            style(ICON_SUCCESS).green(),
            style(succeeded).green(),
        );
    } else {
        eprintln!(
            "{}/{} items exported · {} failed{skip_note}",
            style(succeeded).green(),
            total,
            style(failed).red(),
        );
    }

    // Bytes / speed / elapsed
    let speed = if elapsed_secs > 0.0 {
        format!("{:.1} items/s", total as f64 / elapsed_secs)
    } else {
        String::new()
    };
    eprintln!(
        "{}",
        style(format!(
            "{} fetched · {} · {:.1}s elapsed",
            format_bytes(bytes),
            speed,
            elapsed_secs,
        ))
        .dim(),
    );

    // Output file line
    if let Some(path) = output_path {
        eprintln!("exported to {}", style(path.display()).bold());
    }

    // Warning for failures
    if failed > 0 {
        eprintln!(
            "{} {} item(s) failed — re-run the same command to retry (auto-resume skips completed items)",
            style("warning:").yellow().bold(),
            failed,
        );
    }
}

async fn run_export(
    client: &IaClient,
    args: ExportArgs,
    quiet: u8,
    jobs: Option<usize>,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let mut identifiers = collect_identifiers_from_export(&args, client).await?;

    // Extra query params sent with each metadata GET (e.g. dark_ok=1 for dark items).
    let export_params: Arc<Vec<(String, String)>> = Arc::new(
        crate::commands::search::parse_extra_params(&args.parameters)?,
    );

    // Auto-resume: skip items already successfully exported in this joblog
    let skip_set: HashSet<String> = if let Some(ref path) = joblog_path {
        if path.exists() {
            completed_items_from_joblog(path, "export")?
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let before_skip = identifiers.len();
    if !skip_set.is_empty() {
        identifiers.retain(|id| !skip_set.contains(id));
    }
    let skipped = before_skip - identifiers.len();

    // All items already exported — return early without touching the output file
    if identifiers.is_empty() && skipped > 0 {
        if quiet < 2 {
            eprintln!(
                "{} All {} items already exported",
                style(ICON_SUCCESS).green(),
                style(skipped).green(),
            );
            if let Some(ref path) = args.output {
                eprintln!("  {}", style(path.display()).dim());
            }
        }
        return Ok(());
    }

    // Open joblog writer
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let total = identifiers.len();
    let total_with_skipped = total + skipped;
    let client = Arc::new(client.clone());

    // Progress tracking
    // Adaptive concurrency: when --jobs is omitted, start at 10 and ramp
    // up/down via AIMD. When --jobs N is explicit, use fixed concurrency.
    let limiter = match jobs {
        Some(n) => ia_core::AdaptiveLimiter::fixed(n)?,
        None => ia_core::AdaptiveLimiter::new(10, 2, 200)?,
    };

    let succeeded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let bytes_total = Arc::new(AtomicU64::new(0));
    let errors_shown = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();

    // Progress bar — total includes skipped items so count shows overall progress
    // (e.g., 15280/125061), but rate only counts items fetched this session.
    let pb = if quiet == 0 && total > 0 {
        let skip_offset = skipped as u64;
        let pb = ProgressBar::new(total_with_skipped as u64);
        let pb_limiter = limiter.clone();
        let show_concurrency = limiter.is_adaptive();
        pb.set_style(
            ProgressStyle::with_template(&format!(
                "{{msg}}\n  {{bar:{BAR_WIDTH}.cyan/dim}} {{pos}}/{{len}} {{per_sec:.dim}}  ({{elapsed}} elapsed)",
            ))
            .unwrap()
            .with_key("per_sec", move |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                // Subtract skip offset so rate reflects only items actually fetched
                let fetched = state.pos().saturating_sub(skip_offset);
                let elapsed = state.elapsed().as_secs_f64();
                let rate = if elapsed > 0.0 { fetched as f64 / elapsed } else { 0.0 };
                if show_concurrency {
                    write!(w, "{rate:.1}/s j={}", pb_limiter.target()).ok();
                } else {
                    write!(w, "{rate:.1}/s").ok();
                }
            })
            .progress_chars(PROGRESS_CHARS),
        );

        let msg = if skipped > 0 {
            format!(
                "Exporting metadata... {}",
                style(format!("(resuming — {skipped} already exported)")).dim()
            )
        } else {
            "Exporting metadata...".to_string()
        };
        pb.set_message(msg);

        // Start at skip offset so bar shows overall progress
        if skipped > 0 {
            pb.set_position(skip_offset);
        }

        Some(pb)
    } else {
        None
    };

    // Channel-based bounded spawning: the feeder task acquires a permit
    // before spawning each work task, so only `target` tasks + a small
    // buffer exist at a time (no unbounded memory for large exports).
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(
        String,
        Result<ia_core::types::ItemMetadata, ia_core::IaError>,
        u64,
    )>(64);

    let feeder_client = client.clone();
    let feeder_limiter = limiter.clone();
    let feeder = tokio::spawn(async move {
        for identifier in identifiers {
            let permit = feeder_limiter.acquire().await;
            let client = feeder_client.clone();
            let lim = feeder_limiter.clone();
            let tx = tx.clone();
            let params = export_params.clone();

            tokio::spawn(async move {
                let req_start = Instant::now();
                let adaptive = lim.is_adaptive();
                let mut permit = Some(permit);

                let result = loop {
                    match client.get_item_with_params(&identifier, &params).await {
                        Ok(item) => {
                            lim.on_success();
                            break Ok(item);
                        }
                        Err(ia_core::IaError::RateLimited { retry_after }) => {
                            lim.on_rate_limited(retry_after, |old, new, secs| {
                                if adaptive {
                                    tracing::warn!(
                                        old_concurrency = old,
                                        new_concurrency = new,
                                        pause_secs = secs,
                                        "rate limited — reducing concurrency",
                                    );
                                } else {
                                    tracing::warn!(
                                        pause_secs = secs,
                                        "rate limited — pausing all workers",
                                    );
                                }
                            })
                            .await;
                            // Release permit and re-acquire — excess workers
                            // block here until active < target, naturally
                            // draining to the new concurrency level.
                            if let Some(p) = permit.take() {
                                drop(p);
                            }
                            permit = Some(lim.acquire().await);
                        }
                        Err(e) => break Err(e),
                    }
                };

                let elapsed_ms = req_start.elapsed().as_millis() as u64;
                drop(permit); // release concurrency slot
                let _ = tx.send((identifier, result, elapsed_ms)).await;
            });
        }
        // tx dropped here — signals completion to rx
    });

    if let Some(ref path) = args.output {
        // Detect JSONL output for streaming writes
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let is_jsonl = ext == "jsonl" || ext == "ndjson";

        // JSONL: open in append mode for streaming writes (resilient to Ctrl+C).
        // CSV/XLSX: collect records in memory for unified column computation.
        let mut jsonl_file = if is_jsonl {
            Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .context(format!("failed to open export file: {}", path.display()))?,
            )
        } else {
            None
        };

        // CSV/TSV/XLSX on resume: read existing records so we can merge with new ones.
        let mut records: Vec<ia_core::spreadsheet::SpreadsheetRecord> =
            if !is_jsonl && skipped > 0 && path.exists() {
                ia_core::spreadsheet::read_spreadsheet(path).unwrap_or_else(|e| {
                    eprintln!(
                        "{} could not read existing export file, starting fresh: {e}",
                        style("warning:").yellow().bold(),
                    );
                    Vec::new()
                })
            } else {
                Vec::new()
            };

        while let Some((identifier, result, elapsed_ms)) = rx.recv().await {
            match result {
                Ok(item) => {
                    let json_bytes = serde_json::to_string(&item)
                        .map(|s| s.len() as u64)
                        .unwrap_or(0);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "").ok(json_bytes, elapsed_ms),
                        );
                    }
                    bytes_total.fetch_add(json_bytes, Ordering::Relaxed);
                    succeeded.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref pb) = pb {
                        pb.inc(1);
                    }

                    // JSONL: write full API response immediately (append mode,
                    // survives Ctrl+C). Identical to stdout mode output.
                    // CSV/XLSX: extract metadata fields into flat columns.
                    if let Some(ref mut f) = jsonl_file {
                        let line = serde_json::to_string(&item)?;
                        writeln!(f, "{line}").context("failed to write to export file")?;
                    } else {
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
                        records.push((identifier, fields));
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if let Some(ref jl) = joblog {
                        jl.write(&JoblogEntry::new("export", &identifier, "").error(&msg, 0));
                    }
                    failed.fetch_add(1, Ordering::Relaxed);
                    let shown = errors_shown.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref pb) = pb {
                        pb.inc(1);
                    }
                    if quiet == 0 && shown < MAX_INLINE_ERRORS {
                        if let Some(ref pb) = pb {
                            pb.suspend(|| {
                                eprintln!(
                                    "{} {} — {}",
                                    style(ICON_ERROR).red(),
                                    style(&identifier).bold(),
                                    style(&msg).red(),
                                );
                            });
                        } else {
                            eprintln!(
                                "{} {} — {}",
                                style(ICON_ERROR).red(),
                                style(&identifier).bold(),
                                style(&msg).red(),
                            );
                        }
                    }
                }
            }
        }

        if let Some(ref pb) = pb {
            pb.finish_and_clear();
        }

        // CSV/TSV/XLSX: write all records (existing + new) at once.
        // JSONL was already streamed above — just flush.
        if let Some(mut f) = jsonl_file {
            f.flush().context("failed to flush export file")?;
        } else {
            ia_core::spreadsheet::write_spreadsheet(path, &records)
                .context(format!("failed to write export file: {}", path.display()))?;
        }

        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            skipped,
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            Some(path.as_path()),
            quiet,
        );
        if quiet < 2 {
            print_retry_summary(client.retry_stats());
        }
    } else {
        // Stdout mode: stream results as they complete (no buffering).
        while let Some((identifier, result, elapsed_ms)) = rx.recv().await {
            match result {
                Ok(item) => {
                    let json_bytes = serde_json::to_string(&item)
                        .map(|s| s.len() as u64)
                        .unwrap_or(0);
                    if let Some(ref jl) = joblog {
                        jl.write(
                            &JoblogEntry::new("export", &identifier, "").ok(json_bytes, elapsed_ms),
                        );
                    }
                    bytes_total.fetch_add(json_bytes, Ordering::Relaxed);
                    succeeded.fetch_add(1, Ordering::Relaxed);

                    let output = if args.pretty {
                        serde_json::to_string_pretty(&item)?
                    } else {
                        serde_json::to_string(&item)?
                    };
                    if let Some(ref pb) = pb {
                        pb.suspend(|| println!("{output}"));
                        pb.inc(1);
                    } else {
                        println!("{output}");
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if let Some(ref jl) = joblog {
                        jl.write(&JoblogEntry::new("export", &identifier, "").error(&msg, 0));
                    }
                    failed.fetch_add(1, Ordering::Relaxed);
                    let shown = errors_shown.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref pb) = pb {
                        pb.inc(1);
                    }
                    if quiet == 0 && shown < MAX_INLINE_ERRORS {
                        if let Some(ref pb) = pb {
                            pb.suspend(|| {
                                eprintln!(
                                    "{} {} — {}",
                                    style(ICON_ERROR).red(),
                                    style(&identifier).bold(),
                                    style(&msg).red(),
                                );
                            });
                        } else {
                            eprintln!(
                                "{} {} — {}",
                                style(ICON_ERROR).red(),
                                style(&identifier).bold(),
                                style(&msg).red(),
                            );
                        }
                    }
                }
            }
        }

        if let Some(ref pb) = pb {
            pb.finish_and_clear();
        }

        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            skipped,
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            None,
            quiet,
        );
        if quiet < 2 {
            print_retry_summary(client.retry_stats());
        }
    }

    // Ensure feeder task completes (it should already be done since rx drained).
    feeder
        .await
        .map_err(|e| anyhow::anyhow!("metadata export feeder task panicked: {e}"))?;

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
    let mut identifiers = collect_identifiers_from_batch(&input, client).await?;
    if identifiers.is_empty() {
        bail!(crate::identifier::empty_input_message(
            input.search.as_deref(),
            input.itemlist.as_deref(),
            "no identifiers provided",
        ));
    }

    // Auto-resume: skip items already successfully modified in this joblog
    let skip_set: HashSet<String> = if let Some(ref path) = ctx.joblog_path {
        if path.exists() {
            completed_items_from_joblog(path, MODIFY_OP)?
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let before_skip = identifiers.len();
    if !skip_set.is_empty() {
        identifiers.retain(|id| !skip_set.contains(id));
    }
    let skipped = before_skip - identifiers.len();

    if identifiers.is_empty() && skipped > 0 {
        if ctx.quiet < 2 {
            eprintln!(
                "{} All {} item(s) already modified",
                style(ICON_SUCCESS).green(),
                style(skipped).green(),
            );
        }
        return Ok(());
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

    if skipped > 0 && !json && ctx.quiet == 0 {
        eprintln!(
            "{}",
            style(format!("(resuming — {skipped} already modified)")).dim()
        );
    }

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
                    Ok(resp) => break ModifyOutcome::Success(resp.task_id),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |secs| {
                            eprintln!("Rate limited. Pausing all workers for {secs}s...");
                        })
                        .await;
                    }
                    Err(IaError::NoChanges { .. }) => break ModifyOutcome::NoChanges,
                    Err(e) => break ModifyOutcome::Error(e.to_string()),
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
            (identifier, outcome, elapsed_ms)
        });
    }

    let mut error_count = 0usize;
    let mut skip_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) = result.context("task panicked")?;

        match record_modify_outcome(
            &identifier,
            &outcome,
            elapsed_ms,
            &file_target,
            ctx.quiet,
            joblog.as_ref(),
            json,
        ) {
            OutcomeKind::Error => error_count += 1,
            OutcomeKind::NoChanges => skip_count += 1,
            OutcomeKind::Success => {}
        }
    }

    if skip_count > 0 && error_count == 0 && !json && ctx.quiet == 0 {
        eprintln!(
            "warning: {skip_count} of {total_count} item(s) already matched (no changes applied)"
        );
    }

    if error_count > 0 {
        if json {
            std::process::exit(1);
        }
        if skip_count > 0 {
            bail!("{error_count} of {total_count} item(s) failed ({skip_count} already matched)");
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
async fn run_import(client: &IaClient, args: ImportArgs, ctx: &WriteContext) -> Result<()> {
    let json = args.json;

    let file = match args.file {
        Some(f) => f,
        None => {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "import reads metadata changes from a spreadsheet file, not stdin.\n\
                     To apply metadata to piped identifiers, use modify:\n  \
                     ... | ia metadata modify -m key:value"
                );
            }
            bail!(
                "missing required argument: <FILE>\n\
                 Usage: ia metadata --spreadsheet <FILE> [OPTIONS]\n\n\
                 To apply metadata to identifiers from stdin, use modify:\n  \
                 ... | ia metadata modify -m key:value"
            );
        }
    };

    let records = ia_core::spreadsheet::read_spreadsheet(&file)
        .context(format!("failed to read spreadsheet: {}", file.display()))?;

    let priority = args.priority.unwrap_or(-5);

    // Build (identifier, change_groups) pairs from records.
    // Each record's columns are parsed for operation prefixes.
    let mut work_items: Vec<(String, Vec<ChangeGroup>)> = Vec::new();
    for (identifier, fields) in &records {
        if fields.is_empty() {
            continue;
        }

        let resolved = ia_core::spreadsheet::merge_indexed_columns(fields);

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

    // Auto-resume: skip items already successfully modified in this joblog
    let skip_set: HashSet<String> = if let Some(ref path) = ctx.joblog_path {
        if path.exists() {
            completed_items_from_joblog(path, MODIFY_OP)?
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let before_skip = work_items.len();
    if !skip_set.is_empty() {
        work_items.retain(|(id, _)| !skip_set.contains(id));
    }
    let skipped = before_skip - work_items.len();

    let item_count = work_items.len();

    if item_count == 0 && skipped > 0 {
        if ctx.quiet < 2 {
            eprintln!(
                "{} All {} item(s) already modified",
                style(ICON_SUCCESS).green(),
                style(skipped).green(),
            );
        }
        return Ok(());
    }

    let joblog = ctx
        .joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Parse --expect values
    let expect: Option<HashMap<String, serde_json::Value>> = if !args.expect.is_empty() {
        let mut map = HashMap::new();
        for s in &args.expect {
            let (key, value) =
                parse_key_value(s).with_context(|| format!("invalid expect key:value: {s:?}"))?;
            map.insert(key, json!(value));
        }
        Some(map)
    } else {
        None
    };

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
                expect.as_ref(),
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

    if skipped > 0 && !json && ctx.quiet == 0 {
        eprintln!(
            "{}",
            style(format!("(resuming — {skipped} already modified)")).dim()
        );
    }

    for (identifier, groups) in work_items {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let target = args.target.clone();
        let reduced_priority = args.reduced_priority;
        let expect = expect.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = std::time::Instant::now();

            let compound_req = CompoundModifyRequest {
                identifier: identifier.clone(),
                groups,
                target,
                expect,
                priority: Some(priority),
                reduced_priority,
            };

            let outcome = loop {
                rl.wait_if_paused().await;
                match ia_core::metadata::modify_compound(&client, &compound_req).await {
                    Ok(resp) => break ModifyOutcome::Success(resp.task_id),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |secs| {
                            eprintln!("Rate limited. Pausing all workers for {secs}s...");
                        })
                        .await;
                    }
                    Err(IaError::NoChanges { .. }) => break ModifyOutcome::NoChanges,
                    Err(e) => break ModifyOutcome::Error(e.to_string()),
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
            (identifier, outcome, elapsed_ms)
        });
    }

    let mut error_count = 0usize;
    let mut skip_count = 0usize;
    while let Some(result) = set.join_next().await {
        let (identifier, outcome, elapsed_ms) = result.context("task panicked")?;

        match record_modify_outcome(
            &identifier,
            &outcome,
            elapsed_ms,
            &file_target,
            ctx.quiet,
            joblog.as_ref(),
            json,
        ) {
            OutcomeKind::Error => error_count += 1,
            OutcomeKind::NoChanges => skip_count += 1,
            OutcomeKind::Success => {}
        }
    }

    if skip_count > 0 && error_count == 0 && !json && ctx.quiet == 0 {
        eprintln!(
            "warning: {skip_count} of {item_count} item(s) already matched (no changes applied)"
        );
    }

    if error_count > 0 {
        if json {
            std::process::exit(1);
        }
        if skip_count > 0 {
            bail!("{error_count} of {item_count} item(s) failed ({skip_count} already matched)");
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
    let status = resp.status().as_u16();
    if status == 404 {
        bail!("item not found: {identifier}");
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        bail!("failed to fetch metadata for {identifier}: HTTP {status} {body}");
    }
    let item: serde_json::Value = resp.json().await.map_err(|e| anyhow::anyhow!("{e}"))?;

    let source =
        extract_target_metadata(&item, target, identifier).map_err(|e| anyhow::anyhow!("{e}"))?;

    let patch = ia_core::metadata::compute_compound_patch(&source, groups, expect, identifier)?;

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
/// Result of processing a single modify item.
enum ModifyOutcome {
    Success(Option<u64>),
    NoChanges,
    Error(String),
}

/// Classification returned by `record_modify_outcome` for counting.
enum OutcomeKind {
    Success,
    NoChanges,
    Error,
}

/// Joblog operation name for metadata modify/import writes.
const MODIFY_OP: &str = "modify";

fn record_modify_outcome(
    identifier: &str,
    outcome: &ModifyOutcome,
    elapsed_ms: u64,
    file_target: &str,
    quiet: u8,
    joblog: Option<&JoblogWriter>,
    json: bool,
) -> OutcomeKind {
    match outcome {
        ModifyOutcome::Success(task_id) => {
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
                println!("{identifier}: success (task_id: {})", task_id.unwrap_or(0));
            }
            if let Some(jl) = joblog {
                let entry = JoblogEntry::new(MODIFY_OP, identifier, file_target).ok(0, elapsed_ms);
                jl.write(&entry);
            }
            OutcomeKind::Success
        }
        ModifyOutcome::NoChanges => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "item": identifier,
                        "status": "no_changes",
                        "elapsed_ms": elapsed_ms,
                    })
                );
            } else if quiet < 2 {
                eprintln!("warning: {identifier}: no changes (values already match)");
            }
            if let Some(jl) = joblog {
                let entry = JoblogEntry::new(MODIFY_OP, identifier, file_target).ok(0, elapsed_ms);
                jl.write(&entry);
            }
            OutcomeKind::NoChanges
        }
        ModifyOutcome::Error(e) => {
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
                let entry = JoblogEntry::new(MODIFY_OP, identifier, file_target).error(e, 0);
                jl.write(&entry);
            }
            OutcomeKind::Error
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
            if let Some(id) = parse_identifier_line(line) {
                ids.push(id);
            }
        }
    }

    if let Some(ref query) = input.search {
        let opts = SearchOpts {
            params: crate::commands::search::parse_extra_params(&input.search_parameters)?,
            ..SearchOpts::default()
        };
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
            if let Some(id) = parse_identifier_line(&line) {
                ids.push(id);
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
                bail!("{arg} must appear in the first operation segment (before any +)");
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
        // Any other flag (boolean flags like -d, -l, -q, --no-resume)
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
        let groups = parse_continuation_groups("modify", &["title:New".to_string()]).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::Set));
        assert_eq!(groups[0].changes[0].0, "title");
    }

    #[test]
    fn parse_continuation_insert_with_index() {
        let groups =
            parse_continuation_groups("insert", &["collection[0]:featured".to_string()]).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0].op, MetadataOp::Insert(0)));
    }

    #[test]
    fn parse_continuation_remove() {
        let groups = parse_continuation_groups("remove", &["subject:old".to_string()]).unwrap();
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
        let groups =
            parse_continuation_groups("append-list", &["subject:physics".to_string()]).unwrap();
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
        let groups =
            parse_continuation_groups("insert", &["collection[0]:featured".to_string()]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].op, MetadataOp::Insert(0));
    }

    #[test]
    fn parse_continuation_insert_no_index_defaults_zero() {
        let groups =
            parse_continuation_groups("insert", &["collection:featured".to_string()]).unwrap();
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
        [
            "-c",
            "--config-file",
            "-H",
            "--host",
            "--user-agent-suffix",
            "-j",
            "--jobs",
            "--joblog",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn find_subcommand_bare_metadata() {
        let a = args("ia metadata modify test -m title:New");
        assert_eq!(
            find_metadata_subcommand_pos(&a, &test_value_flags()),
            Some(1)
        );
    }

    #[test]
    fn find_subcommand_with_flags() {
        let a = args("ia -d -j 4 metadata modify test -m title:New");
        assert_eq!(
            find_metadata_subcommand_pos(&a, &test_value_flags()),
            Some(4)
        );
    }

    #[test]
    fn find_subcommand_download_returns_none() {
        let a = args("ia download test-item");
        assert_eq!(find_metadata_subcommand_pos(&a, &test_value_flags()), None);
    }

    #[test]
    fn find_subcommand_host_metadata_skips_flag_value() {
        let a = args("ia --host metadata metadata modify test -m title:New");
        assert_eq!(
            find_metadata_subcommand_pos(&a, &test_value_flags()),
            Some(3)
        );
    }

    #[test]
    fn find_subcommand_config_file_skips_flag_value() {
        let a = args("ia -c /path/to/config metadata modify test -m title:New");
        assert_eq!(
            find_metadata_subcommand_pos(&a, &test_value_flags()),
            Some(3)
        );
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
        let result = extract_compound_from_argv(&a, &test_value_flags())
            .unwrap()
            .unwrap();
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
        let result = extract_compound_from_argv(&a, &test_value_flags())
            .unwrap()
            .unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia --host metadata metadata modify test -m title:New")
        );
    }

    #[test]
    fn extract_compound_flags_before_metadata() {
        let a = args("ia -d -j 4 metadata modify test -m title:New + remove -m x:y");
        let result = extract_compound_from_argv(&a, &test_value_flags())
            .unwrap()
            .unwrap();
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

    // ─── Top-level shorthand compound (no explicit subcommand) ──────────

    #[test]
    fn extract_compound_shorthand_with_plus() {
        let a = args("ia metadata my-item -m title:New + remove -m subject:old");
        let result = extract_compound_from_argv(&a, &test_value_flags())
            .unwrap()
            .unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia metadata my-item -m title:New")
        );
        assert_eq!(result.continuations.len(), 1);
        assert_eq!(result.continuations[0].0, "remove");
        assert_eq!(result.continuations[0].1, vec!["subject:old"]);
    }

    #[test]
    fn extract_compound_shorthand_multi_continuations() {
        let a = args("ia metadata my-item -m title:New + remove -m x:y + append-list -m z:w");
        let result = extract_compound_from_argv(&a, &test_value_flags())
            .unwrap()
            .unwrap();
        assert_eq!(
            result.filtered_argv,
            args("ia metadata my-item -m title:New")
        );
        assert_eq!(result.continuations.len(), 2);
        assert_eq!(result.continuations[0].0, "remove");
        assert_eq!(result.continuations[1].0, "append-list");
    }
}

// ─── MetadataArgs clap parsing tests ────────────────────────────────────────

#[cfg(test)]
mod args_parsing_tests {
    use super::*;
    use clap::Parser;

    /// Wrapper to test MetadataArgs parsing in isolation.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: MetadataArgs,
    }

    fn parse(s: &str) -> MetadataArgs {
        let argv: Vec<&str> = std::iter::once("test")
            .chain(s.split_whitespace())
            .collect();
        TestCli::try_parse_from(argv).unwrap().args
    }

    fn parse_err(s: &str) -> String {
        let argv: Vec<&str> = std::iter::once("test")
            .chain(s.split_whitespace())
            .collect();
        TestCli::try_parse_from(argv).unwrap_err().to_string()
    }

    // ─── Read modes ─────────────────────────────────────────────────────

    #[test]
    fn read_single_identifier() {
        let a = parse("nasa");
        assert_eq!(a.identifiers, vec!["nasa"]);
        assert!(a.metadata.is_empty());
        assert!(a.search.is_none());
        assert!(a.itemlist.is_none());
        assert!(a.command.is_none());
    }

    #[test]
    fn read_multiple_identifiers() {
        let a = parse("nasa apollo11");
        assert_eq!(a.identifiers, vec!["nasa", "apollo11"]);
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn read_with_search() {
        let a = parse("--search collection:nasa");
        assert_eq!(a.search.as_deref(), Some("collection:nasa"));
        assert!(a.metadata.is_empty());
        assert!(a.identifiers.is_empty());
    }

    #[test]
    fn read_with_itemlist() {
        let a = parse("--itemlist ids.txt");
        assert_eq!(a.itemlist.as_ref().unwrap().to_str().unwrap(), "ids.txt");
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn read_with_search_and_itemlist() {
        let a = parse("--search collection:nasa --itemlist ids.txt");
        assert!(a.search.is_some());
        assert!(a.itemlist.is_some());
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn read_with_search_and_identifiers() {
        let a = parse("--search collection:nasa extra-id");
        assert!(a.search.is_some());
        assert_eq!(a.identifiers, vec!["extra-id"]);
    }

    #[test]
    fn read_with_exists() {
        let a = parse("-e nasa");
        assert!(a.exists);
        assert_eq!(a.identifiers, vec!["nasa"]);
    }

    #[test]
    fn read_with_pretty_json() {
        let a = parse("nasa --pretty --json");
        assert!(a.pretty);
        assert!(a.json);
    }

    // ─── Write modes (with -m) ──────────────────────────────────────────

    #[test]
    fn write_single_metadata() {
        let a = parse("nasa -m title:New");
        assert_eq!(a.identifiers, vec!["nasa"]);
        assert_eq!(a.metadata, vec!["title:New"]);
    }

    #[test]
    fn write_multiple_metadata() {
        let a = parse("nasa -m title:New -m date:2024");
        assert_eq!(a.metadata, vec!["title:New", "date:2024"]);
    }

    #[test]
    fn write_with_search() {
        let a = parse("--search collection:test -m subject:updated");
        assert!(a.search.is_some());
        assert_eq!(a.metadata, vec!["subject:updated"]);
    }

    #[test]
    fn write_with_itemlist() {
        let a = parse("--itemlist ids.txt -m subject:new-tag");
        assert!(a.itemlist.is_some());
        assert_eq!(a.metadata, vec!["subject:new-tag"]);
    }

    #[test]
    fn write_with_target() {
        let a = parse("nasa -m title:X --target files/myfile.txt");
        assert_eq!(a.target.as_deref(), Some("files/myfile.txt"));
        assert_eq!(a.metadata, vec!["title:X"]);
    }

    #[test]
    fn write_with_dry_run() {
        let a = parse("nasa -m title:X --dry-run");
        assert!(a.dry_run);
    }

    #[test]
    fn write_with_expect_and_priority() {
        let a = parse("nasa -m title:X --expect field:old --priority 5");
        assert_eq!(a.expect, vec!["field:old"]);
        assert_eq!(a.priority, Some(5));
    }

    #[test]
    fn write_with_reduced_priority() {
        let a = parse("nasa -m title:X --reduced-priority");
        assert!(a.reduced_priority);
    }

    #[test]
    fn write_with_all_options() {
        let a = parse(
            "nasa -m title:X --target files/f.txt --expect a:b --priority 3 --reduced-priority --dry-run --json",
        );
        assert_eq!(a.metadata, vec!["title:X"]);
        assert_eq!(a.target.as_deref(), Some("files/f.txt"));
        assert_eq!(a.expect, vec!["a:b"]);
        assert_eq!(a.priority, Some(3));
        assert!(a.reduced_priority);
        assert!(a.dry_run);
        assert!(a.json);
    }

    #[test]
    fn target_defaults_to_metadata() {
        let a = parse("nasa -m title:X");
        // No explicit --target → None, which run() resolves to "metadata"
        assert!(a.target.is_none());
    }

    // ─── Clap-level conflict errors ─────────────────────────────────────

    #[test]
    fn conflict_metadata_with_spreadsheet() {
        let err = parse_err("--spreadsheet data.csv -m title:X");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    #[test]
    fn conflict_metadata_with_exists() {
        let err = parse_err("nasa -m title:X --exists");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    #[test]
    fn conflict_metadata_with_formats() {
        let err = parse_err("nasa -m title:X --formats");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    #[test]
    fn conflict_spreadsheet_with_identifiers() {
        let err = parse_err("--spreadsheet data.csv nasa");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    #[test]
    fn conflict_spreadsheet_with_search() {
        let err = parse_err("--spreadsheet data.csv --search query");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    #[test]
    fn conflict_spreadsheet_with_itemlist() {
        let err = parse_err("--spreadsheet data.csv --itemlist ids.txt");
        assert!(
            err.contains("cannot be used with"),
            "expected conflict error: {err}"
        );
    }

    // ─── Subcommand coexistence ─────────────────────────────────────────

    #[test]
    fn subcommand_modify_parses_separately() {
        let a = parse("modify nasa -m title:New");
        assert!(matches!(a.command, Some(MetadataCommand::Modify(_))));
        // Top-level metadata should be empty — -m belongs to the subcommand
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn subcommand_export_with_search() {
        let a = parse("export --search collection:nasa");
        assert!(matches!(a.command, Some(MetadataCommand::Export(_))));
    }

    #[test]
    fn subcommand_schema() {
        let a = parse("schema");
        assert!(matches!(a.command, Some(MetadataCommand::Schema(_))));
    }

    #[test]
    fn subcommand_modify_with_target() {
        let a = parse("modify nasa -m title:X --target files/f.txt");
        match a.command {
            Some(MetadataCommand::Modify(sub)) => {
                assert_eq!(sub.write.target, "files/f.txt");
                assert_eq!(sub.write.metadata, vec!["title:X"]);
            }
            other => panic!("expected Modify, got {other:?}"),
        }
    }

    #[test]
    fn subcommand_remove() {
        let a = parse("remove nasa -m subject:old");
        assert!(matches!(a.command, Some(MetadataCommand::Remove(_))));
    }

    #[test]
    fn subcommand_append_list() {
        let a = parse("append-list nasa -m subject:new-tag");
        assert!(matches!(a.command, Some(MetadataCommand::AppendList(_))));
    }

    // ─── Write-only options are accepted without -m at parse level ───────
    // (Runtime validation rejects them — tested in integration tests)

    #[test]
    fn dry_run_without_metadata_parses_ok() {
        // Clap accepts it; runtime should reject
        let a = parse("nasa --dry-run");
        assert!(a.dry_run);
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn target_without_metadata_parses_ok() {
        let a = parse("nasa --target files/x");
        assert_eq!(a.target.as_deref(), Some("files/x"));
        assert!(a.metadata.is_empty());
    }

    #[test]
    fn priority_without_metadata_parses_ok() {
        let a = parse("nasa --priority 5");
        assert_eq!(a.priority, Some(5));
        assert!(a.metadata.is_empty());
    }

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
}
