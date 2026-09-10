use anyhow::{bail, Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;
use futures::{stream, Stream, StreamExt};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::{info, warn};

use ia_core::disk_pool::DiskPool;
use ia_core::download::{
    collect_batch_results, BatchDownloadResult, DownloadOpts, DownloadProgress, DownloadStatus,
    FileDownloadResult, ItemDownloadResult,
};
use ia_core::error::{format_error_chain, IaError, Result as IaResult};
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
         \n\n  <dim># Batch download specific files per item via {identifier} template</dim>\n  <bold>$ ia download --search \"collection:us-supreme-court\" '{identifier}.pdf' '{identifier}_meta.xml'</bold>\
         \n\n  <dim># Batch download from piped identifiers</dim>\n  <bold>$ ia search -q collection:nasa --json | ia download</bold>\
         \n\n  <dim># Download with JSON output (for scripts/agents)</dim>\n  <bold>$ ia download nasa --json</bold>\n"
    ),
)]
pub struct DownloadArgs {
    /// Item identifier to download (single-item mode)
    pub identifier: Option<String>,

    /// File names to download from each item.
    ///
    /// In single-item mode (positional identifier): exact file names from that item.
    /// In batch mode (--search / --itemlist / piped stdin): file names applied to
    /// every item; supports `{identifier}` substitution, e.g. `'{identifier}.pdf'`.
    pub files: Vec<String>,

    /// File containing item identifiers (one per line)
    #[arg(long, conflicts_with = "search")]
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

    /// Increment archive.org's public view counter on each downloaded file.
    ///
    /// By default `ia` sends `cnt=0` with every download request so bulk
    /// downloads do not inflate item view statistics. Pass `--count-views`
    /// to omit the parameter entirely; archive.org only counts a view when
    /// `cnt` is absent — `cnt=1` (or any other value) also suppresses it.
    #[arg(long)]
    count_views: bool,

    /// Download items matching a search query (downloads each result)
    #[arg(short = 's', long, conflicts_with = "itemlist")]
    search: Option<String>,

    /// Extra search parameters for --search (key:value or key=value, repeatable)
    #[arg(long = "search-parameter", value_name = "PARAMETERS")]
    search_parameters: Vec<String>,

    /// Full-screen dashboard mode
    #[arg(long)]
    pub dashboard: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// List files inside a ZIP archive (e.g., "item_jp2.zip")
    #[arg(long, value_name = "ZIPFILE")]
    pub zip_list: Option<String>,

    /// Download a file from inside a ZIP archive (e.g., "item_jp2.zip/item_jp2/item_0001.jp2")
    #[arg(long, value_name = "ZIPFILE/MEMBER")]
    pub zip_member: Option<String>,

    /// Convert format when downloading a zip member (e.g., "jpg" to convert JP2 → JPEG)
    #[arg(long, value_name = "EXT", requires = "zip_member")]
    pub zip_convert: Option<String>,
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

/// Parse `--search-parameter` values into a [`SearchOpts`] with extra params.
///
/// Delegates to the shared parser so all `--search` commands behave identically.
fn search_opts_from_params(params: &[String]) -> Result<SearchOpts> {
    Ok(SearchOpts {
        params: crate::commands::search::parse_extra_params(params)?,
        ..SearchOpts::default()
    })
}

/// Collect all identifiers from args, --itemlist file, --search, and stdin.
async fn collect_identifiers(args: &DownloadArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids: Vec<String> = args.identifier.iter().cloned().collect();

    if let Some(path) = &args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            if let Some(id) = parse_identifier_line(line) {
                ids.push(id);
            }
        }
    }

    // --search: collect identifiers from search results (dashboard path only —
    // the streaming path in run() handles --search before reaching here).
    if let Some(ref query) = args.search {
        let opts = search_opts_from_params(&args.search_parameters)?;
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

    if !args.files.is_empty() && ids.is_empty() {
        bail!(
            "file names require an identifier source \
             (positional, --search, --itemlist, or piped stdin)"
        );
    }

    Ok(ids)
}

