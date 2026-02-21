use anyhow::{bail, Context, Result};
use clap::Args;
use console::style;
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

use ia_core::disk_pool::DiskPool;
use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus, FileDownloadResult};
use ia_core::files::FileFilter;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::search::SearchOpts;
use ia_core::types::FileSource;
use ia_core::IaClient;

use crate::output::DownloadDisplay;

#[derive(Args)]
pub struct DownloadArgs {
    /// Item identifier(s) to download
    pub identifiers: Vec<String>,

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

    /// Filter by source type
    #[arg(long, value_parser = parse_source)]
    source: Option<FileSource>,

    /// Exclude by source type
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

    /// Download items matching search query
    #[arg(short = 's', long)]
    search: Option<String>,

    /// Full-screen dashboard mode
    #[arg(long)]
    pub dashboard: bool,
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!("unknown source: {s} (expected: original, derivative, metadata)")),
    }
}

/// Collect all identifiers from args, --itemlist file, --search, and stdin.
async fn collect_identifiers(args: &DownloadArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = args.identifiers.clone();

    if let Some(path) = &args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    // --search: collect identifiers from search results
    if let Some(ref query) = args.search {
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // Also read from stdin if no identifiers and no itemlist and no search
    if ids.is_empty() && args.itemlist.is_none() && args.search.is_none() {
        // Check if stdin is a pipe
        if atty::isnt(atty::Stream::Stdin) {
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
    let mut identifiers = collect_identifiers(&args, client).await?;

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
    let mut disk_pool = if destdirs.len() > 1 {
        Some(DiskPool::new(&destdirs).context("failed to initialize disk pool")?)
    } else {
        None
    };

    let base_destdir = destdirs.first().cloned().unwrap_or_else(|| PathBuf::from("."));

    let make_opts = |destdir: PathBuf| DownloadOpts {
        destdir,
        no_directories: args.no_directories,
        checksum: args.checksum,
        retries: args.retries,
        no_timestamps: args.no_timestamps,
        dry_run: args.dry_run,
        filter: FileFilter {
            glob: args.glob.clone(),
            exclude: args.exclude.clone(),
            formats: args.format.clone(),
            source: args.source.clone(),
            exclude_source: args.exclude_source.clone(),
            names: vec![],
        },
    };

    let opts = make_opts(base_destdir.clone());
    let semaphore = Arc::new(Semaphore::new(jobs));

    // Dashboard mode
    #[cfg(feature = "tui")]
    if args.dashboard {
        return crate::tui::run_tui(client, &identifiers, &opts, Arc::clone(&semaphore)).await;
    }

    #[cfg(not(feature = "tui"))]
    if args.dashboard {
        bail!("Dashboard mode requires the 'tui' feature. Rebuild with: cargo build --features tui");
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

        let multi = indicatif::MultiProgress::new();
        let display = if quiet == 0 {
            Some(Arc::new(DownloadDisplay::new(identifier, &multi)))
        } else {
            None
        };

        let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
            display.clone().map(|d| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
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

        if quiet == 1 {
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
    let batch_display = if quiet == 0 {
        Some(Arc::new(crate::output::BatchDisplay::new(identifiers.len(), jobs)))
    } else {
        None
    };

    let on_item_start: Option<Arc<dyn Fn(&str, usize, usize) + Send + Sync>> =
        batch_display.clone().map(|bd| -> Arc<dyn Fn(&str, usize, usize) + Send + Sync> {
            Arc::new(move |id, current, total| bd.on_item_start(id, current, total))
        });

    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
        batch_display.clone().map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
            Arc::new(move |p: DownloadProgress| bd.on_progress(p))
        });

    let on_item_complete: Option<Arc<dyn Fn(&ia_core::download::ItemDownloadResult) + Send + Sync>> =
        batch_display.clone().map(|bd| -> Arc<dyn Fn(&ia_core::download::ItemDownloadResult) + Send + Sync> {
            Arc::new(move |result| bd.on_item_complete(result))
        });

    let result = ia_core::download::download_batch(
        client,
        identifiers,
        &opts,
        semaphore,
        progress,
        on_item_start,
        on_item_complete,
    )
    .await;

    // Write batch results to joblog
    if let Some(ref jl) = joblog {
        for item_result in &result.item_results {
            match item_result {
                Ok(ir) => write_item_results(jl, &ir.identifier, &ir.results),
                Err((id, err)) => {
                    jl.write(
                        &JoblogEntry::new("download", id, "").error(&err.to_string(), 0),
                    );
                }
            }
        }
    }

    // Print summary
    if quiet < 2 {
        let disk_statuses = disk_pool.as_ref().map(|p| p.status());
        if let Some(ref bd) = batch_display {
            if disk_pool.is_some() {
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

