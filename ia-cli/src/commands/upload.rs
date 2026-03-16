use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::spreadsheet::read_spreadsheet;
use ia_core::upload::batch::{group_records, validate_groups};
use ia_core::upload::{
    generate_template, upload_batch, upload_item, write_template_csv, TemplateOpts, UploadOpts,
    UploadProgress, UploadResult, UploadStatus,
};
use ia_core::IaClient;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    long_about = "Upload files to the Internet Archive. Uploads one or more files to a single item, \
        with options for metadata, checksums, directory structure, and retry logic. Supports batch \
        uploads from spreadsheets and template generation for bulk workflows.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Upload a file to an existing or new item</dim>\
         \n  <bold>$ ia upload my-item file.pdf -m mediatype:texts -m collection:opensource</bold>\
         \n\n  <dim># Upload a directory, preserving structure</dim>\
         \n  <bold>$ ia upload my-item ./scans/ --keep-directories</bold>\
         \n\n  <dim># Upload from stdin with an explicit remote name</dim>\
         \n  <bold>$ cat data.csv | ia upload my-item - --remote-name data.csv</bold>\
         \n\n  <dim># Batch upload from a spreadsheet</dim>\
         \n  <bold>$ ia upload import batch.csv</bold>\
         \n\n  <dim># Generate a template spreadsheet from a directory</dim>\
         \n  <bold>$ ia upload template ./files/ -o template.csv</bold>\
         \n\n  <dim># Dry run — validate without uploading</dim>\
         \n  <bold>$ ia upload my-item file.pdf --dry-run</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct UploadArgs {
    /// Item identifier
    #[arg()]
    pub identifier: Option<String>,

    /// Files or directories to upload
    #[arg()]
    pub files: Vec<PathBuf>,

    /// Set metadata field (repeatable, KEY:VALUE)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Additional HTTP header (repeatable, KEY:VALUE)
    #[arg(long = "header")]
    pub header: Vec<String>,

    /// Explicit remote filename (required for stdin)
    #[arg(long)]
    pub remote_name: Option<String>,

    /// Prepend path prefix to all remote filenames
    #[arg(long)]
    pub remote_dir: Option<String>,

    /// Preserve relative path structure
    #[arg(long)]
    pub keep_directories: bool,

    /// Skip derivative generation
    #[arg(long)]
    pub no_derive: bool,

    /// Don't keep old file versions
    #[arg(long)]
    pub no_backup: bool,

    /// Error if item doesn't already exist
    #[arg(long)]
    pub no_auto_make_bucket: bool,

    /// Skip Content-MD5 verification
    #[arg(long)]
    pub no_verify: bool,

    /// Don't send x-archive-size-hint
    #[arg(long)]
    pub no_size_hint: bool,

    /// Skip collection existence check
    #[arg(long)]
    pub no_collection_check: bool,

    /// Path to pre-computed MD5 checksums file
    #[arg(long)]
    pub checksums: Option<PathBuf>,

    /// Skip files already uploaded (MD5 match)
    #[arg(long)]
    pub skip_existing: bool,

    /// Delete local file after verified upload
    #[arg(long)]
    pub delete_after_upload: bool,

    /// Upload to test_collection (auto-removed after 30 days)
    #[arg(long)]
    pub test_item: bool,

    /// Open item in browser after upload
    #[arg(long)]
    pub open_after_upload: bool,

    /// Validate everything, upload nothing
    #[arg(long)]
    pub dry_run: bool,

    /// Retry attempts per file
    #[arg(long, default_value = "10")]
    pub retries: u32,

    /// Sleep between retries (seconds)
    #[arg(long, default_value = "30")]
    pub retry_sleep: u64,

    /// Output results as JSONL
    #[arg(long)]
    pub json: bool,

    /// Use multipart upload (recommended for files >5 GB)
    #[arg(long)]
    pub multipart: bool,

    /// Full-screen TUI dashboard for monitoring upload progress
    #[arg(long)]
    pub dashboard: bool,

    #[command(subcommand)]
    pub command: Option<UploadCommand>,
}

