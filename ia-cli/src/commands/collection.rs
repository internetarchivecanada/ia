use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::stream::{self, StreamExt};

use ia_core::collection::{
    create_collection, read_collection_spreadsheet, CollectionRow, CreateCollectionResult,
};
use ia_core::error::format_error_chain;
use ia_core::joblog::{self, JoblogEntry, JoblogWriter};
use ia_core::{IaClient, IaError};

/// The job log `op` of one created collection.
const JOBLOG_OP: &str = "collection-create";

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    about = "Create and manage collections",
    long_about = "Create and manage Internet Archive collections. Collections are items with \
        mediatype=collection that group related items together."
)]
pub struct CollectionArgs {
    #[command(subcommand)]
    pub command: CollectionCommand,
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    /// Create a new collection
    #[command(
        long_about = "Create a new Internet Archive collection via S3. Only the identifier and \
            parent collection are required. Title, description, and subject are recommended but \
            optional.\n\n\
            With --spreadsheet, create one collection per row of a CSV (with a header row), TSV, \
            XLSX, ODS or JSONL file. Columns: identifier (required), collection (the parent; \
            required unless --collection is given), image (a cover image path), and any other \
            column as metadata (title, description, subject, ...). --collection, --title, \
            --description, --subject, --image and -m apply to every row; a field a row fills in \
            replaces them for that row. Every row is checked before the first request and every \
            problem is reported. A failed row does not stop the others; the exit status is 1 if \
            any row failed. With --joblog, a rerun skips the collections already created.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Minimal collection (identifier + parent only)</dim>\
             \n  <bold>$ ia collection create my-collection --collection opensource</bold>\
             \n\n  <dim># Recommended: include title, description, subject</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    -D \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource</bold>\
             \n\n  <dim># With an image and extra metadata</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    -D \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    --image logo.png -m hidden:true</bold>\
             \n\n  <dim># Many collections from a CSV with a header row, one shared parent</dim>\
             \n  <bold>$ ia collection create --spreadsheet collections.csv --collection opensource</bold>\
             \n\n  <dim># Check every row without sending anything</dim>\
             \n  <bold>$ ia collection create --spreadsheet collections.jsonl --dry-run</bold>\
             \n\n  <dim># Record each row; a rerun skips the ones already created</dim>\
             \n  <bold>$ ia collection create --spreadsheet collections.csv --joblog create.jsonl</bold>\n"
        ),
    )]
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Collection identifier
    #[arg(
        required_unless_present = "spreadsheet",
        conflicts_with = "spreadsheet"
    )]
    pub identifier: Option<String>,

    /// Parent collection identifier (with --spreadsheet: for rows without a collection value)
    #[arg(short = 'C', long, required_unless_present = "spreadsheet")]
    pub collection: Option<String>,

    /// Collection title
    #[arg(short = 't', long)]
    pub title: Option<String>,

    /// Collection description
    #[arg(short = 'D', long)]
    pub description: Option<String>,

    /// Subject/topic
    #[arg(short = 's', long)]
    pub subject: Option<String>,

    /// Path to collection image file
    #[arg(short = 'I', long)]
    pub image: Option<PathBuf>,

    /// Additional metadata (repeatable, KEY:VALUE)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Create one collection per row of a file (CSV with a header row, TSV, XLSX, ODS, JSONL)
    #[arg(long, value_name = "FILE")]
    pub spreadsheet: Option<PathBuf>,

    /// Validate everything without sending the request
    #[arg(long)]
    pub dry_run: bool,

    /// Output JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Run ─────────────────────────────────────────────────────────────────────

/// Run `ia collection`. `jobs`, `joblog` and `no_resume` are the global
/// flags, used by `create --spreadsheet`.
pub async fn run(
    client: &IaClient,
    args: CollectionArgs,
    quiet: u8,
    jobs: usize,
    joblog: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    match args.command {
        CollectionCommand::Create(create_args) => match create_args.spreadsheet.clone() {
            Some(path) => {
                run_spreadsheet(client, &create_args, &path, quiet, jobs, joblog, no_resume).await
            }
            None => run_create(client, create_args, quiet).await,
        },
    }
}