pub async fn run(
    client: &IaClient,
    mut args: DownloadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    if args.json && args.dashboard {
        bail!("--json and --dashboard are mutually exclusive");
    }

    // In batch mode (--search / --itemlist), positional args are file names per
    // item, not identifiers. Clap parses positionals as identifier+files; merge
    // the first positional back into `files` so downstream sees the right shape.
    if (args.search.is_some() || args.itemlist.is_some()) && args.identifier.is_some() {
        let ident = args.identifier.take().unwrap();
        args.files.insert(0, ident);
    }

    // Handle zip-specific operations early (these don't go through the normal download path)
    if args.zip_list.is_some() || args.zip_member.is_some() {
        return run_zip(client, &args, quiet).await;
    }

    // ─── Two-press Ctrl-C handler ───────────────────────────────────────
    // First press: print a shutdown message, wake the main select so we
    //   tear down cleanly (summary + http diagnostics), then exit 130.
    // Second press: exit 130 immediately — even if the first-press cleanup
    //   is stuck on a mutex or blocking I/O.
    // Installed here so it covers joblog read, metadata fetch, and every
    // download code path below. TUI mode runs crossterm in raw mode which
    // intercepts Ctrl-C as a key event, so this watcher only fires before
    // the TUI takes over (or after it exits) — no conflict.
    let cancel = Arc::new(Notify::new());
    {
        let cancel = Arc::clone(&cancel);
        let presses = Arc::new(AtomicU8::new(0));
        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                let prev = presses.fetch_add(1, Ordering::SeqCst);
                if prev == 0 {
                    eprintln!(
                        "\n{} shutting down — press Ctrl-C again to force quit",
                        style("interrupt:").bold().yellow(),
                    );
                    cancel.notify_waiters();
                } else {
                    std::process::exit(130);
                }
            }
        });
    }

    // ─── Shared setup ───────────────────────────────────────────────────
    // Moved before collect_identifiers() so the streaming search path can
    // use it without blocking on full identifier collection.

    // Open joblog writer if path provided
    let joblog = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;

    // Build item-level resume skip set from joblog — items whose last run
    // recorded no failures are treated as fully downloaded and filtered out
    // before any metadata fetch happens. Per-file `.part` resume still
    // handles partial files inside items we do end up processing.
    let resume_skip: std::collections::HashSet<String> = if no_resume {
        std::collections::HashSet::new()
    } else if let Some(ref path) = joblog_path {
        if path.exists() {
            // Joblog read is blocking and can take seconds on a log with
            // hundreds of thousands of entries. Show a spinner so the
            // user knows the tool is alive.
            let spinner = if quiet < 2 {
                let sp = indicatif::ProgressBar::new_spinner();
                sp.set_style(
                    indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
                        .unwrap()
                        .tick_strings(&[
                            "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}",
                            "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}", " ",
                        ]),
                );
                sp.enable_steady_tick(std::time::Duration::from_millis(100));
                sp.set_message(format!("resuming — reading {}", path.display()));
                Some(sp)
            } else {
                None
            };

            let entries = ia_core::joblog::read(path)
                .with_context(|| format!("failed to read joblog for resume: {}", path.display()))?;
            let set = ia_core::joblog::items_fully_downloaded(&entries, "download");

            if let Some(sp) = spinner {
                sp.finish_and_clear();
            }

            if !set.is_empty() && quiet < 2 {
                eprintln!(
                    "{} {} item{} already completed, skipping on this run",
                    style("resume:").bold().cyan(),
                    set.len(),
                    if set.len() == 1 { "" } else { "s" },
                );
            }
            set
        } else {
            std::collections::HashSet::new()
        }
    } else {
        std::collections::HashSet::new()
    };

    // Set up disk pool if multiple destdirs
    let destdirs = if args.destdir.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.destdir.clone()
    };

    // Validate destdir paths early so typos are caught before any downloads.
    let multi_disk = destdirs.len() > 1;
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
        } else if multi_disk {
            // Multi-disk: require directories to exist (external drives).
            // If /Volumes/MyDrive doesn't exist, the drive is unplugged —
            // creating it as a regular dir would silently download to boot disk.
            bail!(
                "--destdir does not exist: {}\n\
                 When using multiple --destdir paths (disk pool), all directories\n\
                 must already exist. Is the drive plugged in?",
                dir.display()
            );
        } else {
            // Single destdir: auto-create is fine (backwards compat)
            std::fs::create_dir_all(dir).context(format!(
                "--destdir does not exist and cannot be created: {}",
                dir.display()
            ))?;
        }
    }

    let mut disk_pool = if destdirs.len() > 1 {
        let mut pool = DiskPool::new(&destdirs).context("failed to initialize disk pool")?;
        pool.scan_existing();
        Some(pool)
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
    if let Err(msg) = ia_core::files::validate_name_placeholders(&args.files) {
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
        count_views: args.count_views,
    };

    let opts = make_opts(base_destdir.clone());

    let semaphore = Arc::new(Semaphore::new(jobs));

    // ─── Streaming search path ──────────────────────────────────────────
    // When --search is used without --dashboard, pipe search results
    // directly into the download pipeline instead of collecting all
    // identifiers first. This avoids a 1-2 min delay for large collections.
    if let Some(ref query) = args.search {
        if !args.dashboard {
            let json_mode = args.json;
            // Item-level concurrency caps how many items are in "resolving
            // metadata" or "active download" simultaneously. Files across
            // those items share the single file-level semaphore (capacity =
            // jobs), so total in-flight files is capped at --jobs regardless.
            let items_concurrency = jobs;

            // Quick estimated total for progress display (~200ms).
            let estimated_total = ia_core::search::num_found(client, query, &[])
                .await
                .unwrap_or(0) as usize;

            // If resuming, the header total represents "remaining this run"
            // — already-completed items are filtered before they reach the
            // pipeline and so never increment items_done.
            let header_total = estimated_total.saturating_sub(resume_skip.len());

            let batch_display = if !json_mode && quiet == 0 {
                let bd = Arc::new(crate::output::BatchDisplay::new(header_total));
                bd.set_joblog_path(joblog_path.clone());
                Some(bd)
            } else {
                None
            };

            let search_opts = search_opts_from_params(&args.search_parameters)?;
            let resume_skip = resume_skip.clone();
            let id_stream: Pin<Box<dyn Stream<Item = IaResult<String>> + Send + '_>> = Box::pin(
                ia_core::search::scrape(client, query, &search_opts)
                    .map(|r| r.map(|item| item.identifier))
                    .filter(move |r| {
                        let keep = match r {
                            Ok(id) => !resume_skip.contains(id),
                            Err(_) => true,
                        };
                        async move { keep }
                    }),
            );

            let has_disk_pool = disk_pool.is_some();
            let pool_opt = disk_pool.take();
            let display_for_cancel = batch_display.clone();
            let cancel_branch = Arc::clone(&cancel);

            // Race the batch download against the first-press cancel
            // notify so SIGINT produces the end-of-run summary from live
            // counters. The detached per-file tasks inside are only
            // terminated by `process::exit(130)` below, not by dropping
            // this future — dropping just abandons them on the runtime.
            let result = tokio::select! {
                biased;
                _ = cancel_branch.notified() => {
                    if let Some(ref bd) = display_for_cancel {
                        bd.finish_cancelled(None);
                    }
                    crate::output::print_http_diagnostics(client.retry_stats());
                    std::process::exit(130);
                }
                r = async {
                    match pool_opt {
                        Some(pool) => {
                            let (batch_result, returned_pool) = download_batch_with_pool(
                                client, id_stream, &make_opts, &filter, pool,
                                semaphore, batch_display.clone(), json_mode, 0,
                                items_concurrency, joblog.clone(),
                            ).await?;
                            disk_pool = Some(returned_pool);
                            Ok::<_, anyhow::Error>(batch_result)
                        }
                        None => {
                            let batch_result = download_batch_items(
                                client, id_stream, &opts, semaphore,
                                batch_display.clone(), json_mode, 0,
                                items_concurrency, joblog.clone(),
                            ).await;
                            Ok(batch_result)
                        }
                    }
                } => r?,
            };

            return finish_batch(
                result,
                json_mode,
                quiet,
                has_disk_pool,
                &batch_display,
                &disk_pool,
                &base_destdir,
                client.retry_stats(),
            );
        }
    }

    // ─── Collect-first path ─────────────────────────────────────────────
    // Used for --itemlist, stdin, positional identifiers, and
    // --dashboard --search (dashboard needs all identifiers upfront).
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

    if identifiers.is_empty() {
        bail!("no identifiers provided. Pass identifiers as arguments, use --itemlist, or pipe to stdin.");
    }

    // Filter out items already fully downloaded per joblog.
    if !resume_skip.is_empty() {
        identifiers.retain(|id| !resume_skip.contains(id));
        if identifiers.is_empty() {
            if quiet < 2 {
                eprintln!(
                    "{} all items already completed per joblog; nothing to do",
                    style("resume:").bold().cyan(),
                );
            }
            return Ok(());
        }
    }

    // Dashboard mode
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_tui(client, &identifiers, &opts, Arc::clone(&semaphore), jobs)
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

        let cancel_branch = Arc::clone(&cancel);
        let display_for_cancel = display.clone();
        let result = tokio::select! {
            biased;
            _ = cancel_branch.notified() => {
                if let Some(d) = display_for_cancel {
                    d.finish_cancelled();
                }
                eprintln!("{} {}", style("interrupted:").bold().yellow(), identifier);
                crate::output::print_http_diagnostics(client.retry_stats());
                std::process::exit(130);
            }
            r = ia_core::download::download_item(
                client,
                identifier,
                &item_opts,
                Arc::clone(&semaphore),
                progress,
            ) => r.context(format!("failed to download {}", identifier))?,
        };

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

        if !args.json && quiet < 2 {
            crate::output::print_http_diagnostics(client.retry_stats());
        }

        if result.files_failed > 0 {
            std::process::exit(1);
        }

        return Ok(());
    }

    // Batch mode — wrap collected identifiers as a stream and use the
    // same functions as the streaming search path.
    let json_mode = args.json;
    let items_concurrency = jobs;
    let items_total = identifiers.len();
    let batch_display = if !json_mode && quiet == 0 {
        let bd = Arc::new(crate::output::BatchDisplay::new(items_total));
        bd.set_joblog_path(joblog_path.clone());
        Some(bd)
    } else {
        None
    };

    let id_stream: Pin<Box<dyn Stream<Item = IaResult<String>> + Send>> =
        Box::pin(stream::iter(identifiers).map(Ok));

    let has_disk_pool = disk_pool.is_some();
    let pool_opt = disk_pool.take();
    let display_for_cancel = batch_display.clone();
    let cancel_branch = Arc::clone(&cancel);

    let result = tokio::select! {
        biased;
        _ = cancel_branch.notified() => {
            if let Some(ref bd) = display_for_cancel {
                bd.finish_cancelled(None);
            }
            crate::output::print_http_diagnostics(client.retry_stats());
            std::process::exit(130);
        }
        r = async {
            match pool_opt {
                Some(pool) => {
                    let (batch_result, returned_pool) = download_batch_with_pool(
                        client, id_stream, &make_opts, &filter, pool,
                        semaphore, batch_display.clone(), json_mode,
                        items_total, items_concurrency, joblog.clone(),
                    ).await?;
                    disk_pool = Some(returned_pool);
                    Ok::<_, anyhow::Error>(batch_result)
                }
                None => {
                    let batch_result = download_batch_items(
                        client, id_stream, &opts, semaphore,
                        batch_display.clone(), json_mode, items_total,
                        items_concurrency, joblog.clone(),
                    ).await;
                    Ok(batch_result)
                }
            }
        } => r?,
    };

    finish_batch(
        result,
        json_mode,
        quiet,
        has_disk_pool,
        &batch_display,
        &disk_pool,
        &base_destdir,
        client.retry_stats(),
    )
}

