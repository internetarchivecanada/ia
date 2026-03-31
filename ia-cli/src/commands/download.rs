use anyhow::{bail, Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;
use futures::{stream, StreamExt};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};

use ia_core::disk_pool::DiskPool;
use ia_core::download::{
    collect_batch_results, BatchDownloadResult, DownloadOpts, DownloadProgress, DownloadStatus,
    FileDownloadResult, ItemDownloadResult,
};
use ia_core::error::IaError;
use ia_core::files::FileFilter;
use ia_core::identifier::parse_identifier_line;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::search::SearchOpts;
use ia_core::types::FileSource;
use ia_core::IaClient;

use crate::output::DownloadDisplay;

#[derive(Args)]
#[command(
    long_about = "Download files from the Internet Archive. Downloads all files from an item, \
        or specific files when file names are given. Supports batch downloads via search queries, \
        item lists, or piped identifiers from stdin.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Download all files from an item</dim>\n  <bold>$ ia download nasa</bold>\
         \n\n  <dim># Download specific files</dim>\n  <bold>$ ia download nasa NASAarchiveLogo.jpg</bold>\
         \n\n  <dim># Download only MP4 files</dim>\n  <bold>$ ia download nasa --glob \"*.mp4\"</bold>\
         \n\n  <dim># Batch download from a search query</dim>\n  <bold>$ ia download --search \"collection:nasa AND mediatype:movies\"</bold>\
         \n\n  <dim># Batch download from piped identifiers</dim>\n  <bold>$ ia search -q collection:nasa --json | ia download</bold>\
         \n\n  <dim># Download with JSON output (for scripts/agents)</dim>\n  <bold>$ ia download nasa --json</bold>\n"
    ),
)]
pub struct DownloadArgs {
    /// Item identifier to download
    pub identifier: Option<String>,

    /// Specific file(s) to download from the item
    pub files: Vec<String>,

    /// File containing item identifiers (one per line)
    #[arg(long)]
    itemlist: Option<PathBuf>,

    /// Filter files by glob pattern (pipe-separated: "*.mp4|*.webm")
    #[arg(short = 'g', long)]
    glob: Option<String>,

    /// Exclude files matching pattern
    #[arg(short = 'e', long)]
    exclude: Option<String>,

    /// Filter by file format
    #[arg(short = 'f', long)]
    format: Vec<String>,

    /// Filter by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    source: Option<FileSource>,

    /// Exclude by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    exclude_source: Option<FileSource>,

    /// Destination directory (repeatable for disk pool)
    #[arg(long, default_value = ".")]
    destdir: Vec<PathBuf>,

    /// Don't create item subdirectory
    #[arg(long)]
    no_directories: bool,

    /// Verify checksums (slower, reads every local file)
    #[arg(short = 'C', long)]
    checksum: bool,

    /// Max retries per file
    #[arg(short = 'R', long, default_value = "5")]
    retries: usize,

    /// Don't set file modification times
    #[arg(long)]
    no_timestamps: bool,

    /// Show what would be downloaded without downloading
    #[arg(long)]
    dry_run: bool,

    /// Download items matching a search query (downloads each result)
    #[arg(short = 's', long)]
    search: Option<String>,

    /// Concurrent items for batch/search (use -j/--jobs for concurrent files)
    #[arg(long, default_value = "2")]
    pub items: usize,

    /// Full-screen dashboard mode
    #[arg(long)]
    pub dashboard: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!(
            "unknown source: {s} (expected: original, derivative, metadata)"
        )),
    }
}

/// Collect all identifiers from args, --itemlist file, --search, and stdin.
async fn collect_identifiers(args: &DownloadArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids: Vec<String> = args.identifier.iter().cloned().collect();

    if !args.files.is_empty() && ids.is_empty() {
        bail!("file names require an identifier: ia download <identifier> <file> [file ...]");
    }

    if let Some(path) = &args.itemlist {
        if !args.files.is_empty() {
            bail!("cannot combine file names with --itemlist (file names apply to a single item)");
        }
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            if let Some(id) = parse_identifier_line(line) {
                ids.push(id);
            }
        }
    }

    // --search: collect identifiers from search results
    if let Some(ref query) = args.search {
        if !args.files.is_empty() {
            bail!("cannot combine file names with --search (file names apply to a single item)");
        }
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // Read from stdin if no identifier and no itemlist and no search
    if ids.is_empty()
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

    Ok(ids)
}