/// The metadata the command-line flags give: `--collection`, `--title`,
/// `--description`, `--subject`, then each `-m KEY:VALUE`.
fn flag_metadata(args: &CreateArgs) -> Result<Vec<(String, String)>> {
    let mut metadata: Vec<(String, String)> = Vec::new();
    let flags = [
        ("collection", &args.collection),
        ("title", &args.title),
        ("description", &args.description),
        ("subject", &args.subject),
    ];
    for (key, value) in flags {
        if let Some(value) = value {
            metadata.push((key.into(), value.clone()));
        }
    }
    for m in &args.metadata {
        let (key, value) = m
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid KEY:VALUE format: '{m}'"))?;
        metadata.push((key.to_string(), value.to_string()));
    }
    Ok(metadata)
}

async fn run_create(client: &IaClient, args: CreateArgs, quiet: u8) -> Result<()> {
    // Clap requires both without --spreadsheet.
    let identifier = args
        .identifier
        .clone()
        .ok_or_else(|| anyhow::anyhow!("an identifier is required"))?;
    let metadata = flag_metadata(&args)?;
    let image_path = args.image.as_deref();

    let result = create_collection(client, &identifier, &metadata, image_path, args.dry_run).await;

    match result {
        Ok(ref r) => print_success(r, &metadata, image_path, &args, quiet)?,
        Err(ref e) => {
            if args.json {
                let json = serde_json::json!({
                    "error": true,
                    "identifier": identifier,
                    "message": e.to_string(),
                });
                eprintln!("{}", serde_json::to_string(&json)?);
            }
        }
    }

    result.map(|_| ()).map_err(Into::into)
}

// ─── Spreadsheet ─────────────────────────────────────────────────────────────

/// A command-level error: `{"error": {…}}` on stderr and exit 1 with
/// `--json`, else returned for `main` to print.
fn command_error(err: IaError, json: bool) -> Result<()> {
    if json {
        ia_core::write_json_error(&err);
        std::process::exit(1);
    }
    Err(err.into())
}

/// One row's outcome.
struct RowResult {
    row: CollectionRow,
    result: ia_core::Result<CreateCollectionResult>,
    elapsed_ms: u64,
}

