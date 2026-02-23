use anyhow::{bail, Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;
use futures::StreamExt;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

use ia_core::disk_pool::DiskPool;
use ia_core::download::{
    DownloadOpts, DownloadProgress, DownloadStatus, FileDownloadResult, ItemDownloadResult,
};
use ia_core::error::IaError;
use ia_core::files::FileFilter;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::search::SearchOpts;
use ia_core::types::FileSource;
use ia_core::IaClient;

use crate::output::DownloadDisplay;

#[derive(Args)]
#[command(
    long_about = "Download files from the Internet Archive. Downloads all files from one or more \
        items, with options to filter by format, glob pattern, or source type. Supports batch \
        downloads via search queries or item lists.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Download all files from an item</dim>\n  <bold>$ ia download nasa</bold>\
         \n\n  <dim># Download only MP4 files</dim>\n  <bold>$ ia download nasa --glob \"*.mp4\"</bold>\
         \n\n  <dim># Batch download items matching a search query</dim>\n  <bold>$ ia download --search \"collection:nasa AND mediatype:movies\"</bold>\
         \n\n  <dim># Download with JSON output (for scripts/agents)</dim>\n  <bold>$ ia download nasa --json</bold>\n"
    ),
)]
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
        _ => Err(format!("unknown source: {s} (expected: original, derivative, metadata)")),
    }
}

/// Extract an identifier from a line, handling both plain text and JSONL formats.
///
/// Supports:
///   - Plain identifier: `my-item-id`
///   - JSONL from `ia search --json`: `{"identifier": "my-item-id", ...}`
fn parse_identifier_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    // Try to parse as JSON if it looks like a JSON object
    if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(id) = obj.get("identifier").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }

    Some(trimmed.to_string())
}

/// Collect all identifiers from args, --itemlist file, --search, and stdin.
async fn collect_identifiers(args: &DownloadArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = args.identifiers.clone();

    if let Some(path) = &args.itemlist {
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
        if !std::io::stdin().is_terminal() {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let line = line.context("failed to read from stdin")?;
                if let Some(id) = parse_identifier_line(&line) {
                    ids.push(id);
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
    if args.json && args.dashboard {
        bail!("--json and --dashboard are mutually exclusive");
    }

    let mut identifiers = collect_identifiers(&args, client).await?;

    // Detect file paths passed as identifiers and suggest --itemlist
    for id in &identifiers {
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
        let display = if !args.json && quiet == 0 {
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
    let batch_display = if !json_mode && quiet == 0 {
        Some(Arc::new(crate::output::BatchDisplay::new(identifiers.len(), jobs)))
    } else {
        None
    };

    let on_item_start: Option<ia_core::download::OnItemStartFn> =
        batch_display.clone().map(|bd| -> ia_core::download::OnItemStartFn {
            Arc::new(move |id, current, total| bd.on_item_start(id, current, total))
        });

    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
        batch_display.clone().map(|bd| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
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
        batch_display.clone().map(|bd| -> ia_core::download::OnItemCompleteFn {
            Arc::new(move |result| bd.on_item_complete(result))
        })
    };

    let result = ia_core::download::download_batch(
        client,
        identifiers,
        &opts,
        semaphore,
        progress,
        on_item_start,
        on_item_complete,
        args.items,
    )
    .await;

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
                    jl.write(
                        &JoblogEntry::new("download", id, "").error(&err.to_string(), 0),
                    );
                }
            }
        }
    }

    // Print summary
    if !json_mode && quiet < 2 {
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
fn json_item_result(result: &std::result::Result<ItemDownloadResult, (String, IaError)>) -> serde_json::Value {
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

    // --- parse_identifier_line tests ---

    #[test]
    fn parse_plain_identifier() {
        assert_eq!(
            parse_identifier_line("nasa"),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_plain_identifier_with_whitespace() {
        assert_eq!(
            parse_identifier_line("  nasa  "),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_jsonl_identifier() {
        assert_eq!(
            parse_identifier_line(r#"{"identifier": "cubanc_000418"}"#),
            Some("cubanc_000418".to_string())
        );
    }

    #[test]
    fn parse_jsonl_with_extra_fields() {
        assert_eq!(
            parse_identifier_line(
                r#"{"identifier": "nasa", "title": "NASA Images", "mediatype": "image"}"#
            ),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_empty_and_comment_lines() {
        assert_eq!(parse_identifier_line(""), None);
        assert_eq!(parse_identifier_line("  "), None);
        assert_eq!(parse_identifier_line("# comment"), None);
    }

    #[test]
    fn parse_json_without_identifier_field() {
        // JSON object without "identifier" — use raw line as fallback
        assert_eq!(
            parse_identifier_line(r#"{"title": "something"}"#),
            Some(r#"{"title": "something"}"#.to_string())
        );
    }
}

