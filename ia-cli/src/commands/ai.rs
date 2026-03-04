use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::StreamExt;

use ia_core::ai::pipeline::{PipelineConfig, PipelineSummary, ReviewMode};
use ia_core::ai::types::{AiConfig, FocusConfig, ItemAnalysis};
use ia_core::joblog::JoblogWriter;
use ia_core::search::SearchOpts;
use ia_core::IaClient;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Subcommand)]
pub enum AiCommand {
    /// Reverse changes recorded in a previous session's joblog
    #[command(
        long_about = "Reverse metadata changes from a previous AI session. Reads the joblog \
            file, finds all successful AI changes, and applies the reverse operations.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Undo all changes from a session</dim>\n  <bold>$ ia ai undo session.jsonl</bold>\
             \n\n  <dim># Preview what would be undone</dim>\n  <bold>$ ia ai undo session.jsonl --dry-run</bold>\n"
        ),
    )]
    Undo(UndoArgs),
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Path to the joblog file containing changes to reverse
    pub joblog: PathBuf,

    /// Preview reversals without applying
    #[arg(long)]
    pub dry_run: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
#[command(
    about = "AI-assisted metadata cleanup",
    long_about = "Use an LLM to suggest and apply metadata improvements for Internet Archive items. \
        Analyzes metadata fields, suggests fixes for typos, dates, missing fields, and schema \
        conformance. Review suggestions interactively or process in batch with --headless.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Interactive review of one item</dim>\n  <bold>$ ia ai nasa_photo_apollo11</bold>\
         \n\n  <dim># Headless batch processing (auto-accept all)</dim>\n  <bold>$ ia ai --headless --search \"collection:nasa\"</bold>\
         \n\n  <dim># Dry run — show suggestions without applying</dim>\n  <bold>$ ia ai --dry-run nasa</bold>\
         \n\n  <dim># Focus on date fixes only</dim>\n  <bold>$ ia ai --dates-only --itemlist items.txt</bold>\
         \n\n  <dim># Undo changes from a previous session</dim>\n  <bold>$ ia ai undo session.jsonl</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct AiArgs {
    /// Item identifier(s) to analyze
    #[arg()]
    pub identifiers: Vec<String>,

    // --- Input sources ---
    /// Read identifiers from file (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// Use search results as input
    #[arg(long)]
    pub search: Option<String>,

    // --- Modes ---
    /// Auto-accept all suggestions, output JSONL to stdout (no TUI)
    #[arg(long)]
    pub headless: bool,

    /// TUI review, save to local JSON instead of writing to IA
    #[arg(long)]
    pub record_only: bool,

    /// Show suggestions without applying any changes
    #[arg(long)]
    pub dry_run: bool,

    // --- LLM configuration ---
    /// LLM API base URL (default: https://api.openai.com/v1)
    #[arg(long)]
    pub base_url: Option<String>,

    /// LLM API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// Model name (default: gpt-4o-mini)
    #[arg(long)]
    pub model: Option<String>,

    /// Sampling temperature (default: 0.2)
    #[arg(long)]
    pub temperature: Option<f64>,

    /// Max tokens in response (default: 4096)
    #[arg(long)]
    pub max_tokens: Option<u64>,

    // --- Focus flags ---
    /// Only suggest date-related changes
    #[arg(long)]
    pub dates_only: bool,

    /// Only suggest title changes
    #[arg(long)]
    pub titles_only: bool,

    /// Only suggest description changes
    #[arg(long)]
    pub descriptions_only: bool,

    /// Only fill empty/missing fields
    #[arg(long)]
    pub missing_fields: bool,

    /// Only fix schema conformance issues
    #[arg(long)]
    pub schema_fix: bool,

    /// Only fix typos
    #[arg(long)]
    pub typos: bool,

    /// Only suggest changes to these fields (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub only_fields: Vec<String>,

    /// Never suggest changes to these fields (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub exclude_fields: Vec<String>,

    // --- Prompt ---
    /// Path to custom prompt file (appended to system prompt)
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,

    /// Override the entire system prompt
    #[arg(long)]
    pub system_prompt: Option<String>,

    // --- Performance ---
    /// Concurrent LLM requests (default: 1)
    #[arg(long, default_value = "1")]
    pub ai_jobs: usize,

    /// Items to prefetch ahead (default: 5)
    #[arg(long, default_value = "5")]
    pub prefetch: usize,

    /// Stop after this many total tokens
    #[arg(long)]
    pub max_tokens_budget: Option<u64>,

    // --- Output ---
    /// Write accepted changes to JSON file (record-only mode)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Output results as JSON/JSONL
    #[arg(long)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<AiCommand>,
}