/// `ia collection create --spreadsheet <FILE>`: validate every row, skip
/// the rows the job log records as created, then create the rest
/// concurrently up to `jobs`. Exits 1 when any row failed.
async fn run_spreadsheet(
    client: &IaClient,
    args: &CreateArgs,
    path: &Path,
    quiet: u8,
    jobs: usize,
    joblog: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    let defaults = match flag_metadata(args) {
        Ok(defaults) => defaults,
        Err(e) => {
            return command_error(IaError::InvalidArgument(format!("{e:#}")), args.json);
        }
    };
    let mut rows = match read_collection_spreadsheet(path, &defaults, args.image.as_deref()) {
        Ok(rows) => rows,
        // A validation problem names the rows; a read error (missing file,
        // unsupported extension, malformed CSV) is given the file's name.
        Err(e) if args.json || is_validation_error(&e) => return command_error(e, args.json),
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("failed to read spreadsheet: {}", path.display())))
        }
    };

    if let (Some(log), false) = (&joblog, no_resume) {
        if log.exists() {
            let done = match joblog::read(log) {
                Ok(entries) => joblog::successful_items(&entries, JOBLOG_OP),
                Err(e) => return command_error(e, args.json),
            };
            let before = rows.len();
            rows.retain(|row| !done.contains(&row.identifier));
            let skipped = before - rows.len();
            if skipped > 0 && quiet == 0 {
                eprintln!(
                    "{} skipping {skipped} collection(s) already created in joblog",
                    style("resuming:").cyan().bold(),
                );
            }
        }
    }

    if args.dry_run {
        for row in &rows {
            let result = create_collection(
                client,
                &row.identifier,
                &row.metadata,
                row.image.as_deref(),
                true,
            )
            .await?;
            print_success(&result, &row.metadata, row.image.as_deref(), args, quiet)?;
        }
        if !args.json && quiet < 2 {
            eprintln!(
                "{} would create {} collection(s), nothing sent",
                style("dry-run:").yellow().bold(),
                rows.len()
            );
        }
        return Ok(());
    }
    if rows.is_empty() {
        // Every row is already created according to the job log.
        return Ok(());
    }

    // Checked once, so missing keys stop the command rather than failing
    // every row.
    if let Err(e) = client.require_auth() {
        return command_error(e, args.json);
    }

    let writer = match joblog.as_deref().map(JoblogWriter::open).transpose() {
        Ok(writer) => writer,
        Err(e) => return command_error(e, args.json),
    };

    let total = rows.len();
    let mut results = stream::iter(rows)
        .map(|row| async move {
            let start = Instant::now();
            let result = create_collection(
                client,
                &row.identifier,
                &row.metadata,
                row.image.as_deref(),
                false,
            )
            .await;
            RowResult {
                row,
                result,
                elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            }
        })
        .buffer_unordered(jobs);

    let (mut created, mut failed) = (0usize, 0usize);
    let mut print_error = None;
    while let Some(done) = results.next().await {
        // The entry is written before the result is printed, so a created
        // collection is recorded even when printing fails.
        if let Some(writer) = &writer {
            let entry = JoblogEntry::new(JOBLOG_OP, &done.row.identifier, "");
            writer.write(&match &done.result {
                Ok(_) => entry.ok(0, done.elapsed_ms),
                Err(e) => entry.error(&format_error_chain(e), 0),
            });
        }
        if done.result.is_ok() {
            created += 1;
        } else {
            failed += 1;
        }
        if print_error.is_none() {
            print_error = print_row(&done, args, quiet).err();
        }
    }
    if let Some(e) = print_error {
        return Err(e);
    }

    if total > 1 && !args.json && quiet < 2 {
        eprintln!("{created} created, {failed} failed");
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Print one row's result: the single form's success output, or the error
/// (`{"identifier", "error": {…}}` on stdout with `--json`; else
/// `error: <identifier>: <message>` on stderr unless `-qq`).
fn print_row(done: &RowResult, args: &CreateArgs, quiet: u8) -> Result<()> {
    match &done.result {
        Ok(result) => print_success(
            result,
            &done.row.metadata,
            done.row.image.as_deref(),
            args,
            quiet,
        ),
        Err(e) => {
            if args.json {
                let json = serde_json::json!({
                    "identifier": done.row.identifier,
                    "error": e.to_json_error().error,
                });
                println!("{}", serde_json::to_string(&json)?);
            } else if quiet < 2 {
                eprintln!(
                    "{} {}: {}",
                    style("error:").red().bold(),
                    done.row.identifier,
                    format_error_chain(e)
                );
            }
            Ok(())
        }
    }
}

fn print_success(
    result: &CreateCollectionResult,
    metadata: &[(String, String)],
    image: Option<&Path>,
    args: &CreateArgs,
    quiet: u8,
) -> Result<()> {
    if args.json {
        let mut json = serde_json::json!({
            "identifier": result.identifier,
            "status": result.status,
            "url": result.url,
        });
        if args.dry_run {
            // Include metadata in JSON dry-run output
            let meta_obj = metadata_json(metadata);
            json["mediatype"] = serde_json::Value::String("collection".into());
            json["metadata"] = serde_json::Value::Object(meta_obj);
            if let Some(img) = image {
                json["image"] = serde_json::Value::String(img.display().to_string());
            }
        }
        println!("{}", serde_json::to_string(&json)?);
    } else if quiet == 0 {
        if args.dry_run {
            println!("{} {}", style("dry-run:").yellow().bold(), result.url);
            println!();
            println!("  {:<14} {}", style("identifier:").dim(), result.identifier);
            println!("  {:<14} collection", style("mediatype:").dim());
            for (key, value) in metadata.iter().filter(|(k, _)| k != "mediatype") {
                println!("  {:<14} {}", style(format!("{key}:")).dim(), value);
            }
            if let Some(img) = image {
                println!("  {:<14} {}", style("image:").dim(), img.display());
            }
        } else {
            println!("{} {}", style("created:").green().bold(), result.url);
        }
    }
    Ok(())
}

/// Metadata as a JSON object, as it will be sent: a key given once is a
/// string, a key given more than once is an array of its values in order.
/// A `mediatype` is left out: the create drops it and sends `collection`.
fn metadata_json(metadata: &[(String, String)]) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut map = serde_json::Map::new();
    for (key, value) in metadata.iter().filter(|(k, _)| k != "mediatype") {
        let value = Value::String(value.clone());
        match map.get_mut(key) {
            None => {
                map.insert(key.clone(), value);
            }
            Some(Value::Array(values)) => values.push(value),
            Some(first) => {
                let first = first.take();
                map.insert(key.clone(), Value::Array(vec![first, value]));
            }
        }
    }
    map
}

/// Whether `err` is the row check of `read_collection_spreadsheet`
/// (every problem, or no records) rather than a failure to read the file.
fn is_validation_error(err: &IaError) -> bool {
    matches!(err, IaError::Config(m)
        if m.starts_with("batch validation failed") || m == "spreadsheet contains no records")
}