#[derive(Debug, Subcommand)]
pub enum UploadCommand {
    /// Batch upload from a spreadsheet (CSV/TSV/XLSX/ODS/JSONL)
    #[command(
        long_about = "Upload items in batch from a spreadsheet file. Each row specifies an \
            identifier, a file path, and optional metadata columns. Rows sharing the same \
            identifier are grouped into a single item upload.\n\n\
            Required columns: identifier, file\n\
            All other columns become metadata key-value pairs.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Batch upload from CSV</dim>\
             \n  <bold>$ ia upload import batch.csv</bold>\
             \n\n  <dim># Batch upload from XLSX with dry run</dim>\
             \n  <bold>$ ia upload import batch.xlsx --dry-run</bold>\
             \n\n  <dim># Batch upload with JSON output</dim>\
             \n  <bold>$ ia upload import batch.csv --json</bold>\n"
        ),
    )]
    Import(ImportArgs),

    /// Generate a template spreadsheet from a directory
    #[command(
        long_about = "Scan a directory and generate a template spreadsheet with one row per file. \
            The template includes columns for identifier, file, mediatype, collection, title, \
            and other common metadata fields. Fill in the template and use 'ia upload import' \
            to perform the batch upload.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Generate CSV template to stdout</dim>\
             \n  <bold>$ ia upload template ./files/</bold>\
             \n\n  <dim># Generate template to file with auto-identifiers from filenames</dim>\
             \n  <bold>$ ia upload template ./files/ -o template.csv --identifier-from-filename</bold>\
             \n\n  <dim># Generate template with identifier prefix</dim>\
             \n  <bold>$ ia upload template ./files/ -o template.csv --identifier-prefix myproject</bold>\n"
        ),
    )]
    Template(TemplateArgs),

    /// Clean up incomplete multipart uploads
    #[command(
        long_about = "List or abort incomplete multipart uploads for an item. \
            Use this to clean up uploads that were interrupted or abandoned.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># List all incomplete uploads for an item</dim>\
             \n  <bold>$ ia upload cleanup my-item</bold>\
             \n\n  <dim># Abort a specific file's upload</dim>\
             \n  <bold>$ ia upload cleanup my-item file.zip</bold>\
             \n\n  <dim># Abort all incomplete uploads</dim>\
             \n  <bold>$ ia upload cleanup my-item --abort-all</bold>\n"
        ),
    )]
    Cleanup(CleanupArgs),
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to spreadsheet file
    #[arg()]
    pub spreadsheet: PathBuf,

    /// Set metadata field (repeatable, KEY:VALUE) — merged with spreadsheet data
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Additional HTTP header (repeatable, KEY:VALUE)
    #[arg(long = "header")]
    pub header: Vec<String>,

    /// Path to pre-computed MD5 checksums file
    #[arg(long)]
    pub checksums: Option<PathBuf>,

    /// Skip derivative generation
    #[arg(long)]
    pub no_derive: bool,

    /// Don't keep old file versions
    #[arg(long)]
    pub no_backup: bool,

    /// Error if item doesn't already exist
    #[arg(long)]
    pub no_auto_make_bucket: bool,

    /// Skip Content-MD5 verification
    #[arg(long)]
    pub no_verify: bool,

    /// Don't send x-archive-size-hint
    #[arg(long)]
    pub no_size_hint: bool,

    /// Skip collection existence check
    #[arg(long)]
    pub no_collection_check: bool,

    /// Skip files already uploaded (MD5 match)
    #[arg(long)]
    pub skip_existing: bool,

    /// Delete local file after verified upload
    #[arg(long)]
    pub delete_after_upload: bool,

    /// Upload to test_collection (auto-removed after 30 days)
    #[arg(long)]
    pub test_item: bool,

    /// Use multipart upload (recommended for files >5 GB)
    #[arg(long)]
    pub multipart: bool,

    /// Validate everything, upload nothing
    #[arg(long)]
    pub dry_run: bool,

    /// Retry attempts per file
    #[arg(long, default_value = "10")]
    pub retries: u32,

    /// Sleep between retries (seconds)
    #[arg(long, default_value = "30")]
    pub retry_sleep: u64,

    /// Output results as JSONL
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum TemplateFormat {
    Csv,
    Tsv,
    Xlsx,
}

#[derive(Debug, Args)]
pub struct TemplateArgs {
    /// Directory to scan for files
    #[arg()]
    pub dir: PathBuf,

    /// Output file (default: stdout)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Output format
    #[arg(long, default_value = "csv", value_enum)]
    pub format: TemplateFormat,

    /// Prefix to prepend to generated identifiers
    #[arg(long)]
    pub identifier_prefix: Option<String>,

    /// Generate identifiers from filenames
    #[arg(long)]
    pub identifier_from_filename: bool,

    /// Generate identifiers from parent directory names
    #[arg(long)]
    pub identifier_from_dirname: bool,

    /// Output template as JSONL
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// Item identifier
    #[arg()]
    pub identifier: String,

    /// Specific file to clean up
    #[arg()]
    pub file: Option<String>,

    /// Abort all incomplete uploads without confirmation
    #[arg(long)]
    pub abort_all: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Run ─────────────────────────────────────────────────────────────────────

pub async fn run(
    client: &IaClient,
    args: UploadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    if args.json && args.dashboard {
        bail!("--json and --dashboard are mutually exclusive");
    }
    #[cfg(feature = "tui")]
    if args.dashboard {
        match &args.command {
            None => {
                // Dashboard supported for bare upload — handled in run_bare_upload
            }
            Some(UploadCommand::Template(_) | UploadCommand::Cleanup(_)) => {
                bail!("--dashboard is only supported for bare upload and import");
            }
            Some(UploadCommand::Import(_)) => {
                // Dashboard supported for import — handled in run_import
            }
        }
    }

    #[cfg(not(feature = "tui"))]
    if args.dashboard {
        bail!(
            "Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui"
        );
    }

    // --retry-failed is only supported for import (batch) uploads
    if retry_failed && !matches!(args.command, Some(UploadCommand::Import(_))) {
        bail!(
            "--retry-failed is only supported with 'ia upload import'.\n\
             For single-item uploads, re-run the same upload command."
        );
    }

    match args.command {
        Some(UploadCommand::Import(sub)) => {
            run_import(
                client,
                sub,
                quiet,
                jobs,
                joblog_path,
                args.dashboard,
                retry_failed,
            )
            .await
        }
        Some(UploadCommand::Template(sub)) => run_template(sub),
        Some(UploadCommand::Cleanup(sub)) => run_cleanup(client, sub).await,
        None => run_bare_upload(client, args, quiet, joblog_path).await,
    }
}

// ─── Bare upload ─────────────────────────────────────────────────────────────

async fn run_bare_upload(
    client: &IaClient,
    args: UploadArgs,
    quiet: u8,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let identifier = args
        .identifier
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("identifier is required for upload"))?;