/// Batch download with per-item disk pool assignment.
///
/// Each item is assigned to the disk with the most free space before
/// downloading. If a disk can't fit an item, the item is skipped with an
/// error (not fatal to the batch). Items are downloaded concurrently up to
/// `items_concurrency`.
///
/// Accepts a stream of identifiers — works for both collected Vecs (wrapped
/// via `stream::iter(ids).map(Ok)`) and live search streams.
/// `items_total` is used for progress display: pass `identifiers.len()` when
/// known, or `0` for streaming (unknown total).
#[allow(clippy::too_many_arguments)]
async fn download_batch_with_pool(
    client: &IaClient,
    id_stream: Pin<Box<dyn Stream<Item = IaResult<String>> + Send + '_>>,
    make_opts: &dyn Fn(PathBuf) -> DownloadOpts,
    filter: &FileFilter,
    pool: DiskPool,
    semaphore: Arc<Semaphore>,
    batch_display: Option<Arc<crate::output::BatchDisplay>>,
    json_mode: bool,
    items_total: usize,
    items_concurrency: usize,
    joblog: Option<JoblogWriter>,
) -> Result<(BatchDownloadResult, DiskPool)> {
    let start = std::time::Instant::now();
    let pool = Arc::new(Mutex::new(pool));
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let filter = filter.clone();

    // Per-item: fetch metadata → compute filtered size → assign disk → download.
    // The pool mutex is held only briefly for assign_item(). Metadata fetch and
    // download run concurrently across items.
    let item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> = id_stream
        .map(|id_result| {
            let client = client.clone();
            let semaphore = Arc::clone(&semaphore);
            let counter = Arc::clone(&counter);
            let batch_display = batch_display.clone();
            let pool = Arc::clone(&pool);
            let filter = filter.clone();
            let joblog = joblog.clone();

            async move {
                let identifier = match id_result {
                    Ok(id) => id,
                    // Stream error (e.g. search pagination failure) — record
                    // as a failed item. Unreachable for Vec-backed streams.
                    Err(e) => {
                        let msg = format_error_chain(&e);
                        if let Some(ref jl) = joblog {
                            jl.write(
                                &JoblogEntry::new("download", "<search>", "").error(&msg, 0),
                            );
                        }
                        return Err(("<search>".to_string(), IaError::Config(msg)));
                    }
                };

                let idx = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

                if let Some(ref bd) = batch_display {
                    bd.on_item_start(&identifier, idx, items_total);
                }
                info!(item = %identifier, idx, "starting item download");

                // Emit Resolving so TUI/console show activity during the
                // 5-15 s metadata round-trip instead of a silent "starting..."
                // (multi-disk path fetches metadata outside download_item,
                // so the Resolving emit there doesn't cover this branch).
                if let Some(ref bd) = batch_display {
                    bd.on_progress(DownloadProgress {
                        identifier: identifier.clone(),
                        file_name: String::new(),
                        bytes_downloaded: 0,
                        total_bytes: None,
                        status: DownloadStatus::Resolving,
                    });
                }

                // Fetch metadata to compute filtered size for disk assignment
                let item = match ia_core::metadata::get(&client, &identifier).await {
                    Ok(item) => item,
                    Err(e) => {
                        let msg = format_error_chain(&e);
                        warn!(item = %identifier, error = %msg, "failed to fetch metadata");
                        if let Some(ref jl) = joblog {
                            jl.write(
                                &JoblogEntry::new("download", &identifier, "").error(&msg, 0),
                            );
                        }
                        if let Some(ref bd) = batch_display {
                            bd.on_item_error(&identifier, &msg);
                        }
                        return Err((identifier, e));
                    }
                };

                let mut item_filter = filter.clone();
                item_filter.names = ia_core::files::substitute_names(&filter.names, &identifier);
                let files = ia_core::files::list(&item, &item_filter);
                let estimated_size = ia_core::files::total_size(&files);

                // Assign item to a disk (brief lock)
                let dest = {
                    let mut pool_guard = pool.lock().await;
                    match pool_guard.assign_item(&identifier, estimated_size) {
                        Ok(dest) => dest.to_path_buf(),
                        Err(e) => {
                            let msg = format_error_chain(&e);
                            warn!(item = %identifier, error = %msg, "skipping item: no disk space");
                            if let Some(ref jl) = joblog {
                                jl.write(
                                    &JoblogEntry::new("download", &identifier, "").error(&msg, 0),
                                );
                            }
                            if let Some(ref bd) = batch_display {
                                bd.on_item_error(&identifier, &msg);
                            }
                            return Err((identifier, e));
                        }
                    }
                };

                let item_opts = make_opts(dest);

                let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> = batch_display
                    .clone()
                    .map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                        Arc::new(move |p: DownloadProgress| bd.on_progress(p))
                    });

                let emit_success = |result: &ItemDownloadResult| {
                    if let Some(ref jl) = joblog {
                        write_item_results(jl, &result.identifier, &result.results);
                    }
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
                        println!("{obj}");
                    }
                    if let Some(ref bd) = batch_display {
                        bd.on_item_complete(result);
                    }
                };

                let emit_error = |id: &str, e: &IaError| {
                    let msg = format_error_chain(e);
                    if let Some(ref jl) = joblog {
                        jl.write(&JoblogEntry::new("download", id, "").error(&msg, 0));
                    }
                    if let Some(ref bd) = batch_display {
                        bd.on_item_error(id, &msg);
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
                        warn!(item = %identifier, "disk full, cleaning up and attempting failover");

                        ia_core::download::cleanup_item_dir(&item_opts.destdir, &identifier).await;

                        let new_dest = {
                            let mut pool_guard = pool.lock().await;
                            pool_guard
                                .handle_disk_full(&identifier)
                                .map(|p| p.to_path_buf())
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

    let actual_total = item_results.len();
    let returned_pool = Arc::try_unwrap(pool)
        .map_err(|_| {
            anyhow::anyhow!("disk pool Arc still shared after batch completed — this is a bug")
        })?
        .into_inner();
    Ok((
        collect_batch_results(actual_total, item_results, start.elapsed()),
        returned_pool,
    ))
}

/// Shared batch result handling: JSON errors, summary, exit code.
///
/// Joblog writes happen per-item inside `download_batch_items` and
/// `download_batch_with_pool` so an interrupted run keeps whatever
/// progress made it to disk.
#[allow(clippy::too_many_arguments)]
fn finish_batch(
    result: BatchDownloadResult,
    json_mode: bool,
    quiet: u8,
    has_disk_pool: bool,
    batch_display: &Option<Arc<crate::output::BatchDisplay>>,
    disk_pool: &Option<DiskPool>,
    base_destdir: &Path,
    retry_stats: &ia_core::retry::RetryStats,
) -> Result<()> {
    // Print JSON for failed items (on_item_complete only fires for Ok results)
    if json_mode {
        for item_result in &result.item_results {
            if item_result.is_err() {
                print_json_item_result(item_result);
            }
        }
    }

    // NOTE: joblog writes happen per-item inside download_batch_items and
    // download_batch_with_pool. Writing here too would duplicate every entry.

    // Print summary
    if !json_mode && quiet < 2 {
        let disk_statuses = disk_pool.as_ref().map(|p| p.status());
        if let Some(ref bd) = batch_display {
            if has_disk_pool {
                bd.finish(&result, disk_statuses.as_deref());
            } else if let Some(free) = crate::output::disk_space_free(base_destdir) {
                let single_status = vec![ia_core::disk_pool::DiskStatus {
                    path: base_destdir.to_path_buf(),
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

        crate::output::print_http_diagnostics(retry_stats);
    }

    if result.files_failed > 0 || result.items_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Batch download (single destdir).
///
/// Downloads items from a stream of identifiers with controlled concurrency.
/// Works for both collected Vecs (wrapped via `stream::iter(ids).map(Ok)`)
/// and live search streams.
/// `items_total` is used for progress display: pass `identifiers.len()` when
/// known, or `0` for streaming (unknown total).
#[allow(clippy::too_many_arguments)]
async fn download_batch_items(
    client: &IaClient,
    id_stream: Pin<Box<dyn Stream<Item = IaResult<String>> + Send + '_>>,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    batch_display: Option<Arc<crate::output::BatchDisplay>>,
    json_mode: bool,
    items_total: usize,
    items_concurrency: usize,
    joblog: Option<JoblogWriter>,
) -> BatchDownloadResult {
    let start = std::time::Instant::now();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> = id_stream
        .map(|id_result| {
            let client = client.clone();
            let opts = opts.clone();
            let semaphore = Arc::clone(&semaphore);
            let counter = Arc::clone(&counter);
            let batch_display = batch_display.clone();
            let joblog = joblog.clone();

            async move {
                let identifier = match id_result {
                    Ok(id) => id,
                    // Stream error (e.g. search pagination failure) — record
                    // as a failed item. Unreachable for Vec-backed streams.
                    Err(e) => {
                        let msg = e.to_string();
                        if let Some(ref jl) = joblog {
                            jl.write(&JoblogEntry::new("download", "<search>", "").error(&msg, 0));
                        }
                        return Err(("<search>".to_string(), IaError::Config(msg)));
                    }
                };

                let idx = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

                if let Some(ref bd) = batch_display {
                    bd.on_item_start(&identifier, idx, items_total);
                }
                info!(item = %identifier, idx, "starting item download");

                let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> = batch_display
                    .clone()
                    .map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
                        Arc::new(move |p: DownloadProgress| bd.on_progress(p))
                    });

                match ia_core::download::download_item(
                    &client,
                    &identifier,
                    &opts,
                    semaphore,
                    progress,
                )
                .await
                {
                    Ok(result) => {
                        if let Some(ref jl) = joblog {
                            write_item_results(jl, &result.identifier, &result.results);
                        }
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
                            println!("{obj}");
                        }
                        if let Some(ref bd) = batch_display {
                            bd.on_item_complete(&result);
                        }
                        Ok(result)
                    }
                    Err(e) => {
                        let msg = format_error_chain(&e);
                        warn!(identifier = %identifier, error = %msg, "item download failed");
                        if let Some(ref jl) = joblog {
                            jl.write(&JoblogEntry::new("download", &identifier, "").error(&msg, 0));
                        }
                        if let Some(ref bd) = batch_display {
                            bd.on_item_error(&identifier, &msg);
                        }
                        Err((identifier, e))
                    }
                }
            }
        })
        .buffer_unordered(items_concurrency)
        .collect()
        .await;

    let actual_total = item_results.len();
    collect_batch_results(actual_total, item_results, start.elapsed())
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

// ─── Zip operations ──────────────────────────────────────────────────────────

/// Handle zip-specific operations: --zip-list and --zip-member.
async fn run_zip(client: &IaClient, args: &DownloadArgs, quiet: u8) -> Result<()> {
    let identifier = args
        .identifier
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("identifier required for zip operations"))?;

    if let Some(ref zip_filename) = args.zip_list {
        // List contents of a zip file
        let all_entries =
            ia_core::download::zip::list_zip_contents(client, identifier, zip_filename)
                .await
                .context(format!(
                    "failed to list zip {zip_filename} for {identifier}"
                ))?;

        // Apply glob/exclude filters if present
        let entries: Vec<_> = {
            use globset::Glob;
            let glob_matchers = args.glob.as_ref().map(|g| {
                g.split('|')
                    .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
                    .collect::<Vec<_>>()
            });
            let exclude_matchers = args.exclude.as_ref().map(|g| {
                g.split('|')
                    .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
                    .collect::<Vec<_>>()
            });

            all_entries
                .into_iter()
                .filter(|e| {
                    if let Some(ref matchers) = glob_matchers {
                        if !matchers.iter().any(|m| m.is_match(&e.path)) {
                            return false;
                        }
                    }
                    if let Some(ref matchers) = exclude_matchers {
                        if matchers.iter().any(|m| m.is_match(&e.path)) {
                            return false;
                        }
                    }
                    true
                })
                .collect()
        };

        if args.json {
            for entry in &entries {
                let obj = serde_json::json!({
                    "path": entry.path,
                    "size": entry.size,
                    "modified": entry.modified,
                });
                println!("{}", serde_json::to_string(&obj)?);
            }
        } else {
            if quiet == 0 {
                eprintln!(
                    "{} {} entries in {}/{}",
                    style("●").cyan(),
                    entries.len(),
                    identifier,
                    zip_filename,
                );
            }
            for entry in &entries {
                if let Some(size) = entry.size {
                    println!("{:>10}  {}", size, entry.path);
                } else {
                    println!("         -  {}", entry.path);
                }
            }
        }
        return Ok(());
    }

    if let Some(ref zip_member_spec) = args.zip_member {
        let (zip_filename, member_path) = parse_zip_member_spec(zip_member_spec)?;

        let out_filename = if let Some(ref ext) = args.zip_convert {
            let base = member_path.rsplit('/').next().unwrap_or(&member_path);
            if let Some(dot) = base.rfind('.') {
                format!("{}.{ext}", &base[..dot])
            } else {
                format!("{base}.{ext}")
            }
        } else {
            member_path
                .rsplit('/')
                .next()
                .unwrap_or(&member_path)
                .to_string()
        };

        let dest = if args.destdir.is_empty() {
            PathBuf::from(".")
        } else {
            args.destdir[0].clone()
        };
        let out_path = dest.join(&out_filename);

        if args.dry_run {
            if quiet == 0 {
                eprintln!(
                    "{} Would download {}/{}/{} → {}",
                    style("●").cyan(),
                    identifier,
                    zip_filename,
                    member_path,
                    out_path.display(),
                );
            }
            if args.json {
                let obj = serde_json::json!({
                    "identifier": identifier,
                    "zip": zip_filename,
                    "member": member_path,
                    "output": out_path.display().to_string(),
                    "dry_run": true,
                });
                println!("{}", serde_json::to_string(&obj)?);
            }
            return Ok(());
        }

        let data = if let Some(ref ext) = args.zip_convert {
            ia_core::download::zip::download_zip_member_converted(
                client,
                identifier,
                &zip_filename,
                &member_path,
                ext,
            )
            .await
            .context(format!(
                "failed to download {member_path} as {ext} from {zip_filename}"
            ))?
        } else {
            ia_core::download::zip::download_zip_member(
                client,
                identifier,
                &zip_filename,
                &member_path,
            )
            .await
            .context(format!(
                "failed to download {member_path} from {zip_filename}"
            ))?
        };

        tokio::fs::write(&out_path, &data)
            .await
            .context(format!("failed to write {}", out_path.display()))?;

        if quiet == 0 && !args.json {
            eprintln!(
                "{} {} ({} bytes)",
                style("✓").green(),
                out_path.display(),
                data.len(),
            );
        }
        if args.json {
            let obj = serde_json::json!({
                "identifier": identifier,
                "zip": zip_filename,
                "member": member_path,
                "output": out_path.display().to_string(),
                "size": data.len(),
            });
            println!("{}", serde_json::to_string(&obj)?);
        }
        return Ok(());
    }

    Ok(())
}

/// Parse a "zipfile/member/path" spec into (zip_filename, member_path).
///
/// The zip filename is the first path segment that ends with `.zip`.
/// Everything after is the member path.
fn parse_zip_member_spec(spec: &str) -> Result<(String, String)> {
    if let Some(zip_end) = spec.find(".zip/") {
        let zip_filename = spec[..zip_end + 4].to_string();
        let member_path = spec[zip_end + 5..].to_string();
        if member_path.is_empty() {
            bail!("no member path after zip filename in: {spec:?}");
        }
        Ok((zip_filename, member_path))
    } else {
        bail!(
            "invalid zip member spec: {spec:?}\n\
             Expected format: ZIPFILE.zip/member/path\n\
             Example: item_jp2.zip/item_jp2/item_0001.jp2"
        );
    }
}
