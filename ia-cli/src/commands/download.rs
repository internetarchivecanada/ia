use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;

use ia_core::download::{DownloadOpts, DownloadProgress};
use ia_core::files::FileFilter;
use ia_core::types::FileSource;
use ia_core::IaClient;

use crate::output::DownloadDisplay;

#[derive(Args)]
pub struct DownloadArgs {
    /// Item identifier to download
    pub identifier: String,

    /// Specific files to download (optional)
    pub files: Vec<String>,

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
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!("unknown source: {s} (expected: original, derivative, metadata)")),
    }
}

pub async fn run(client: &IaClient, args: DownloadArgs, quiet: u8) -> Result<()> {
    let identifier = args.identifier.clone();
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
            names: args.files,
        },
    };

    let display = if quiet == 0 {
        Some(Arc::new(DownloadDisplay::new(&identifier)))
    } else {
        None
    };

    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
        display.clone().map(|d| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
            Arc::new(move |p| d.update(p))
        });

    let result = ia_core::download::download_item(
        client,
        &identifier,
        &opts,
        progress,
    )
    .await
    .context(format!("failed to download {}", identifier))?;

    if let Some(d) = display {
        d.finish(&result);
    }

    // Summary for -q mode
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

    Ok(())
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
