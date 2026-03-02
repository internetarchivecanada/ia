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
         \n  <dim># View job log summary</dim>\n  <bold>$ ia status --joblog downloads.jsonl</bold>\n\
         \n  <dim># Machine-readable status output</dim>\n  <bold>$ ia status --joblog downloads.jsonl --json</bold>\n"
    ),
)]
pub struct StatusArgs {
    /// Path to job log file
    #[arg(long)]
    pub joblog: PathBuf,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: StatusArgs) -> Result<()> {
    let path = &args.joblog;

    if !path.exists() {
        bail!("joblog not found: {}", path.display());
    }

    let entries = joblog::read(path).context("failed to read joblog")?;

    if entries.is_empty() {
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "total": 0,
                    "succeeded": 0,
                    "failed": 0,
                    "skipped": 0,
                    "failures": []
                })
            );
        } else {
            println!("Job log: {} (empty)", path.display());
        }
        return Ok(());
    }

    let summary = joblog::summarize(&entries);

    if args.json {
        let failed = joblog::failed_files(&entries);
        let failures: Vec<serde_json::Value> = failed
            .iter()
            .map(|(item, file)| {
                let error_msg = entries
                    .iter()
                    .rev()
                    .find(|e| e.item == *item && e.file == *file && e.status == "error")
                    .and_then(|e| e.error.clone())
                    .unwrap_or_else(|| "unknown error".to_string());
                serde_json::json!({"item": item, "file": file, "error": error_msg})
            })
            .collect();

        println!(
            "{}",
            serde_json::json!({
                "total": summary.total,
                "succeeded": summary.succeeded,
                "failed": summary.failed,
                "skipped": summary.skipped,
                "failures": failures,
            })
        );
        if summary.failed > 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

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

    // Show AI section if present
    if let Some(ai) = joblog::ai_summarize(&entries) {
        println!("\n  {}", style("AI Operations").bold().underlined());
        println!("  Items analyzed:     {:>6}", ai.items_analyzed);
        if ai.items_with_changes > 0 {
            println!(
                "  Items with changes: {:>6}",
                ai.items_with_changes
            );
        }
        if ai.changes_applied > 0 {
            println!(
                "  Changes applied:    {:>6}",
                ai.changes_applied
            );
        }
        if ai.items_errored > 0 {
            println!(
                "  Items errored:      {:>6}",
                style(ai.items_errored).red()
            );
        }
        if ai.items_skipped > 0 {
            println!(
                "  Items skipped:      {:>6}",
                ai.items_skipped
            );
        }
        if ai.prompt_tokens > 0 || ai.completion_tokens > 0 {
            let total_tokens = ai.prompt_tokens + ai.completion_tokens;
            println!(
                "  Tokens used:        {:>6} ({} prompt + {} completion)",
                total_tokens, ai.prompt_tokens, ai.completion_tokens
            );
        }
        if ai.undos > 0 {
            println!(
                "  Undo operations:    {:>6} ({} changes reversed)",
                ai.undos, ai.changes_reversed
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_output_shape() {
        let output = serde_json::json!({
            "total": 150,
            "succeeded": 140,
            "failed": 8,
            "skipped": 2,
            "failures": [
                {"item": "x", "file": "y.jpg", "error": "timeout"},
            ],
        });
        assert_eq!(output["total"], 150);
        assert_eq!(output["succeeded"], 140);
        assert_eq!(output["failed"], 8);
        assert_eq!(output["skipped"], 2);
        assert_eq!(output["failures"][0]["item"], "x");
        assert_eq!(output["failures"][0]["file"], "y.jpg");
        assert_eq!(output["failures"][0]["error"], "timeout");
    }

    #[test]
    fn json_empty_output_shape() {
        let output = serde_json::json!({
            "total": 0,
            "succeeded": 0,
            "failed": 0,
            "skipped": 0,
            "failures": [],
        });
        assert_eq!(output["total"], 0);
        assert!(output["failures"].as_array().unwrap().is_empty());
    }
}