pub async fn run(
    client: &IaClient,
    args: DownloadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    if args.json && args.dashboard {
        bail!("--json and --dashboard are mutually exclusive");
    }

    let mut identifiers = collect_identifiers(&args, client).await?;

    // Detect file paths passed as identifier and suggest --itemlist
    if let Some(ref id) = args.identifier {
        if std::path::Path::new(id).exists() && (id.contains('/') || id.contains('\\')) {
            bail!(
                "\"{}\" looks like a file path. Did you mean:\n  ia download --itemlist {}",
                id,
                id
            );
        }
    }

    // If --retry-failed, read joblog and use failed items as identifiers
    if retry_failed {
        if let Some(ref path) = joblog_path {
            let entries = ia_core::joblog::read(path)
                .context(format!("failed to read joblog: {}", path.display()))?;
            let failed = ia_core::joblog::failed_items(&entries);
            if failed.is_empty() {
                eprintln!("{} No failed items in joblog", style("✓").green());
                return Ok(());
            }
            identifiers = failed;
        } else {
            bail!("--retry-failed requires --joblog");
        }
    }

    if identifiers.is_empty() {
        bail!("no identifiers provided. Pass identifiers as arguments, use --itemlist, or pipe to stdin.");
    }

    // Open joblog writer if path provided
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Set up disk pool if multiple destdirs
    let destdirs = if args.destdir.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.destdir.clone()
    };

    // Validate destdir paths early so typos are caught before any downloads.
    for dir in &destdirs {
        if dir.exists() {
            if !dir.is_dir() {
                bail!("--destdir is not a directory: {}", dir.display());
            }
            // Quick writability check (use PID to avoid races between concurrent runs)
            let probe = dir.join(format!(".ia-probe-{}", std::process::id()));
            if let Err(e) = std::fs::File::create(&probe) {
                if e.kind() == std::io::ErrorKind::StorageFull {
                    bail!("--destdir disk is full: {} ({e})", dir.display());
                }
                bail!("--destdir is not writable: {} ({e})", dir.display());
            }
            let _ = std::fs::remove_file(&probe);
        } else {
            // Try to create the directory — fail fast if impossible
            std::fs::create_dir_all(dir).context(format!(
                "--destdir does not exist and cannot be created: {}",
                dir.display()
            ))?;
        }
    }

    let mut disk_pool = if destdirs.len() > 1 {
        Some(DiskPool::new(&destdirs).context("failed to initialize disk pool")?)
    } else {
        None
    };

    let base_destdir = destdirs
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));

    let filter = FileFilter {
        glob: args.glob.clone(),
        exclude: args.exclude.clone(),
        formats: args.format.clone(),
        source: args.source.clone(),
        exclude_source: args.exclude_source.clone(),
        names: args.files.clone(),
    };

    // Validate glob/exclude patterns early so the user gets a clear error
    // instead of silently downloading everything (or nothing).
    if let Err(msg) = ia_core::files::validate_filter(&filter) {
        bail!("{msg}");
    }

    let make_opts = |destdir: PathBuf| DownloadOpts {
        destdir,
        no_directories: args.no_directories,
        checksum: args.checksum,
        retries: args.retries,
        no_timestamps: args.no_timestamps,
        dry_run: args.dry_run,
        filter: filter.clone(),
    };

    let opts = make_opts(base_destdir.clone());

    let semaphore = Arc::new(Semaphore::new(jobs));

    // Dashboard mode
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_tui(
            client,
            &identifiers,
            &opts,
            Arc::clone(&semaphore),
            args.items,
        )
        .await;
    }

    #[cfg(not(feature = "tui"))]
    if args.dashboard {
        bail!(
            "Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui"
        );
    }

    // Single item — use the original simple path
    if identifiers.len() == 1 {
        let identifier = &identifiers[0];

        // Use disk pool to select destination if multi-disk
        let item_opts = if let Some(ref mut pool) = disk_pool {
            let dest = pool.assign_item(identifier, 0)?;
            make_opts(dest.to_path_buf())
        } else {
            opts.clone()
        };

        let display = if !args.json && quiet == 0 {
            Some(Arc::new(DownloadDisplay::new(identifier)))
        } else {
            None
        };

        let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
            display
                .clone()
                .map(|d| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                    Arc::new(move |p| d.update(p))
                });

        let result = ia_core::download::download_item(
            client,
            identifier,
            &item_opts,
            Arc::clone(&semaphore),
            progress,
        )
        .await
        .context(format!("failed to download {}", identifier))?;

        if let Some(d) = display {
            d.finish(&result, &item_opts.destdir);
        }

        if let Some(ref jl) = joblog {
            write_item_results(jl, identifier, &result.results);
        }

        if args.json {
            for r in &result.results {
                print_json_file_result(identifier, r);
            }
        } else if quiet == 1 {
            eprintln!(
                "{}  {} files ({}) in {:.1}s",
                identifier,
                result.files_downloaded,
                crate::output::format_bytes(result.bytes_total),
                result.elapsed.as_secs_f64(),
            );
        }

        if result.files_failed > 0 {
            std::process::exit(1);
        }

        return Ok(());
    }

    // Batch mode
    let json_mode = args.json;
    let items_concurrency = args.items;
    let batch_display = if !json_mode && quiet == 0 {
        Some(Arc::new(crate::output::BatchDisplay::new(
            identifiers.len(),
            jobs,
        )))
    } else {
        None
    };

    let has_disk_pool = disk_pool.is_some();
    let result =
        if let Some(pool) = disk_pool.take() {
            // Multi-disk mode: per-item loop with disk pool assignment.
            // We can't use download_batch() because it applies a single destdir
            // to all items. Instead, assign each item to a disk, then download
            // with per-item opts.
            let (batch_result, returned_pool) = download_batch_with_pool(
                client,
                identifiers,
                &make_opts,
                &filter,
                pool,
                semaphore,
                batch_display.clone(),
                json_mode,
                items_concurrency,
            )
            .await?;
            disk_pool = Some(returned_pool);
            batch_result
        } else {
            // Standard single-destdir batch path — delegate to core.
            let on_item_start: Option<ia_core::download::OnItemStartFn> = batch_display
                .clone()
                .map(|bd| -> ia_core::download::OnItemStartFn {
                    Arc::new(move |id, current, total| bd.on_item_start(id, current, total))
                });

            let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> = batch_display
                .clone()
                .map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                    Arc::new(move |p: DownloadProgress| bd.on_progress(p))
                });

            let on_item_complete: Option<ia_core::download::OnItemCompleteFn> = if json_mode {
                Some(Arc::new(move |result: &ItemDownloadResult| {
                    let obj = serde_json::json!({
                        "item": result.identifier,
                        "status": "ok",
                        "files_ok": result.files_downloaded,
                        "files_skipped": result.files_skipped,
                        "files_failed": result.files_failed,
                        "bytes": result.bytes_total,
                        "elapsed_ms": result.elapsed.as_millis() as u64,
                    });
                    println!("{}", obj);
                }))
            } else {
                batch_display
                    .clone()
                    .map(|bd| -> ia_core::download::OnItemCompleteFn {
                        Arc::new(move |result| bd.on_item_complete(result))
                    })
            };

            let on_item_error: Option<ia_core::download::OnItemErrorFn> = batch_display
                .clone()
                .map(|bd| -> ia_core::download::OnItemErrorFn {
                    Arc::new(move |id, err| bd.on_item_error(id, &err.to_string()))
                });

            ia_core::download::download_batch(
                client,
                identifiers,
                &opts,
                semaphore,
                progress,
                on_item_start,
                on_item_complete,
                on_item_error,
                items_concurrency,
            )
            .await
        };

    // Print JSON for failed items (on_item_complete only fires for Ok results)
    if json_mode {
        for item_result in &result.item_results {
            if item_result.is_err() {
                print_json_item_result(item_result);
            }
        }
    }

    // Write batch results to joblog
    if let Some(ref jl) = joblog {
        for item_result in &result.item_results {
            match item_result {
                Ok(ir) => write_item_results(jl, &ir.identifier, &ir.results),
                Err((id, err)) => {
                    jl.write(&JoblogEntry::new("download", id, "").error(&err.to_string(), 0));
                }
            }
        }
    }

    // Print summary
    if !json_mode && quiet < 2 {
        let disk_statuses = disk_pool.as_ref().map(|p| p.status());
        if let Some(ref bd) = batch_display {
            if has_disk_pool {
                bd.finish(&result, disk_statuses.as_deref());
            } else if let Some(free) = crate::output::disk_space_free(&base_destdir) {
                let single_status = vec![ia_core::disk_pool::DiskStatus {
                    path: base_destdir.clone(),
                    free_bytes: free,
                    total_bytes: 0,
                    items_count: result.items_total,
                }];
                bd.finish(&result, Some(&single_status));
            } else {
                bd.finish(&result, None);
            }
        } else if quiet == 1 {
            eprintln!(
                "{}  {} items, {} files downloaded ({}), {} skipped, {} failed — {:.1}s",
                style("done").bold(),
                result.items_total,
                result.files_downloaded,
                crate::output::format_bytes(result.bytes_total),
                result.files_skipped,
                result.files_failed + result.items_failed,
                result.elapsed.as_secs_f64(),
            );
        }
    }

    if result.files_failed > 0 || result.items_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Batch download with per-item disk pool assignment.