    if args.files.is_empty() {
        bail!("no files provided. Pass file paths as arguments, or use '-' for stdin.");
    }

    // Parse -m metadata as key:value pairs
    let metadata = parse_key_values(&args.metadata)?;

    // Parse --header as key:value pairs
    let headers = parse_key_values(&args.header)?;

    // --delete-after-upload forces verify — non-negotiable data safety invariant
    if args.delete_after_upload && args.no_verify {
        bail!(
            "--delete-after-upload requires verification (Content-MD5).\n\
             Cannot combine with --no-verify — refusing to delete local files \
             without server-side integrity confirmation."
        );
    }

    let checksums = args
        .checksums
        .as_ref()
        .map(|p| load_checksums(p))
        .transpose()?;

    // Handle stdin ('-' as file arg)
    let (files, _temp_file) = handle_stdin_files(&args.files, args.remote_name.as_deref())?;

    // Build UploadOpts
    let opts = UploadOpts {
        metadata,
        remote_name: args.remote_name.clone(),
        remote_dir: args.remote_dir.clone(),
        keep_directories: args.keep_directories,
        verify: !args.no_verify,
        skip_existing: args.skip_existing,
        checksums,
        delete_after_upload: args.delete_after_upload,
        no_derive: args.no_derive,
        no_backup: args.no_backup,
        no_auto_make_bucket: args.no_auto_make_bucket,
        no_size_hint: args.no_size_hint,
        no_collection_check: args.no_collection_check,
        test_item: args.test_item,
        multipart: args.multipart,
        retries: args.retries,
        retry_sleep: Duration::from_secs(args.retry_sleep),
        headers,
        dry_run: args.dry_run,
    };

    // Dry run (interactive): validate and print what would be uploaded
    if opts.dry_run && !args.json {
        let results = upload_item(client, identifier, &files, &opts, None)
            .await
            .context(format!("failed to validate upload to {identifier}"))?;
        print_dry_run_results(identifier, &results, &opts.metadata);
        return Ok(());
    }

    // Dashboard mode — hand off to the TUI and return early
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_upload_tui(
            client,
            vec![identifier.to_string()],
            vec![files.clone()],
            opts,
            1,
        )
        .await;
    }

    // Open joblog writer if path provided
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Set up progress display
    let display: Option<std::sync::Arc<crate::output::UploadDisplay>> = if !args.json && quiet == 0
    {
        Some(std::sync::Arc::new(crate::output::UploadDisplay::new(
            identifier,
            opts.dry_run,
        )))
    } else {
        None
    };
    let progress_ref: Option<std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>> =
        display.as_ref().map(|d| {
            let d = std::sync::Arc::clone(d);
            std::sync::Arc::new(move |p: UploadProgress| {
                d.update(p);
            }) as std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>
        });

    let results = upload_item(client, identifier, &files, &opts, progress_ref)
        .await
        .context(format!("failed to upload to {identifier}"))?;

    // Finish display (prints summary)
    if let Some(d) = &display {
        d.finish();
    }

    // Handle results: JSON output, joblog, failure detection
    let had_failure = if args.json {
        output_results(&results, true, quiet, joblog.as_ref())?
    } else {
        let failure = check_failures_and_log(&results, joblog.as_ref());
        // quiet==1 summary (display handles quiet==0)
        if quiet == 1 {
            let (uploaded, skipped, failed, total_bytes) = summarize_results(&results);
            let total_ms: u64 = results.iter().map(|r| r.elapsed_ms).max().unwrap_or(0);
            eprintln!(
                "{}  {} uploaded, {} skipped, {} failed ({}) in {:.1}s",
                identifier,
                uploaded,
                skipped,
                failed,
                crate::output::format_bytes(total_bytes),
                total_ms as f64 / 1000.0,
            );
        }
        failure
    };

    // Open in browser if requested
    if args.open_after_upload && !had_failure {
        let url = format!("https://archive.org/details/{identifier}");
        let result = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&url).spawn()
        } else if cfg!(target_os = "linux") {
            std::process::Command::new("xdg-open").arg(&url).spawn()
        } else if cfg!(target_os = "windows") {
            std::process::Command::new("cmd")
                .args(["/C", "start", &url])
                .spawn()
        } else {
            eprintln!("--open-after-upload is not supported on this platform");
            return Ok(());
        };
        if let Err(e) = result {
            eprintln!("failed to open browser: {e}");
        }
    }

    if had_failure {
        std::process::exit(1);
    }

    Ok(())
}

// ─── Import ──────────────────────────────────────────────────────────────────