impl AiArgs {
    /// Build a FocusConfig from the CLI flags.
    pub fn build_focus_config(&self) -> FocusConfig {
        FocusConfig {
            dates: self.dates_only,
            titles: self.titles_only,
            descriptions: self.descriptions_only,
            missing_fields: self.missing_fields,
            schema_fix: self.schema_fix,
            typos: self.typos,
            only_fields: if self.only_fields.is_empty() {
                None
            } else {
                Some(self.only_fields.clone())
            },
            exclude_fields: if self.exclude_fields.is_empty() {
                None
            } else {
                Some(self.exclude_fields.clone())
            },
            prompt_file: self.prompt_file.clone(),
            system_prompt_override: self.system_prompt.clone(),
        }
    }

    /// Build an AiConfig from CLI flags, falling back to the provided base config.
    pub fn build_ai_config(&self, base: AiConfig) -> AiConfig {
        AiConfig {
            base_url: self.base_url.clone().unwrap_or(base.base_url),
            api_key: self.api_key.clone().or(base.api_key),
            model: self.model.clone().unwrap_or(base.model),
            temperature: self.temperature.unwrap_or(base.temperature),
            max_tokens: self.max_tokens.unwrap_or(base.max_tokens),
        }
    }
}

pub async fn run(
    client: &IaClient,
    args: AiArgs,
    quiet: u8,
    _jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    // Handle undo subcommand
    if let Some(AiCommand::Undo(undo_args)) = args.command {
        let undo_writer = joblog_path
            .as_ref()
            .map(|p| JoblogWriter::open(p))
            .transpose()?;
        let summary = ia_core::ai::undo::undo_from_joblog(
            client,
            &undo_args.joblog,
            undo_writer.as_ref(),
            undo_args.dry_run,
        )
        .await?;

        if quiet == 0 && !undo_args.json {
            if undo_args.dry_run {
                eprintln!("{}", style("Undo dry run complete").yellow());
            } else {
                eprintln!("{}", style("Undo complete").green().bold());
            }
            eprintln!("  Items undone: {}", summary.items_undone);
            eprintln!("  Changes reversed: {}", summary.changes_reversed);
            if summary.items_skipped > 0 {
                eprintln!("  Items skipped: {}", summary.items_skipped);
            }
            if summary.items_errored > 0 {
                eprintln!(
                    "  Items errored: {}",
                    style(summary.items_errored).red()
                );
            }
        }
        if undo_args.json {
            println!("{}", serde_json::to_string(&summary)?);
        }
        return Ok(());
    }

    // Validate input sources
    if args.identifiers.is_empty()
        && args.itemlist.is_none()
        && args.search.is_none()
    {
        bail!(
            "no input specified. Provide identifiers, --itemlist, or --search.\n\
             Run 'ia ai --help' for usage."
        );
    }

    // Collect identifiers
    let identifiers = collect_identifiers(&args, client).await?;
    if identifiers.is_empty() {
        bail!("no identifiers to process");
    }

    // Build AI config: config file → env vars → CLI flags
    let base_config = client.config().ai.clone().unwrap_or_default();
    let ai_config = args.build_ai_config(base_config);

    // Validate API key is available
    if ai_config.api_key.is_none() {
        bail!(
            "no LLM API key configured. Set one via:\n\
             \x20 --api-key <KEY>\n\
             \x20 IA_AI_API_KEY environment variable\n\
             \x20 [ai] api_key in ia.ini"
        );
    }

    let focus = args.build_focus_config();

    // Determine review mode
    let review_mode = if args.headless {
        ReviewMode::Headless
    } else if args.record_only {
        ReviewMode::RecordOnly
    } else {
        ReviewMode::Interactive
    };

    let joblog_writer = joblog_path
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()?;

    let item_count = identifiers.len();
    let dry_run = args.dry_run;
    let json_output = args.json;
    let prefetch = args.prefetch;

    if quiet == 0 && !args.headless {
        eprintln!(
            "{} Analyzing {} item{}...",
            style("●").cyan(),
            item_count,
            if item_count == 1 { "" } else { "s" }
        );
    }

    // For interactive mode, create channels to bridge the TUI between
    // the analyzer and writer stages of the pipeline, plus a shared
    // shutdown signal so the TUI can stop the pipeline when the user quits.
    let (tui_review_tx, tui_review_rx, shutdown_tx_opt, tui_channels) =
        if review_mode == ReviewMode::Interactive {
            let (analysis_tx, analysis_rx) = mpsc::channel::<ItemAnalysis>(prefetch);
            let (reviewed_tx, reviewed_rx) = mpsc::channel::<ItemAnalysis>(32);
            let (shutdown_tx, _) = watch::channel(false);
            let shutdown_for_tui = shutdown_tx.clone();
            (
                Some(analysis_tx),
                Some(reviewed_rx),
                Some(shutdown_tx),
                Some((analysis_rx, reviewed_tx, shutdown_for_tui)),
            )
        } else {
            (None, None, None, None)
        };

    let pipeline_config = PipelineConfig {
        ai_config,
        focus,
        review_mode,
        dry_run,
        ai_jobs: args.ai_jobs,
        prefetch,
        max_tokens_budget: args.max_tokens_budget,
        joblog_writer,
        output_file: args.output.clone(),
        tui_review_tx,
        tui_review_rx,
        shutdown_tx: shutdown_tx_opt,
    };

    let ia_client = Arc::new(client.clone());

    let summary = if let Some((analysis_rx, reviewed_tx, shutdown_for_tui)) = tui_channels {
        // Interactive mode: run pipeline and TUI concurrently
        let pipeline_fut = ia_core::ai::pipeline::run_pipeline(
            ia_client,
            identifiers,
            pipeline_config,
        );
        let tui_fut = crate::tui::ai::run_ai_tui(
            analysis_rx,
            reviewed_tx,
            shutdown_for_tui,
            item_count as u64,
        );

        let (pipeline_result, tui_result) = tokio::join!(pipeline_fut, tui_fut);
        tui_result?;
        pipeline_result?
    } else {
        // Headless / record-only: no TUI
        ia_core::ai::pipeline::run_pipeline(
            ia_client,
            identifiers,
            pipeline_config,
        )
        .await?
    };

    // Print summary
    if quiet == 0 && !json_output {
        print_summary(&summary, dry_run);
    }

    if json_output {
        println!("{}", serde_json::to_string(&summary)?);
    }

    Ok(())
}

