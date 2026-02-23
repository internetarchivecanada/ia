use anyhow::{bail, Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;
use std::path::PathBuf;

use ia_core::joblog;

#[derive(Args)]
#[command(
    long_about = "Show a summary of a job log file. Displays total operations, success/failure/skip \
        counts, and lists any failed files with error messages.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># View job log summary</dim>\n  <bold>$ ia status --joblog downloads.jsonl</bold>\n"
    ),
)]
pub struct StatusArgs {
    /// Path to job log file
    #[arg(long)]
    pub joblog: PathBuf,
}

pub async fn run(args: StatusArgs) -> Result<()> {
    let path = &args.joblog;

    if !path.exists() {
        bail!("joblog not found: {}", path.display());
    }

    let entries = joblog::read(path).context("failed to read joblog")?;

    if entries.is_empty() {
        println!("Job log: {} (empty)", path.display());
        return Ok(());
    }

    let summary = joblog::summarize(&entries);

    // File modification time
    let mtime_str = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            let elapsed = t.elapsed().ok()?;
            let secs = elapsed.as_secs();
            if secs < 60 {
                Some(format!("{secs}s ago"))
            } else if secs < 3600 {
                Some(format!("{}m ago", secs / 60))
            } else if secs < 86400 {
                Some(format!("{}h ago", secs / 3600))
            } else {
                Some(format!("{}d ago", secs / 86400))
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!(
        "Job log: {} (last updated {})\n",
        style(path.display()).bold(),
        mtime_str
    );

    println!("  Total operations: {:>6}", summary.total);

    if summary.succeeded > 0 {
        let pct = 100.0 * summary.succeeded as f64 / summary.total as f64;
        println!(
            "  {} Succeeded:      {:>6} ({:.1}%)",
            style("✓").green(),
            summary.succeeded,
            pct
        );
    }

    if summary.failed > 0 {
        let pct = 100.0 * summary.failed as f64 / summary.total as f64;
        println!(
            "  {} Failed:         {:>6} ({:.1}%)",
            style("✗").red(),
            summary.failed,
            pct
        );
    }

    if summary.skipped > 0 {
        let pct = 100.0 * summary.skipped as f64 / summary.total as f64;
        println!(
            "  {} Skipped:        {:>6} ({:.1}%)",
            style("○").yellow(),
            summary.skipped,
            pct
        );
    }

    // Show failed items
    let failed = joblog::failed_files(&entries);
    if !failed.is_empty() {
        println!("\n  Failed files:");
        for (item, file) in &failed {
            // Find the error message for this file
            let error_msg = entries
                .iter()
                .rev()
                .find(|e| e.item == *item && e.file == *file && e.status == "error")
                .and_then(|e| e.error.clone())
                .unwrap_or_else(|| "unknown error".to_string());

            println!(
                "    {}/{:<30} {}",
                style(item).dim(),
                file,
                style(&error_msg).red()
            );
        }

        println!(
            "\n  Run {} to retry failures.",
            style("ia download --retry-failed --joblog <file>").cyan()
        );
    }

    Ok(())
}