async fn run_import(
    client: &IaClient,
    args: ImportArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    dashboard: bool,
    retry_failed: bool,
) -> Result<()> {
    let records = read_spreadsheet(&args.spreadsheet).context(format!(
        "failed to read spreadsheet: {}",
        args.spreadsheet.display()
    ))?;

    if records.is_empty() {
        bail!("spreadsheet is empty — no records to upload");
    }

    // --retry-failed: filter to only items that failed in a previous run
    let records = if retry_failed {
        let jl_path = joblog_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!("--retry-failed requires --joblog <path> so we know which items failed")
        })?;
        let entries = ia_core::joblog::read(jl_path)
            .context(format!("failed to read joblog: {}", jl_path.display()))?;
        let failed = ia_core::joblog::failed_items(&entries);
        if failed.is_empty() {
            eprintln!("No failed items found in joblog — nothing to retry.");
            return Ok(());
        }
        let failed_set: std::collections::HashSet<&str> =
            failed.iter().map(|s| s.as_str()).collect();
        let filtered: Vec<_> = records
            .into_iter()
            .filter(|(id, _)| failed_set.contains(id.as_str()))
            .collect();
        if filtered.is_empty() {
            eprintln!("No matching records found in spreadsheet for failed items.");
            return Ok(());
        }
        filtered
    } else {
        records
    };

    // Parse extra -m metadata and --header
    let extra_metadata = parse_key_values(&args.metadata)?;
    let headers = parse_key_values(&args.header)?;

    // --delete-after-upload forces verify — non-negotiable data safety invariant
    if args.delete_after_upload && args.no_verify {
        bail!(
            "--delete-after-upload requires verification (Content-MD5).\n\
             Cannot combine with --no-verify — refusing to delete local files \
             without server-side integrity confirmation."
        );
    }

    let checksums = args
        .checksums
        .as_ref()
        .map(|p| load_checksums(p))
        .transpose()?;

    let opts = UploadOpts {
        metadata: extra_metadata,
        headers,
        checksums,
        verify: !args.no_verify,
        skip_existing: args.skip_existing,
        delete_after_upload: args.delete_after_upload,
        no_derive: args.no_derive,
        no_backup: args.no_backup,
        no_auto_make_bucket: args.no_auto_make_bucket,
        no_size_hint: args.no_size_hint,
        no_collection_check: args.no_collection_check,
        test_item: args.test_item,
        multipart: args.multipart,
        retries: args.retries,
        retry_sleep: Duration::from_secs(args.retry_sleep),
        dry_run: args.dry_run,
        ..UploadOpts::default()
    };

    // Dry run (interactive): validate and print what would be uploaded
    if opts.dry_run && !args.json {
        return run_import_dry_run(client, records, &opts).await;
    }

    // Dashboard mode — hand off to the TUI and return early
    #[cfg(feature = "tui")]
    if dashboard {
        return crate::tui::run_upload_batch_tui(client, records, opts, jobs).await;
    }
    #[cfg(not(feature = "tui"))]
    let _ = dashboard;

    // Open joblog writer if path provided
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    let json_mode = args.json;

    // Count unique identifiers for the batch display header
    let item_count = {
        let mut ids = std::collections::HashSet::new();
        for (id, _) in &records {
            ids.insert(id.as_str());
        }
        ids.len()
    };

    // Set up batch progress display
    let batch_display: Option<std::sync::Arc<crate::output::UploadBatchDisplay>> =
        if !json_mode && quiet == 0 {
            Some(std::sync::Arc::new(crate::output::UploadBatchDisplay::new(
                item_count,
                jobs,
                retry_failed,
            )))
        } else {
            None
        };
    let progress_ref: Option<std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>> =
        batch_display.as_ref().map(|bd| {
            let bd = std::sync::Arc::clone(bd);
            std::sync::Arc::new(move |p: UploadProgress| {
                bd.update(p);
            }) as std::sync::Arc<dyn Fn(UploadProgress) + Send + Sync>
        });

    let start = std::time::Instant::now();
    let results = upload_batch(client, records, &opts, jobs, progress_ref)
        .await
        .context("batch upload failed")?;
    let elapsed = start.elapsed();

    // Finish batch display (prints summary)
    if let Some(bd) = &batch_display {
        bd.finish(&results, elapsed);
    }

    // Handle results: JSON output, joblog, failure detection
    let had_failure = if json_mode {
        output_results(&results, true, quiet, joblog.as_ref())?
    } else {
        let failure = check_failures_and_log(&results, joblog.as_ref());
        // quiet==1 summary (batch display handles quiet==0)
        if quiet == 1 {
            let (uploaded, skipped, failed, total_bytes) = summarize_results(&results);
            eprintln!(
                "{} {} uploaded, {} skipped, {} failed ({})",
                if failure {
                    style("done").red().bold().to_string()
                } else {
                    style("done").green().bold().to_string()
                },
                uploaded,
                skipped,
                failed,
                crate::output::format_bytes(total_bytes),
            );
        }
        failure
    };

    if had_failure {
        std::process::exit(1);
    }

    Ok(())
}

// ─── Template ────────────────────────────────────────────────────────────────

// ─── Dry-run display ─────────────────────────────────────────────────────────