/// Collect identifiers from args, --itemlist, --search, and stdin.
async fn collect_identifiers(args: &AiArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = args.identifiers.clone();

    if let Some(ref path) = args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    if let Some(ref query) = args.search {
        let opts = SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    // Read from stdin if no other sources
    if ids.is_empty()
        && args.itemlist.is_none()
        && args.search.is_none()
        && !std::io::stdin().is_terminal()
    {
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

    Ok(ids)
}

fn print_summary(summary: &PipelineSummary, dry_run: bool) {
    eprintln!();
    if dry_run {
        eprintln!("{}", style("Dry run complete (no changes applied)").yellow());
    } else {
        eprintln!("{}", style("Complete").green().bold());
    }
    eprintln!(
        "  Items analyzed: {}",
        summary.items_analyzed
    );
    if summary.items_with_changes > 0 {
        eprintln!(
            "  Items with changes: {}",
            summary.items_with_changes
        );
    }
    if summary.changes_applied > 0 {
        eprintln!(
            "  Changes applied: {}",
            summary.changes_applied
        );
    }
    if summary.changes_rejected > 0 {
        eprintln!(
            "  Changes rejected: {}",
            summary.changes_rejected
        );
    }
    if summary.items_skipped > 0 {
        eprintln!(
            "  Items skipped: {}",
            summary.items_skipped
        );
    }
    if summary.items_errored > 0 {
        eprintln!(
            "  Items errored: {}",
            style(summary.items_errored).red()
        );
    }
    if summary.total_prompt_tokens > 0 || summary.total_completion_tokens > 0 {
        eprintln!(
            "  Tokens: {} prompt + {} completion",
            summary.total_prompt_tokens,
            summary.total_completion_tokens
        );
    }
    eprintln!("  Elapsed: {:.1}s", summary.elapsed_secs);
}