///
/// Each item is assigned to the disk with the most free space before
/// downloading. If a disk can't fit an item, the item is skipped with an
/// error (not fatal to the batch). Items are downloaded concurrently up to
/// `items_concurrency`.
#[allow(clippy::too_many_arguments)]
async fn download_batch_with_pool(
    client: &IaClient,
    identifiers: Vec<String>,
    make_opts: &dyn Fn(PathBuf) -> DownloadOpts,
    filter: &FileFilter,
    pool: DiskPool,
    semaphore: Arc<Semaphore>,
    batch_display: Option<Arc<crate::output::BatchDisplay>>,
    json_mode: bool,
    items_concurrency: usize,
) -> Result<(BatchDownloadResult, DiskPool)> {
    let start = std::time::Instant::now();
    let items_total = identifiers.len();
    let pool = Arc::new(Mutex::new(pool));
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let filter = filter.clone();

    // Per-item: fetch metadata → compute filtered size → assign disk → download.
    // The pool mutex is held only briefly for assign_item(). Metadata fetch and
    // download run concurrently across items.
    let item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> =
        stream::iter(identifiers)
            .map(|identifier| {
                let client = client.clone();
                let semaphore = Arc::clone(&semaphore);
                let counter = Arc::clone(&counter);
                let batch_display = batch_display.clone();
                let pool = Arc::clone(&pool);
                let filter = filter.clone();

                async move {
                    let idx =
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

                    // Notify display of item start
                    if let Some(ref bd) = batch_display {
                        bd.on_item_start(&identifier, idx, items_total);
                    }

                    info!(item = %identifier, idx, "starting item download");

                    // Fetch metadata to compute filtered size for disk assignment
                    let item = match ia_core::metadata::get(&client, &identifier).await {
                        Ok(item) => item,
                        Err(e) => {
                            warn!(item = %identifier, error = %e, "failed to fetch metadata");
                            if let Some(ref bd) = batch_display {
                                bd.on_item_error(&identifier, &e.to_string());
                            }
                            return Err((identifier, e));
                        }
                    };

                    let files = ia_core::files::list(&item, &filter);
                    let estimated_size = ia_core::files::total_size(&files);

                    // Assign item to a disk (brief lock)
                    let dest = {
                        let mut pool_guard = pool.lock().await;
                        match pool_guard.assign_item(&identifier, estimated_size) {
                            Ok(dest) => dest.to_path_buf(),
                            Err(e) => {
                                warn!(item = %identifier, error = %e, "skipping item: no disk space");
                                if let Some(ref bd) = batch_display {
                                    bd.on_item_error(&identifier, &e.to_string());
                                }
                                return Err((identifier, e));
                            }
                        }
                    };

                    let item_opts = make_opts(dest);

                    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
                        batch_display
                            .clone()
                            .map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                                Arc::new(move |p: DownloadProgress| bd.on_progress(p))
                            });

                    let emit_success = |result: &ItemDownloadResult| {
                        if json_mode {
                            let obj = serde_json::json!({
                                "item": result.identifier,
                                "status": "ok",
                                "files_ok": result.files_downloaded,
                                "files_skipped": result.files_skipped,
                                "files_failed": result.files_failed,
                                "bytes": result.bytes_total,
                                "elapsed_ms": result.elapsed.as_millis() as u64,
                            });
                            println!("{}", obj);
                        }
                        if let Some(ref bd) = batch_display {
                            bd.on_item_complete(result);
                        }
                    };

                    let emit_error = |id: &str, e: &IaError| {
                        if let Some(ref bd) = batch_display {
                            bd.on_item_error(id, &e.to_string());
                        }
                    };

                    match ia_core::download::download_item_with_metadata(
                        &client,
                        &identifier,
                        &item,
                        &item_opts,
                        Arc::clone(&semaphore),
                        progress.clone(),
                    )
                    .await
                    {
                        Ok(result) => {
                            emit_success(&result);
                            Ok(result)
                        }
                        Err(e) if e.is_disk_full() => {
                            // Disk full: clean up partial download, reassign to
                            // another disk, and retry the entire item from scratch.
                            warn!(item = %identifier, "disk full, cleaning up and attempting failover");

                            // Remove partial item directory on the full disk
                            ia_core::download::cleanup_item_dir(
                                &item_opts.destdir,
                                &identifier,
                            )
                            .await;

                            let new_dest = {
                                let mut pool_guard = pool.lock().await;
                                pool_guard.handle_disk_full(&identifier).map(|p| p.to_path_buf())
                            };
                            match new_dest {
                                Ok(dest) => {
                                    let retry_opts = make_opts(dest);
                                    match ia_core::download::download_item_with_metadata(
                                        &client,
                                        &identifier,
                                        &item,
                                        &retry_opts,
                                        semaphore,
                                        progress,
                                    )
                                    .await
                                    {
                                        Ok(result) => {
                                            emit_success(&result);
                                            Ok(result)
                                        }
                                        Err(e2) => {
                                            warn!(item = %identifier, error = %e2, "retry after disk failover also failed");
                                            emit_error(&identifier, &e2);
                                            Err((identifier, e2))
                                        }
                                    }
                                }
                                Err(no_space) => {
                                    warn!(item = %identifier, error = %no_space, "no alternative disk available");
                                    emit_error(&identifier, &no_space);
                                    Err((identifier, no_space))
                                }
                            }
                        }
                        Err(e) => {
                            warn!(item = %identifier, error = %e, "item download failed");
                            emit_error(&identifier, &e);
                            Err((identifier, e))
                        }
                    }
                }
            })
            .buffer_unordered(items_concurrency)
            .collect()
            .await;

    let returned_pool = Arc::try_unwrap(pool)
        .map_err(|_| {
            anyhow::anyhow!("disk pool Arc still shared after batch completed — this is a bug")
        })?
        .into_inner();
    Ok((
        collect_batch_results(items_total, item_results, start.elapsed()),
        returned_pool,
    ))
}