/// Dry-run for batch import: validate groups and print what would be uploaded.
async fn run_import_dry_run(
    client: &IaClient,
    records: Vec<ia_core::spreadsheet::SpreadsheetRecord>,
    opts: &UploadOpts,
) -> Result<()> {
    let groups = group_records(records)?;
    validate_groups(&groups)?;

    // Check collections exist (unless --no-collection-check)
    if !opts.no_collection_check {
        let collections: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.metadata.iter())
            .filter(|(k, _)| k == "collection")
            .map(|(_, v)| v.as_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !collections.is_empty() {
            ia_core::upload::validate::check_collections(client, &collections).await?;
        }
    }

    // Compute totals and per-group file info
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    struct GroupInfo {
        group_bytes: u64,
        file_details: Vec<(String, u64)>,
    }

    let mut infos: Vec<GroupInfo> = Vec::new();
    for group in &groups {
        let mut group_bytes = 0u64;
        let mut file_details = Vec::new();
        total_files += group.files.len();

        for file in &group.files {
            let size = std::fs::metadata(file)
                .map(|m| m.len())
                .context(format!("cannot read file: {}", file.display()))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.display().to_string());
            file_details.push((name, size));
            group_bytes += size;
        }
        total_bytes += group_bytes;
        infos.push(GroupInfo {
            group_bytes,
            file_details,
        });
    }

    // Header
    eprintln!(
        "{} {} — {} items, {} files ({})",
        style("⊘").dim(),
        style("Dry run").bold(),
        style(groups.len()).bold(),
        total_files,
        crate::output::format_bytes(total_bytes),
    );

    // Per-item details
    for (group, info) in groups.iter().zip(infos.iter()) {
        eprintln!();
        let file_word = if group.files.len() == 1 {
            "file"
        } else {
            "files"
        };
        eprintln!(
            "  {}  {} {}  {}",
            style(&group.identifier).bold(),
            group.files.len(),
            style(file_word).dim(),
            style(crate::output::format_bytes(info.group_bytes)).dim(),
        );

        // Metadata (merged: CLI opts overridden by spreadsheet per-group)
        let merged = merge_metadata(&opts.metadata, &group.metadata);
        print_metadata(&merged, 4);

        // Files
        for (name, size) in &info.file_details {
            eprintln!(
                "    {} {}  {}",
                style("→").dim(),
                name,
                style(crate::output::format_bytes(*size)).dim(),
            );
        }

        eprintln!(
            "    {}",
            style(format!("https://archive.org/details/{}", group.identifier)).dim(),
        );
    }

    eprintln!();
    eprintln!("{}", style("Validation passed.").green());
    Ok(())
}

/// Print dry-run results for a single-item upload.
fn print_dry_run_results(
    identifier: &str,
    results: &[UploadResult],
    metadata: &[(String, String)],
) {
    let total_bytes: u64 = results.iter().map(|r| r.bytes).sum();
    let file_word = if results.len() == 1 { "file" } else { "files" };
    eprintln!(
        "{} {} — {} {} ({}) → {}",
        style("⊘").dim(),
        style("Dry run").bold(),
        results.len(),
        file_word,
        crate::output::format_bytes(total_bytes),
        style(identifier).bold(),
    );
    eprintln!();

    // Metadata
    print_metadata(metadata, 2);

    // Files with remote keys
    for r in results {
        eprintln!(
            "  {} {}  {}",
            style("→").dim(),
            r.key,
            style(crate::output::format_bytes(r.bytes)).dim(),
        );
    }

    eprintln!(
        "  {}",
        style(format!("https://archive.org/details/{identifier}")).dim(),
    );
    eprintln!();
    eprintln!("{}", style("Validation passed.").green());
}

/// Print metadata key-value pairs with aligned columns.
fn print_metadata(metadata: &[(String, String)], indent: usize) {
    if metadata.is_empty() {
        return;
    }
    let max_key_len = metadata.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let pad = " ".repeat(indent);
    for (key, value) in metadata {
        eprintln!(
            "{pad}{:<width$}  {}",
            style(format!("{key}:")).dim(),
            value,
            width = max_key_len + 1, // +1 for the colon
        );
    }
}

/// Merge CLI metadata with per-item metadata (per-item overrides CLI for same key).
fn merge_metadata(
    cli_metadata: &[(String, String)],
    item_metadata: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = cli_metadata.to_vec();
    for (key, value) in item_metadata {
        if let Some(existing) = merged.iter_mut().find(|(k, _)| k == key) {
            existing.1 = value.clone();
        } else {
            merged.push((key.clone(), value.clone()));
        }
    }
    merged
}

