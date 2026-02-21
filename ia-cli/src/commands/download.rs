use anyhow::{bail, Context, Result};
use clap::Args;
use console::style;
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

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
    #[arg(short = 'i', long)]
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

    /// Concurrent downloads per item
    #[arg(short = 'j', long, default_value = "4")]
    jobs: usize,

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

    /// Concurrent items in batch mode
    #[arg(long, default_value = "2")]
    items: usize,
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

    let opts = DownloadOpts {
        jobs: args.jobs,
        destdir: args.destdir.first().cloned().unwrap_or_else(|| PathBuf::from(".")),
        no_directories: args.no_directories,
        checksum: args.checksum,
        retries: args.retries,
        no_timestamps: args.no_timestamps,
        dry_run: args.dry_run,
        filter: FileFilter {
            glob: args.glob,
            exclude: args.exclude,
            formats: args.format,
            source: args.source,
            exclude_source: args.exclude_source,
            names: vec![],
        },
    };

    // Single item — use the original simple path
    if identifiers.len() == 1 {
        let identifier = &identifiers[0];
        let display = if quiet == 0 {
            Some(Arc::new(DownloadDisplay::new(identifier)))
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
            &opts,
            progress,
        )
        .await
        .context(format!("failed to download {}", identifier))?;

        if let Some(d) = display {
            d.finish(&result);
        }

        if let Some(ref jl) = joblog {
            write_item_results(jl, identifier, &result.results);
        }

        if quiet == 1 {
            eprintln!(
                "{}  {} files ({}) in {:.1}s",
                identifier,
                result.files_downloaded,
                format_bytes(result.bytes_total),
                result.elapsed.as_secs_f64(),
            );
        }

        if result.files_failed > 0 {
            std::process::exit(1);
        }

        return Ok(());
    }

    // Batch mode
    if quiet == 0 {
        eprintln!(
            "{}  Downloading {} items...\n",
            style("batch").bold(),
            identifiers.len(),
        );
    }

    let on_item_start: Option<&(dyn Fn(&str, usize, usize) + Send + Sync)> = if quiet < 2 {
        Some(&|id: &str, current: usize, total: usize| {
            eprintln!(
                "{} [{}/{}] {}",
                style("→").cyan(),
                current,
                total,
                style(id).bold(),
            );
        })
    } else {
        None
    };

    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> = if quiet == 0 {
        // In batch mode, just show per-file status lines (no progress bars to avoid clutter)
        Some(Arc::new(move |p: DownloadProgress| {
            match &p.status {
                ia_core::download::DownloadStatus::Complete => {
                    eprintln!("  {} {}", style("✓").green(), p.file_name);
                }
                ia_core::download::DownloadStatus::Skipped(reason) => {
                    eprintln!("  {} {} ({})", style("–").yellow(), style(&p.file_name).dim(), reason);
                }
                ia_core::download::DownloadStatus::Failed(err) => {
                    eprintln!("  {} {} {}", style("✗").red(), p.file_name, style(err).red());
                }
                _ => {}
            }
        }))
    } else {
        None
    };

    let result = ia_core::download::download_batch_concurrent(
        client,
        identifiers,
        &opts,
        args.items,
        progress,
        on_item_start,
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
        eprintln!(
            "\n{}  {} items, {} files downloaded ({}), {} skipped, {} failed — {:.1}s",
            style("done").bold(),
            result.items_total,
            result.files_downloaded,
            format_bytes(result.bytes_total),
            result.files_skipped,
            result.files_failed + result.items_failed,
            result.elapsed.as_secs_f64(),
        );
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

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