fn write_item_results(jl: &JoblogWriter, identifier: &str, results: &[FileDownloadResult]) {
    for r in results {
        let entry = JoblogEntry::new("download", identifier, &r.file_name);
        let entry = match &r.status {
            DownloadStatus::Complete => entry.ok(r.bytes, r.elapsed.as_millis() as u64),
            DownloadStatus::Skipped(_) => entry.skipped(),
            DownloadStatus::Failed(msg) => entry.error(msg, 0),
            _ => continue,
        };
        jl.write(&entry);
    }
}

/// Build a JSON value for a single file download result.
fn json_file_result(identifier: &str, r: &FileDownloadResult) -> Option<serde_json::Value> {
    match &r.status {
        DownloadStatus::Complete => Some(serde_json::json!({
            "item": identifier,
            "file": r.file_name,
            "status": "ok",
            "bytes": r.bytes,
            "elapsed_ms": r.elapsed.as_millis() as u64,
        })),
        DownloadStatus::Skipped(reason) => Some(serde_json::json!({
            "item": identifier,
            "file": r.file_name,
            "status": "skipped",
            "reason": reason,
        })),
        DownloadStatus::Failed(msg) => Some(serde_json::json!({
            "item": identifier,
            "file": r.file_name,
            "status": "error",
            "error": {
                "code": "download_failed",
                "message": msg,
            },
        })),
        _ => None,
    }
}