fn run_template(args: TemplateArgs) -> Result<()> {
    let template_opts = TemplateOpts {
        identifier_prefix: args.identifier_prefix,
        identifier_from_filename: args.identifier_from_filename,
        identifier_from_dirname: args.identifier_from_dirname,
    };

    let rows = generate_template(&args.dir, &template_opts)
        .context(format!("failed to scan directory: {}", args.dir.display()))?;

    if rows.is_empty() {
        bail!("no files found in {}", args.dir.display());
    }

    if args.json {
        for row in &rows {
            println!(
                "{}",
                serde_json::to_string(row).context("failed to serialize template row")?
            );
        }
        return Ok(());
    }

    match args.format {
        TemplateFormat::Csv | TemplateFormat::Tsv => {
            let is_tsv = matches!(args.format, TemplateFormat::Tsv);
            if let Some(ref output_path) = args.output {
                let file = std::fs::File::create(output_path)
                    .context(format!("failed to create {}", output_path.display()))?;
                let mut writer = std::io::BufWriter::new(file);
                if is_tsv {
                    write_template_tsv(&rows, &mut writer)?;
                } else {
                    write_template_csv(&rows, &mut writer)
                        .context("failed to write CSV template")?;
                }
                eprintln!(
                    "{} Wrote {} rows to {}",
                    style("✓").green(),
                    rows.len(),
                    output_path.display(),
                );
            } else {
                let stdout = std::io::stdout();
                let mut writer = std::io::BufWriter::new(stdout.lock());
                if is_tsv {
                    write_template_tsv(&rows, &mut writer)?;
                } else {
                    write_template_csv(&rows, &mut writer)
                        .context("failed to write CSV template")?;
                }
            }
        }
        TemplateFormat::Xlsx => {
            let output_path = args
                .output
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--output is required for xlsx format"))?;
            write_template_xlsx(&rows, output_path)?;
            eprintln!(
                "{} Wrote {} rows to {}",
                style("✓").green(),
                rows.len(),
                output_path.display(),
            );
        }
    }

    Ok(())
}

// ─── Cleanup ─────────────────────────────────────────────────────────────────

async fn run_cleanup(client: &IaClient, args: CleanupArgs) -> Result<()> {
    let uploads = ia_core::upload::multipart::list_uploads(client, &args.identifier).await?;

    if uploads.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!(
                "{} No incomplete multipart uploads for {}",
                style("✓").green(),
                args.identifier,
            );
        }
        return Ok(());
    }

    // Filter by file if specified
    let targets: Vec<_> = if let Some(ref file) = args.file {
        uploads.into_iter().filter(|u| u.key == *file).collect()
    } else {
        uploads
    };

    if targets.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!(
                "{} No incomplete uploads matching '{}' for {}",
                style("✓").green(),
                args.file.as_deref().unwrap_or(""),
                args.identifier,
            );
        }
        return Ok(());
    }

    // List mode: no file and no --abort-all → just list
    if args.file.is_none() && !args.abort_all {
        if args.json {
            let json = serde_json::to_string(&targets)?;
            println!("{json}");
        } else {
            eprintln!(
                "{} {} incomplete multipart upload(s) for {}:",
                style("▸").cyan(),
                targets.len(),
                args.identifier,
            );
            for u in &targets {
                eprintln!("  {} {} (initiated: {})", u.upload_id, u.key, u.initiated,);
            }
            eprintln!("\nUse --abort-all or specify a file to abort.");
        }
        return Ok(());
    }

    // Abort mode
    for u in &targets {
        ia_core::upload::multipart::abort_upload(client, &args.identifier, &u.key, &u.upload_id)
            .await?;
        if args.json {
            let json = serde_json::json!({
                "action": "aborted",
                "identifier": args.identifier,
                "key": u.key,
                "upload_id": u.upload_id,
            });
            println!("{}", serde_json::to_string(&json)?);
        } else {
            eprintln!(
                " {} aborted {}/{}",
                style("✓").green(),
                args.identifier,
                u.key,
            );
        }
    }

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Write results to joblog and return whether any file failed.
fn check_failures_and_log(results: &[UploadResult], joblog: Option<&JoblogWriter>) -> bool {
    let mut had_failure = false;
    for r in results {
        if matches!(r.status, UploadStatus::Failed(_)) {
            had_failure = true;
        }
        if let Some(jl) = joblog {
            write_upload_result(jl, r);
        }
    }
    had_failure
}

/// Output upload results: print per-line (JSON or human), write to joblog.
/// Returns whether any file failed.
fn output_results(
    results: &[UploadResult],
    json_mode: bool,
    quiet: u8,
    joblog: Option<&JoblogWriter>,
) -> Result<bool> {
    let mut had_failure = false;
    for r in results {
        if matches!(r.status, UploadStatus::Failed(_)) {
            had_failure = true;
        }

        if json_mode {
            match &r.status {
                UploadStatus::Failed(msg) => {
                    // Use project error convention for failures → stderr
                    let err_json = serde_json::json!({
                        "error": {
                            "code": "upload_failed",
                            "message": msg,
                            "identifier": r.identifier,
                            "key": r.key,
                        }
                    });
                    eprintln!("{}", serde_json::to_string(&err_json).unwrap_or_default());
                }
                _ => {
                    // Success/skip/dry-run go to stdout as JSONL
                    let json =
                        serde_json::to_string(r).context("failed to serialize upload result")?;
                    println!("{json}");
                }
            }
        } else if quiet == 0 {
            print_result_line(r);
        }

        if let Some(jl) = joblog {
            write_upload_result(jl, r);
        }
    }
    Ok(had_failure)
}

/// Compute summary counts from upload results.
fn summarize_results(results: &[UploadResult]) -> (usize, usize, usize, u64) {
    let uploaded = results
        .iter()
        .filter(|r| matches!(r.status, UploadStatus::Uploaded))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, UploadStatus::Skipped))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, UploadStatus::Failed(_)))
        .count();
    let total_bytes: u64 = results
        .iter()
        .filter(|r| matches!(r.status, UploadStatus::Uploaded))
        .map(|r| r.bytes)
        .sum();
    (uploaded, skipped, failed, total_bytes)
}

/// Load and parse a checksums file (one `MD5  filename` per line).
fn load_checksums(path: &std::path::Path) -> Result<std::collections::HashMap<String, String>> {
    let content = std::fs::read_to_string(path)
        .context(format!("failed to read checksums file: {}", path.display()))?;
    Ok(ia_core::upload::checksum::parse_checksums(&content))
}

/// Parse a list of `KEY:VALUE` strings into `(String, String)` pairs.
///
/// Splits on the first `:` — values may contain additional colons.
fn parse_key_values(items: &[String]) -> Result<Vec<(String, String)>> {
    items
        .iter()
        .map(|s| {
            let (key, value) = s
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("invalid KEY:VALUE format: {s}"))?;
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Handle stdin ('-') as a file argument.
///
/// If any file path is `-`, reads stdin into a temp file and substitutes it.
/// Returns the list of real file paths and optionally the temp file handle
/// (to keep it alive until upload completes).
fn handle_stdin_files(
    files: &[PathBuf],
    remote_name: Option<&str>,
) -> Result<(Vec<PathBuf>, Option<tempfile::NamedTempFile>)> {
    let has_stdin = files.iter().any(|f| f.as_os_str() == "-");
    if !has_stdin {
        return Ok((files.to_vec(), None));
    }

    if remote_name.is_none() {
        bail!("--remote-name is required when reading from stdin (-)");
    }

    if std::io::stdin().is_terminal() {
        bail!("stdin is a terminal — pipe data or use a file path instead of '-'");
    }

    let mut temp = tempfile::NamedTempFile::new().context("failed to create temp file")?;
    std::io::copy(&mut std::io::stdin().lock(), &mut temp).context("failed to read from stdin")?;

    let real_files: Vec<PathBuf> = files
        .iter()
        .map(|f| {
            if f.as_os_str() == "-" {
                temp.path().to_path_buf()
            } else {
                f.clone()
            }
        })
        .collect();

    Ok((real_files, Some(temp)))
}

/// Print a human-readable result line for a single file upload.
fn print_result_line(r: &UploadResult) {
    match &r.status {
        UploadStatus::Uploaded => {
            eprintln!(
                " {} {}/{} ({}, {:.1}s)",
                style("✓").green(),
                r.identifier,
                r.key,
                crate::output::format_bytes(r.bytes),
                r.elapsed_ms as f64 / 1000.0,
            );
        }
        UploadStatus::Skipped => {
            eprintln!(
                " {} {}/{} (skipped, already exists)",
                style("–").dim(),
                r.identifier,
                r.key,
            );
        }
        UploadStatus::Failed(msg) => {
            eprintln!(" {} {}/{}: {}", style("✗").red(), r.identifier, r.key, msg,);
        }
        UploadStatus::DryRun => {
            eprintln!(
                " {} {}/{} (dry run, {})",
                style("⊘").dim(),
                r.identifier,
                r.key,
                crate::output::format_bytes(r.bytes),
            );
        }
    }
}

/// Write an upload result to the joblog.
fn write_upload_result(jl: &JoblogWriter, r: &UploadResult) {
    let entry = JoblogEntry::new("upload", &r.identifier, &r.key);
    let entry = match &r.status {
        UploadStatus::Uploaded => entry.ok(r.bytes, r.elapsed_ms),
        UploadStatus::Skipped => entry.skipped(),
        UploadStatus::Failed(msg) => entry.error(msg, r.retries as usize),
        UploadStatus::DryRun => entry.skipped(), // dry runs logged as skipped
    };
    jl.write(&entry);
}

/// Write template rows as TSV.
fn write_template_tsv<W: std::io::Write>(
    rows: &[ia_core::upload::TemplateRow],
    writer: &mut W,
) -> Result<()> {
    let columns = [
        "identifier",
        "file",
        "REMOTE_NAME",
        "mediatype",
        "collection",
        "title",
        "creator",
        "date",
        "description",
        "subject",
        "language",
    ];
    writeln!(writer, "{}", columns.join("\t")).context("failed to write TSV header")?;
    for row in rows {
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.identifier,
            row.file,
            row.remote_name,
            row.mediatype,
            row.collection,
            row.title,
            row.creator,
            row.date,
            row.description,
            row.subject,
            row.language,
        )
        .context("failed to write TSV row")?;
    }
    Ok(())
}

/// Write template rows as XLSX using rust_xlsxwriter.
fn write_template_xlsx(
    rows: &[ia_core::upload::TemplateRow],
    path: &std::path::Path,
) -> Result<()> {
    use rust_xlsxwriter::{Workbook, XlsxError};

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    let columns = [
        "identifier",
        "file",
        "REMOTE_NAME",
        "mediatype",
        "collection",
        "title",
        "creator",
        "date",
        "description",
        "subject",
        "language",
    ];

    for (col, name) in columns.iter().enumerate() {
        worksheet
            .write_string(0, col as u16, *name)
            .map_err(|e: XlsxError| anyhow::anyhow!("XLSX write error: {e}"))?;
    }

    for (row_idx, row) in rows.iter().enumerate() {
        let r = (row_idx + 1) as u32;
        let fields = [
            &row.identifier,
            &row.file,
            &row.remote_name,
            &row.mediatype,
            &row.collection,
            &row.title,
            &row.creator,
            &row.date,
            &row.description,
            &row.subject,
            &row.language,
        ];
        for (col, val) in fields.iter().enumerate() {
            worksheet
                .write_string(r, col as u16, val.as_str())
                .map_err(|e: XlsxError| anyhow::anyhow!("XLSX write error: {e}"))?;
        }
    }

    workbook
        .save(path)
        .map_err(|e: XlsxError| anyhow::anyhow!("failed to save XLSX: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_values_basic() {
        let items = vec!["title:My Book".to_string(), "mediatype:texts".to_string()];
        let result = parse_key_values(&items).unwrap();
        assert_eq!(
            result,
            vec![
                ("title".to_string(), "My Book".to_string()),
                ("mediatype".to_string(), "texts".to_string()),
            ]
        );
    }

    #[test]
    fn parse_key_values_with_colon_in_value() {
        let items = vec!["description:foo:bar:baz".to_string()];
        let result = parse_key_values(&items).unwrap();
        assert_eq!(
            result,
            vec![("description".to_string(), "foo:bar:baz".to_string()),]
        );
    }

    #[test]
    fn parse_key_values_empty() {
        let items: Vec<String> = vec![];
        let result = parse_key_values(&items).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_key_values_invalid() {
        let items = vec!["no-colon-here".to_string()];
        let result = parse_key_values(&items);
        assert!(result.is_err());
    }

    #[test]
    fn print_result_line_uploaded() {
        // Smoke test — just ensure it doesn't panic
        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: Some("abc123".into()),
            elapsed_ms: 500,
            retries: 0,
        };
        print_result_line(&r);
    }

    #[test]
    fn print_result_line_skipped() {
        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Skipped,
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        };
        print_result_line(&r);
    }

    #[test]
    fn print_result_line_failed() {
        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Failed("connection reset".into()),
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 3,
        };
        print_result_line(&r);
    }

    #[test]
    fn print_result_line_dry_run() {
        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::DryRun,
            bytes: 4096,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        };
        print_result_line(&r);
    }

    #[test]
    fn handle_stdin_files_no_stdin() {
        let files = vec![PathBuf::from("/tmp/file.txt")];
        let (result, temp) = handle_stdin_files(&files, None).unwrap();
        assert_eq!(result, files);
        assert!(temp.is_none());
    }

    #[test]
    fn handle_stdin_files_requires_remote_name() {
        let files = vec![PathBuf::from("-")];
        let result = handle_stdin_files(&files, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--remote-name"));
    }

    #[test]
    fn write_upload_result_to_joblog() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");
        let jl = JoblogWriter::open(&path).unwrap();

        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: Some("abc123".into()),
            elapsed_ms: 500,
            retries: 0,
        };
        write_upload_result(&jl, &r);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"op\":\"upload\""));
        assert!(content.contains("\"item\":\"test-item\""));
        assert!(content.contains("\"status\":\"ok\""));
    }

    #[test]
    fn write_upload_result_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");
        let jl = JoblogWriter::open(&path).unwrap();

        let r = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Failed("timeout".into()),
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 3,
        };
        write_upload_result(&jl, &r);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"status\":\"error\""));
        assert!(content.contains("\"error\":\"timeout\""));
    }

    #[test]
    fn output_results_json_failed_uses_error_convention() {
        let results = vec![UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Failed("connection reset".into()),
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        }];

        // output_results writes to stdout/stderr — just verify it doesn't panic
        // and returns had_failure = true
        let had_failure = output_results(&results, true, 0, None).unwrap();
        assert!(had_failure);
    }

    #[test]
    fn output_results_json_success_goes_to_stdout() {
        let results = vec![UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: Some("abc123".into()),
            elapsed_ms: 500,
            retries: 0,
        }];

        let had_failure = output_results(&results, true, 0, None).unwrap();
        assert!(!had_failure);
    }

    #[test]
    fn merge_metadata_no_overlap() {
        let cli = vec![("mediatype".into(), "texts".into())];
        let item = vec![("title".into(), "My Book".into())];
        let merged = merge_metadata(&cli, &item);
        assert_eq!(
            merged,
            vec![
                ("mediatype".into(), "texts".into()),
                ("title".into(), "My Book".into()),
            ]
        );
    }

    #[test]
    fn merge_metadata_item_overrides_cli() {
        let cli = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "default-coll".into()),
        ];
        let item = vec![("collection".into(), "special-coll".into())];
        let merged = merge_metadata(&cli, &item);
        assert_eq!(
            merged,
            vec![
                ("mediatype".into(), "texts".into()),
                ("collection".into(), "special-coll".into()),
            ]
        );
    }

    #[test]
    fn merge_metadata_both_empty() {
        let merged = merge_metadata(&[], &[]);
        assert!(merged.is_empty());
    }

    #[test]
    fn merge_metadata_duplicate_cli_keys_first_wins() {
        // If CLI has duplicate keys, only the first is overridden by item metadata
        let cli = vec![
            ("subject".into(), "first".into()),
            ("subject".into(), "second".into()),
        ];
        let item = vec![("subject".into(), "override".into())];
        let merged = merge_metadata(&cli, &item);
        assert_eq!(
            merged,
            vec![
                ("subject".into(), "override".into()),
                ("subject".into(), "second".into()),
            ]
        );
    }
}