/// Print a single file download result as a JSON line to stdout.
fn print_json_file_result(identifier: &str, r: &FileDownloadResult) {
    if let Some(obj) = json_file_result(identifier, r) {
        println!("{}", obj);
    }
}

/// Build a JSON value for an item-level result (batch mode).
fn json_item_result(
    result: &std::result::Result<ItemDownloadResult, (String, IaError)>,
) -> serde_json::Value {
    match result {
        Ok(ir) => serde_json::json!({
            "item": ir.identifier,
            "status": "ok",
            "files_ok": ir.files_downloaded,
            "files_skipped": ir.files_skipped,
            "files_failed": ir.files_failed,
            "bytes": ir.bytes_total,
            "elapsed_ms": ir.elapsed.as_millis() as u64,
        }),
        Err((id, err)) => {
            let je = err.to_json_error();
            serde_json::json!({
                "item": id,
                "status": "error",
                "error": {
                    "code": je.error.code,
                    "message": je.error.message,
                },
            })
        }
    }
}

/// Print an item-level result as a JSON line to stdout (batch mode).
fn print_json_item_result(result: &std::result::Result<ItemDownloadResult, (String, IaError)>) {
    println!("{}", json_item_result(result));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn json_file_result_ok() {
        let r = FileDownloadResult {
            file_name: "photo.jpg".to_string(),
            bytes: 4200000,
            status: DownloadStatus::Complete,
            elapsed: Duration::from_millis(2100),
        };
        let v = json_file_result("nasa", &r).unwrap();
        assert_eq!(v["item"], "nasa");
        assert_eq!(v["file"], "photo.jpg");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["bytes"], 4200000);
        assert_eq!(v["elapsed_ms"], 2100);
    }

    #[test]
    fn json_file_result_skipped() {
        let r = FileDownloadResult {
            file_name: "thumb.jpg".to_string(),
            bytes: 0,
            status: DownloadStatus::Skipped("already_exists".to_string()),
            elapsed: Duration::ZERO,
        };
        let v = json_file_result("nasa", &r).unwrap();
        assert_eq!(v["item"], "nasa");
        assert_eq!(v["file"], "thumb.jpg");
        assert_eq!(v["status"], "skipped");
        assert_eq!(v["reason"], "already_exists");
        assert!(v.get("bytes").is_none());
    }

    #[test]
    fn json_file_result_error() {
        let r = FileDownloadResult {
            file_name: "video.mp4".to_string(),
            bytes: 0,
            status: DownloadStatus::Failed("connection reset".to_string()),
            elapsed: Duration::from_millis(500),
        };
        let v = json_file_result("nasa", &r).unwrap();
        assert_eq!(v["item"], "nasa");
        assert_eq!(v["file"], "video.mp4");
        assert_eq!(v["status"], "error");
        assert_eq!(v["error"]["code"], "download_failed");
        assert_eq!(v["error"]["message"], "connection reset");
    }

    #[test]
    fn json_item_result_ok() {
        let ir = ItemDownloadResult {
            identifier: "nasa".to_string(),
            files_total: 15,
            files_downloaded: 12,
            files_skipped: 3,
            files_failed: 0,
            bytes_total: 42000000,
            elapsed: Duration::from_millis(8500),
            results: vec![],
        };
        let v = json_item_result(&Ok(ir));
        assert_eq!(v["item"], "nasa");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["files_ok"], 12);
        assert_eq!(v["files_skipped"], 3);
        assert_eq!(v["files_failed"], 0);
        assert_eq!(v["bytes"], 42000000);
        assert_eq!(v["elapsed_ms"], 8500);
    }

    #[test]
    fn json_item_result_error() {
        let err = IaError::NotFound("broken".to_string());
        let v = json_item_result(&Err(("broken".to_string(), err)));
        assert_eq!(v["item"], "broken");
        assert_eq!(v["status"], "error");
        assert_eq!(v["error"]["code"], "not_found");
        assert!(v["error"]["message"].as_str().unwrap().contains("broken"));
    }
}
