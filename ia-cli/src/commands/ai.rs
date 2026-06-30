use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

use ia_core::search::SearchOpts;
use ia_core::IaClient;

/// Options passed to `run_qa_single` for each item.
struct QaSingleOpts<'a> {
    ai_config_path: Option<&'a std::path::Path>,
    qa_opts: &'a ia_core::ai::qa::QaOpts,
    promote_opts: &'a Option<ia_core::ai::promote::PromoteOpts>,
    quiet: u8,
    json: bool,
    print_prompt: bool,
    image_urls: bool,
    image_quality: ia_core::ai::image::ImageQuality,
    /// When true, suppress per-item verbose stderr (progress bar mode).
    compact: bool,
}

/// `--print-prompt` output: mirrors the exact JSON request body sent to the
/// OpenAI-compatible chat completions API. The `messages` array, `model`,
/// `temperature`, and `max_tokens` are all top-level fields in the real
/// request — so this output IS the API request, plus `identifier` for context.
#[derive(Debug, serde::Serialize)]
struct QaPromptInfo {
    identifier: String,
    provider: String,
    model: String,
    temperature: f64,
    max_tokens: u32,
    messages: Vec<QaPromptMessage>,
}

/// A single message in the chat completions `messages` array.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "role")]
enum QaPromptMessage {
    /// The system message that sets the QA agent's behavior.
    #[serde(rename = "system")]
    System { content: String },
    /// The user message containing page images and the verification prompt.
    /// Content is an ordered array: images first, then the text prompt.
    #[serde(rename = "user")]
    User { content: Vec<QaContentPart> },
}

/// A single part within a user message's `content` array.
///
/// Tagged with `"type"` to match the OpenAI vision API format.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type")]
enum QaContentPart {
    /// A page image sent as a URL for vision analysis.
    #[serde(rename = "image_url")]
    ImageUrl {
        url: String,
        member_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The text prompt containing extracted metadata and schema for verification.
    #[serde(rename = "text")]
    Text { text: String },
}

// ── Top-level AI command ────────────────────────────────────────────────

#[derive(Args)]
#[command(
    about = "AI metadata extraction QA and configuration",
    long_about = "Verify AI-extracted metadata using vision-based LLM QA, manage AI extraction \
        configurations for collections, and promote confirmed metadata to items.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># QA extraction results for an item</dim>\n  <bold>$ ia ai qa my-item</bold>\
         \n\n  <dim># QA with JSON output</dim>\n  <bold>$ ia ai qa --json --search \"collection:theses\"</bold>\
         \n\n  <dim># Show AI config for a collection</dim>\n  <bold>$ ia ai config show theses-and-dissertations</bold>\
         \n\n  <dim># Create a new AI config</dim>\n  <bold>$ ia ai config create my-collection --model gpt-5-nano</bold>\n"
    ),
)]
pub struct AiArgs {
    /// Path to a local AI Config JSON file (overrides collection lookup)
    #[arg(long, global = true)]
    pub ai_config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: AiCommand,
}

#[derive(Subcommand)]
pub enum AiCommand {
    /// Verify AI-extracted metadata using vision-based LLM QA
    #[command(
        long_about = "Run a QA pipeline that fetches AI-extracted metadata for items, downloads \
            page images from the item's JP2 zip, and sends both to a second LLM model for \
            verification. Produces per-field verdicts with confidence scores.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># QA a single item</dim>\n  <bold>$ ia ai qa my-item</bold>\
             \n\n  <dim># QA items from a search, output JSONL</dim>\n  <bold>$ ia ai qa --json --search \"collection:theses\"</bold>\
             \n\n  <dim># Save results to XLSX and JSONL</dim>\n  <bold>$ ia ai qa --search \"collection:theses\" -o results.xlsx -o results.jsonl</bold>\
             \n\n  <dim># Re-process cached results into XLSX (no LLM calls)</dim>\n  <bold>$ ia ai qa --from-results results.jsonl -o results.xlsx</bold>\
             \n\n  <dim># QA and promote confirmed metadata</dim>\n  <bold>$ ia ai qa --promote --confidence 0.9 item1 item2</bold>\
             \n\n  <dim># Dry run — show what would be promoted</dim>\n  <bold>$ ia ai qa --promote --dry-run my-item</bold>\
             \n\n  <dim># Use Anthropic API directly</dim>\n  <bold>$ ia ai qa --base-url https://api.anthropic.com --model claude-sonnet-4-6 item1</bold>\
             \n\n  <dim># Local model (no API key needed)</dim>\n  <bold>$ ia ai qa --base-url http://localhost:11434/v1 --model llava:34b item1</bold>\n"
        ),
    )]
    Qa(QaArgs),

    /// Manage AI extraction configurations for collections
    #[command(
        long_about = "Read, create, and edit AI Config JSON files stored in collection items. \
            These configs define the LLM model, prompt, page selection, and response schema \
            used by the AI Metadata Extractor derive module.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Show a collection's AI config</dim>\n  <bold>$ ia ai config show theses-and-dissertations</bold>\
             \n\n  <dim># Create with defaults</dim>\n  <bold>$ ia ai config create my-collection</bold>\
             \n\n  <dim># Create from a file</dim>\n  <bold>$ ia ai config create my-collection --from-file config.json</bold>\n"
        ),
    )]
    Config(ConfigArgs),

    /// Reverse changes recorded in a previous session's joblog
    #[cfg(feature = "ai-analyze")]
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

    /// AI-assisted metadata analysis (standalone extraction)
    #[cfg(feature = "ai-analyze")]
    #[command(
        long_about = "Use an LLM to suggest and apply metadata improvements for Internet Archive items. \
            Analyzes metadata fields, suggests fixes for typos, dates, missing fields, and schema \
            conformance. Review suggestions interactively or process in batch with --headless."
    )]
    Analyze(AnalyzeArgs),
}

// ── QA subcommand ───────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct QaArgs {
    /// Item identifier(s) to QA
    pub identifiers: Vec<String>,

    // --- Input sources ---
    /// Read identifiers from file (one per line)
    #[arg(long)]
    pub itemlist: Option<PathBuf>,

    /// QA items matching a search query
    #[arg(long)]
    pub search: Option<String>,

    /// Extra search parameters (key:value or key=value, repeatable)
    #[arg(long = "search-parameter")]
    pub search_parameters: Vec<String>,

    // --- QA LLM configuration ---
    /// QA LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// LLM API base URL
    #[arg(long)]
    pub base_url: Option<String>,

    /// LLM API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// LLM API provider (openai or anthropic; auto-detected from base URL if omitted)
    #[arg(long)]
    pub provider: Option<String>,

    /// LLM temperature
    #[arg(long, default_value = "0.2")]
    pub temperature: f64,

    /// Min overall confidence for promotion
    #[arg(long, default_value = "0.8")]
    pub confidence: f64,

    /// Min per-field confidence
    #[arg(long, default_value = "0.6")]
    pub min_field_confidence: f64,

    // --- Output ---
    /// Output results as JSONL to stdout
    #[arg(long)]
    pub json: bool,

    /// Write results to file(s). Format inferred from extension: .xlsx, .jsonl, .csv, .tsv
    ///
    /// Multiple -o flags are supported to write several formats in one run.
    /// XLSX produces a two-sheet workbook (Items summary + Fields detail).
    /// CSV/TSV produces the Fields layout (one row per field per item).
    #[arg(short = 'o', long = "output")]
    pub outputs: Vec<PathBuf>,

    /// Re-process cached QA results from a JSONL file (no LLM calls)
    ///
    /// Reads a JSONL file of QaResult objects (from a previous --json or -o run)
    /// and formats them into -o outputs. No network access required.
    #[arg(long)]
    pub from_results: Option<PathBuf>,

    /// Write confirmed metadata to items after QA
    #[arg(long)]
    pub promote: bool,

    /// Show what would be done without making changes
    #[arg(long)]
    pub dry_run: bool,

    /// Print the prompt that would be sent to the LLM and exit
    #[arg(long)]
    pub print_prompt: bool,

    /// Estimate cost without processing items
    #[arg(long)]
    pub estimate: bool,

    /// Image quality for vision requests [default: medium]
    ///
    /// Controls the resolution of page images sent to the LLM.
    /// Lower quality = fewer tokens = lower cost, but less detail.
    #[arg(long, default_value = "medium", value_parser = parse_image_quality,
        long_help = "Image quality for vision requests.\n\n\
            Controls the maximum pixel dimension of page images sent to the LLM.\n\
            Anthropic charges (width × height) / 750 tokens per image.\n\n\
            Levels (approximate cost for 8 pages with Sonnet):\n  \
            high   — 1568px, ~143 DPI, ≈$0.048 (fine print, handwriting)\n  \
            medium — 1024px, ~93 DPI,  ≈$0.034 (standard printed text) [default]\n  \
            low    — 768px,  ~70 DPI,  ≈$0.023 (large print, titles only)\n  \
            min    — 512px,  ~47 DPI,  ≈$0.012 (covers, simple verification)")]
    pub image_quality: ia_core::ai::image::ImageQuality,

    /// Send image URLs to the LLM instead of downloading and base64-encoding
    #[arg(long)]
    pub image_urls: bool,

    /// Interactive TUI for reviewing QA results
    #[arg(long)]
    #[cfg(feature = "tui")]
    pub dashboard: bool,
}

// ── Config subcommand ───────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Display AI config for a collection
    Show(ConfigShowArgs),
    /// Create a new AI config for a collection
    Create(ConfigCreateArgs),
    /// Edit an existing AI config
    Edit(ConfigEditArgs),
}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// Collection identifier
    pub collection: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ConfigCreateArgs {
    /// Collection identifier
    pub collection: String,
    /// Create from an existing JSON file
    #[arg(long)]
    pub from_file: Option<PathBuf>,
    /// LLM model name
    #[arg(long)]
    pub model: Option<String>,
    /// Extraction prompt text
    #[arg(long)]
    pub prompt: Option<String>,
    /// Read prompt from file
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    /// Page specification (e.g., "cover,normal:5")
    #[arg(long)]
    pub pages: Option<String>,
    /// JSON Schema file for response format
    #[arg(long)]
    pub schema_file: Option<PathBuf>,
    /// Show the config JSON without uploading
    #[arg(long)]
    pub dry_run: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ConfigEditArgs {
    /// Collection identifier
    pub collection: String,
    /// Open in $EDITOR
    #[arg(long)]
    pub editor: bool,
    /// Update model name
    #[arg(long)]
    pub set_model: Option<String>,
    /// Update prompt text
    #[arg(long)]
    pub set_prompt: Option<String>,
    /// Update prompt from file
    #[arg(long)]
    pub set_prompt_file: Option<PathBuf>,
    /// Update page specification
    #[arg(long)]
    pub set_pages: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

// ── Shelved analyze/undo args (behind ai-analyze feature) ───────────────

#[cfg(feature = "ai-analyze")]
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

#[cfg(feature = "ai-analyze")]
#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    /// Item identifier(s) to analyze
    pub identifiers: Vec<String>,
    #[arg(long)]
    pub itemlist: Option<PathBuf>,
    #[arg(long)]
    pub search: Option<String>,
    /// Extra search parameters for --search (key:value or key=value, repeatable)
    #[arg(long = "search-parameter")]
    pub search_parameters: Vec<String>,
    #[arg(long)]
    pub headless: bool,
    #[arg(long)]
    pub record_only: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub base_url: Option<String>,
    #[arg(long)]
    pub api_key: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub temperature: Option<f64>,
    #[arg(long)]
    pub max_tokens: Option<u64>,
    #[arg(long)]
    pub dates_only: bool,
    #[arg(long)]
    pub titles_only: bool,
    #[arg(long)]
    pub descriptions_only: bool,
    #[arg(long)]
    pub missing_fields: bool,
    #[arg(long)]
    pub schema_fix: bool,
    #[arg(long)]
    pub typos: bool,
    #[arg(long, value_delimiter = ',')]
    pub only_fields: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    pub exclude_fields: Vec<String>,
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub system_prompt: Option<String>,
    #[arg(long, default_value = "1")]
    pub ai_jobs: usize,
    #[arg(long, default_value = "5")]
    pub prefetch: usize,
    #[arg(long)]
    pub max_tokens_budget: Option<u64>,
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

// ── Entry point ─────────────────────────────────────────────────────────

pub async fn run(
    client: &IaClient,
    args: AiArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    let ai_config_path = args.ai_config;
    match args.command {
        AiCommand::Qa(qa_args) => {
            run_qa(
                client,
                qa_args,
                ai_config_path,
                quiet,
                jobs,
                joblog_path,
                no_resume,
            )
            .await
        }
        AiCommand::Config(config_args) => run_config(client, config_args, quiet).await,
        #[cfg(feature = "ai-analyze")]
        AiCommand::Undo(undo_args) => run_undo(client, undo_args, quiet, joblog_path).await,
        #[cfg(feature = "ai-analyze")]
        AiCommand::Analyze(analyze_args) => {
            run_analyze(client, analyze_args, quiet, jobs, joblog_path).await
        }
    }
}

// ── QA implementation ───────────────────────────────────────────────────

async fn run_qa(
    client: &IaClient,
    args: QaArgs,
    ai_config_path: Option<PathBuf>,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    // ── Validate -o extensions ────────────────────────────────────────
    for output_path in &args.outputs {
        if !ia_core::ai::qa_output::is_supported_extension(output_path) {
            bail!(
                "unsupported output format: {} (expected .xlsx, .jsonl, .csv, or .tsv)",
                output_path.display()
            );
        }
    }

    // ── --from-results mode: read cached JSONL, format into -o outputs ──
    if let Some(ref from_results_path) = args.from_results {
        // Mutual exclusions
        if args.search.is_some() {
            bail!("--from-results cannot be combined with --search");
        }
        if args.itemlist.is_some() {
            bail!("--from-results cannot be combined with --itemlist");
        }
        if !args.identifiers.is_empty() {
            bail!("--from-results cannot be combined with positional identifiers");
        }
        if args.promote {
            bail!("--from-results cannot be combined with --promote");
        }

        let results = ia_core::ai::qa_output::read_jsonl(from_results_path).context(format!(
            "failed to read results from {}",
            from_results_path.display()
        ))?;

        if quiet == 0 && !args.json {
            eprintln!(
                "{} Loaded {} result{} from {}",
                style("●").cyan(),
                results.len(),
                if results.len() == 1 { "" } else { "s" },
                from_results_path.display(),
            );
        }

        // Show stderr display (per-item verdicts)
        if quiet == 0 && !args.json {
            for result in &results {
                let verdict_style = match result.verdict {
                    ia_core::ai::qa::QaVerdict::Pass => style("PASS").green().bold(),
                    ia_core::ai::qa::QaVerdict::Fail => style("FAIL").red().bold(),
                    ia_core::ai::qa::QaVerdict::NeedsReview => style("REVIEW").yellow().bold(),
                };
                eprintln!(
                    "  {} {} (confidence: {:.0}%, {} fields)",
                    verdict_style,
                    result.identifier,
                    result.overall_confidence * 100.0,
                    result.fields.len(),
                );
            }
        }

        // --json to stdout
        if args.json {
            for result in &results {
                println!("{}", serde_json::to_string(result).unwrap_or_default());
            }
        }

        // Write -o outputs
        for output_path in &args.outputs {
            ia_core::ai::qa_output::write_results(&results, output_path).context(format!(
                "failed to write output to {}",
                output_path.display()
            ))?;
            if quiet == 0 && !args.json {
                eprintln!("{} Wrote {}", style("✓").green(), output_path.display(),);
            }
        }

        return Ok(());
    }

    // Validate input sources
    if args.identifiers.is_empty() && args.itemlist.is_none() && args.search.is_none() {
        // Check stdin
        if std::io::stdin().is_terminal() {
            bail!(
                "no input specified. Provide identifiers, --itemlist, --search, or pipe stdin.\n\
                 Run 'ia ai qa --help' for usage."
            );
        }
    }

    if args.print_prompt && args.promote {
        bail!("--print-prompt cannot be used with --promote (nothing to promote)");
    }

    if args.estimate && args.promote {
        bail!("--estimate cannot be combined with --promote");
    }

    // Collect identifiers (skipped for --estimate + --search, which uses num_found)
    let search_params = crate::commands::search::parse_extra_params(&args.search_parameters)?;
    let search_opts = SearchOpts {
        params: search_params,
        ..SearchOpts::default()
    };
    let identifiers = if args.estimate && args.search.is_some() {
        // --estimate with --search: skip full scrape, handled in estimate block
        Vec::new()
    } else {
        let search = args.search.as_deref().map(|q| (q, &search_opts));
        let ids = crate::identifier::collect_identifiers(
            &args.identifiers,
            args.itemlist.as_deref(),
            search,
            client,
        )
        .await?;
        if ids.is_empty() {
            bail!("no identifiers to process");
        }
        ids
    };

    let mut identifiers = identifiers;
    let original_item_count = identifiers.len();
    let mut items_already_done: usize = 0;

    // ── Auto-resume: skip already-completed items ────────────────────
    if !no_resume && !args.print_prompt {
        if let Some(ref path) = joblog_path {
            if path.exists() {
                let entries = ia_core::joblog::read(path).context(format!(
                    "failed to read joblog for resume: {}",
                    path.display()
                ))?;
                let done = ia_core::joblog::successful_items(&entries, "ai-qa");
                if !done.is_empty() {
                    let before = identifiers.len();
                    identifiers.retain(|id| !done.contains(id));
                    items_already_done = before - identifiers.len();
                    if quiet == 0 && !args.json {
                        eprintln!(
                            "{} Resuming: {} item{} already QA'd (from {})",
                            style("▸").cyan(),
                            items_already_done,
                            if items_already_done == 1 { "" } else { "s" },
                            path.display(),
                        );
                    }
                    if identifiers.is_empty() {
                        if quiet == 0 && !args.json {
                            eprintln!("{} All {} items already QA'd", style("✓").green(), before,);
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    // ── Open joblog writer ───────────────────────────────────────────
    let joblog = if !args.print_prompt {
        joblog_path
            .as_ref()
            .map(|p| ia_core::joblog::JoblogWriter::open(p))
            .transpose()
            .context("failed to open joblog")?
    } else {
        None
    };

    // Build QA LLM config: CLI flag → [ai-qa] → [ai] → error
    let qa_overlay = client.config().ai_qa.clone().unwrap_or_default();
    let ai_base = client.config().ai.as_ref();

    let base_url = args
        .base_url
        .or(qa_overlay.base_url)
        .or(ai_base.map(|c| c.base_url.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no LLM base URL configured for QA. Set one via:\n\
                 \x20 --base-url <URL>\n\
                 \x20 IA_AI_QA_BASE_URL environment variable\n\
                 \x20 [ai-qa] base_url in ia.ini\n\
                 \x20 [ai] base_url in ia.ini"
            )
        })?;

    let model = args
        .model
        .or(qa_overlay.model)
        .or(ai_base.map(|c| c.model.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no LLM model configured for QA. Set one via:\n\
                 \x20 --model <NAME>\n\
                 \x20 IA_AI_QA_MODEL environment variable\n\
                 \x20 [ai-qa] model in ia.ini\n\
                 \x20 [ai] model in ia.ini"
            )
        })?;

    let api_key = args
        .api_key
        .or(qa_overlay.api_key)
        .or(ai_base.and_then(|c| c.api_key.clone()));

    // Parse --provider flag, then cascade through config layers, then auto-detect
    let provider_flag = args
        .provider
        .map(|s| {
            s.parse::<ia_core::ai::types::Provider>()
                .map_err(|e| anyhow::anyhow!(e))
        })
        .transpose()?;
    let provider = provider_flag
        .or(qa_overlay.provider)
        .or(ai_base.map(|c| c.provider))
        .unwrap_or_else(|| ia_core::ai::types::Provider::detect(&base_url));

    // Localhost URLs (ollama, vLLM, etc.) don't require API keys
    let is_localhost =
        base_url.starts_with("http://localhost") || base_url.starts_with("http://127.0.0.1");
    if api_key.is_none() && !is_localhost && !args.estimate {
        bail!(
            "no LLM API key configured for QA. Set one via:\n\
             \x20 --api-key <KEY>\n\
             \x20 IA_AI_QA_API_KEY environment variable\n\
             \x20 [ai-qa] api_key in ia.ini\n\
             \x20 [ai] api_key in ia.ini\n\n\
             Tip: API keys are not required for localhost URLs."
        );
    }

    let ai_config = ia_core::ai::types::AiConfig {
        base_url,
        api_key,
        model,
        temperature: args.temperature,
        max_tokens: 4096,
        provider,
    };

    // ── --estimate: show cost estimate and exit ───────────────────────
    if args.estimate {
        let image_quality = args.image_quality;

        // Item count: num_found for --search, identifiers.len() otherwise
        let item_count = if let Some(ref query) = args.search {
            ia_core::search::num_found(client, query, &search_opts.params)
                .await
                .context("failed to get search count")? as usize
        } else {
            identifiers.len()
        };

        if item_count == 0 {
            bail!("no items to estimate");
        }

        // Page count from AI config (no sample item needed)
        let page_count = if let Some(ref path) = ai_config_path {
            let cfg = ia_core::ai::ia_config::load_ai_config_from_file(path)?;
            ia_core::ai::ia_config::compute_page_count(&cfg.result.page_info)
        } else {
            // Default from the uoftgovpubs-style config: cover + title + other + normal:5 = 8
            // This is a reasonable default; actual count varies per collection config.
            8
        };

        // Estimate: image tokens + ~1,100 text tokens (system prompt + metadata + schema)
        // Output: ~100 tokens per field, assume ~8 fields
        let est_text_tokens: u64 = 1_100;
        let est_output_tokens: u64 = 800;
        let est_image_tokens = page_count as u64 * image_quality.est_tokens_per_image();
        let est_input = est_image_tokens + est_text_tokens;
        let per_item_cost = estimate_vision_cost(&ai_config.model, est_input, est_output_tokens);

        eprintln!(
            "{} Cost estimate for {} item{}:",
            style("●").cyan(),
            style(item_count).bold(),
            if item_count == 1 { "" } else { "s" },
        );
        eprintln!("  Model: {}", ai_config.model);
        eprintln!(
            "  Image quality: {} ({}px max)",
            image_quality,
            image_quality.max_dimension(),
        );
        eprintln!("  Pages/item: ~{}", page_count);
        eprintln!(
            "  Est. tokens/item: ~{} input + ~{} output",
            est_input, est_output_tokens,
        );
        if let Some(cost) = per_item_cost {
            eprintln!("  Est. cost/item: ≈${:.4}", cost);
            eprintln!(
                "  {}",
                style(format!("Est. total: ≈${:.2}", cost * item_count as f64)).bold(),
            );
        }

        // Show comparison across quality levels
        eprintln!();
        eprintln!("  Cost at each quality level:");
        for q in [
            ia_core::ai::image::ImageQuality::High,
            ia_core::ai::image::ImageQuality::Medium,
            ia_core::ai::image::ImageQuality::Low,
            ia_core::ai::image::ImageQuality::Min,
        ] {
            let qi = page_count as u64 * q.est_tokens_per_image() + est_text_tokens;
            let qcost = estimate_vision_cost(&ai_config.model, qi, est_output_tokens);
            let marker = if q == image_quality { " ←" } else { "" };
            if let Some(c) = qcost {
                eprintln!(
                    "    {:6} — ≈${:.4}/item, ≈${:.2} total{}",
                    q,
                    c,
                    c * item_count as f64,
                    marker,
                );
            }
        }

        return Ok(());
    }

    let llm_client = ia_core::ai::client::LlmClient::new(ai_config.clone())?;

    let item_count = identifiers.len();
    if quiet == 0 && !args.json {
        // Compute estimated total cost for the header
        let page_count = if let Some(ref path) = ai_config_path {
            ia_core::ai::ia_config::load_ai_config_from_file(path)
                .map(|cfg| ia_core::ai::ia_config::compute_page_count(&cfg.result.page_info))
                .unwrap_or(8)
        } else {
            8 // default: cover + title + other + normal:5
        };
        let est_image_tokens = page_count as u64 * args.image_quality.est_tokens_per_image();
        let est_input = est_image_tokens + 1_100;
        let est_output: u64 = 800;
        let est_per_item = estimate_vision_cost(&ai_config.model, est_input, est_output);
        let est_total_str = est_per_item
            .map(|c| format!(", est. ≈${:.2}", c * original_item_count as f64))
            .unwrap_or_default();

        eprintln!(
            "{} QA checking {} item{} ({}{})...",
            style("●").cyan(),
            original_item_count,
            if original_item_count == 1 { "" } else { "s" },
            ai_config.model,
            est_total_str,
        );
    }

    let qa_opts = ia_core::ai::qa::QaOpts {
        model: ai_config.model.clone(),
        temperature: args.temperature,
        max_tokens: 4096,
        confidence_threshold: args.confidence,
        min_field_confidence: args.min_field_confidence,
        dry_run: args.dry_run,
    };

    let promote_opts = if args.promote {
        Some(ia_core::ai::promote::PromoteOpts {
            confidence_threshold: args.confidence,
            min_field_confidence: args.min_field_confidence,
            fields: None,
            dry_run: args.dry_run,
        })
    } else {
        None
    };

    let image_quality = args.image_quality;
    let has_outputs = !args.outputs.is_empty();

    // Compact mode: suppress per-item verbose stderr when -o is active
    let compact = has_outputs && !args.json && quiet == 0;

    let single_opts = QaSingleOpts {
        ai_config_path: ai_config_path.as_deref(),
        qa_opts: &qa_opts,
        promote_opts: &promote_opts,
        quiet,
        json: args.json,
        print_prompt: args.print_prompt,
        image_urls: args.image_urls,
        image_quality,
        compact,
    };

    use futures::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    let pass_count = std::sync::Arc::new(AtomicU64::new(0));
    let fail_count = std::sync::Arc::new(AtomicU64::new(0));
    let review_count = std::sync::Arc::new(AtomicU64::new(0));
    let error_count = std::sync::Arc::new(AtomicU64::new(0));
    let joblog = std::sync::Arc::new(joblog);
    let stdout_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let llm_client = std::sync::Arc::new(llm_client);

    // Collect results for -o output (only when -o flags are present)
    let collected_results: std::sync::Arc<tokio::sync::Mutex<Vec<ia_core::ai::qa::QaResult>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

    // Collect errors for end-of-run display (compact mode)
    let collected_errors: std::sync::Arc<tokio::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

    // Running cost accumulator (in dollars, stored as f64 bits in AtomicU64)
    let total_cost_bits = std::sync::Arc::new(AtomicU64::new(0));
    let model_name = ai_config.model.clone();

    // Progress bar for compact (-o) mode
    let progress_bar: Option<indicatif::ProgressBar> = if compact {
        use indicatif::{ProgressBar, ProgressStyle};
        let bar = ProgressBar::new(original_item_count as u64);
        bar.set_style(
            ProgressStyle::with_template(&format!(
                "  {{bar:{}.cyan/dim}} {{pos}}/{{len}} items  {{msg}}  ({{per_sec}})",
                crate::output::BAR_WIDTH,
            ))
            .unwrap()
            .progress_chars(crate::output::PROGRESS_CHARS),
        );
        bar.set_position(items_already_done as u64);
        bar.set_message("≈$0.000 spent");
        Some(bar)
    } else {
        None
    };
    let progress_bar = std::sync::Arc::new(progress_bar);

    let is_print_prompt = args.print_prompt;
    let is_dry_run = args.dry_run;
    let is_json = args.json;
    let image_urls = single_opts.image_urls;

    futures::stream::iter(identifiers.iter().cloned())
        .map(|identifier| {
            let pass_count = std::sync::Arc::clone(&pass_count);
            let fail_count = std::sync::Arc::clone(&fail_count);
            let review_count = std::sync::Arc::clone(&review_count);
            let error_count = std::sync::Arc::clone(&error_count);
            let joblog = std::sync::Arc::clone(&joblog);
            let stdout_lock = std::sync::Arc::clone(&stdout_lock);
            let llm_client = std::sync::Arc::clone(&llm_client);
            let collected_results = std::sync::Arc::clone(&collected_results);
            let collected_errors = std::sync::Arc::clone(&collected_errors);
            let total_cost_bits = std::sync::Arc::clone(&total_cost_bits);
            let progress_bar = std::sync::Arc::clone(&progress_bar);
            let model_name = model_name.clone();
            let single_opts = QaSingleOpts {
                ai_config_path: ai_config_path.as_deref(),
                qa_opts: &qa_opts,
                promote_opts: &promote_opts,
                quiet,
                json: is_json,
                print_prompt: is_print_prompt,
                image_urls,
                image_quality,
                compact,
            };

            async move {
                match run_qa_single(client, &llm_client, &identifier, &single_opts).await {
                    Ok(result) => {
                        if !is_print_prompt {
                            // Write joblog entry: all verdicts are valid results (status "ok")
                            if let Some(ref jl) = *joblog {
                                if is_dry_run {
                                    jl.write(
                                        &ia_core::joblog::JoblogEntry::new(
                                            "ai-qa",
                                            &identifier,
                                            "",
                                        )
                                        .skipped(),
                                    );
                                } else {
                                    let tokens = result.token_usage.as_ref().map(|u| {
                                        ia_core::ai::types::JoblogTokens {
                                            prompt: u.prompt_tokens,
                                            completion: u.completion_tokens,
                                        }
                                    });
                                    jl.write(
                                        &ia_core::joblog::JoblogEntry::new(
                                            "ai-qa",
                                            &identifier,
                                            "",
                                        )
                                        .ai_ok(
                                            vec![],
                                            tokens,
                                            result.elapsed_ms,
                                        ),
                                    );
                                }
                            }

                            match result.verdict {
                                ia_core::ai::qa::QaVerdict::Pass => {
                                    pass_count.fetch_add(1, Ordering::Relaxed);
                                }
                                ia_core::ai::qa::QaVerdict::Fail => {
                                    fail_count.fetch_add(1, Ordering::Relaxed);
                                }
                                ia_core::ai::qa::QaVerdict::NeedsReview => {
                                    review_count.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            // Accumulate cost from actual token usage
                            if let Some(ref usage) = result.token_usage {
                                if let Some(cost) = estimate_vision_cost(
                                    &model_name,
                                    usage.prompt_tokens,
                                    usage.completion_tokens,
                                ) {
                                    // Add cost using atomic CAS on f64 bits
                                    let cost_bits = cost.to_bits();
                                    let _ = total_cost_bits.fetch_update(
                                        Ordering::Relaxed,
                                        Ordering::Relaxed,
                                        |old| {
                                            let old_f = f64::from_bits(old);
                                            Some((old_f + f64::from_bits(cost_bits)).to_bits())
                                        },
                                    );
                                }
                            }

                            if is_json {
                                let _guard = stdout_lock.lock().await;
                                println!("{}", serde_json::to_string(&result).unwrap_or_default());
                            }

                            // Update progress bar
                            if let Some(ref bar) = *progress_bar {
                                bar.inc(1);
                                let spent = f64::from_bits(total_cost_bits.load(Ordering::Relaxed));
                                bar.set_message(format!("≈${spent:.3} spent"));
                            }

                            // Collect for -o output
                            if has_outputs {
                                collected_results.lock().await.push(result);
                            }
                        }
                    }
                    Err(e) => {
                        error_count.fetch_add(1, Ordering::Relaxed);
                        // Write joblog error entry for infrastructure failures
                        if let Some(ref jl) = *joblog {
                            jl.write(
                                &ia_core::joblog::JoblogEntry::new("ai-qa", &identifier, "")
                                    .ai_error(&e.to_string(), 0),
                            );
                        }

                        // Collect error for end-of-run display (compact mode)
                        if compact {
                            collected_errors
                                .lock()
                                .await
                                .push(format!("{}: {:#}", identifier, e,));
                        }

                        // Update progress bar (errors still count as progress)
                        if let Some(ref bar) = *progress_bar {
                            bar.inc(1);
                        }

                        if !compact {
                            let _guard = stdout_lock.lock().await;
                            if is_json {
                                let json_err = serde_json::json!({
                                    "identifier": identifier,
                                    "error": e.to_string(),
                                });
                                eprintln!(
                                    "{}",
                                    serde_json::to_string(&json_err).unwrap_or_default()
                                );
                            } else if quiet == 0 {
                                eprintln!("  {} {}: {:#}", style("✗").red(), identifier, e);
                            }
                        }
                    }
                }
            }
        })
        .buffer_unordered(jobs)
        .collect::<Vec<()>>()
        .await;

    // Finish progress bar
    if let Some(ref bar) = *progress_bar {
        bar.finish_and_clear();
    }

    let pass_count = pass_count.load(Ordering::Relaxed);
    let fail_count = fail_count.load(Ordering::Relaxed);
    let review_count = review_count.load(Ordering::Relaxed);
    let error_count = error_count.load(Ordering::Relaxed);
    let total_cost = f64::from_bits(total_cost_bits.load(Ordering::Relaxed));

    // Print summary
    if quiet == 0 && !args.json && !args.print_prompt {
        if compact {
            // Batch-style summary for -o mode
            let succeeded = pass_count + fail_count + review_count;
            eprintln!(
                "{}",
                style("────────────────────────────────────────────────────").dim()
            );
            if error_count > 0 {
                eprintln!(
                    "{}/{} items ({} done) · {} error{}",
                    succeeded,
                    original_item_count,
                    style(succeeded).green(),
                    style(error_count).red(),
                    if error_count == 1 { "" } else { "s" },
                );
            } else {
                eprintln!(
                    "{}/{} items ({} done)",
                    succeeded,
                    original_item_count,
                    style(succeeded).green(),
                );
            }
            if pass_count > 0 || fail_count > 0 || review_count > 0 {
                eprintln!(
                    "  Pass: {}  Fail: {}  Review: {}",
                    style(pass_count).green(),
                    style(fail_count).red(),
                    style(review_count).yellow(),
                );
            }
            // Show collected errors
            let errors = collected_errors.lock().await;
            for (i, err) in errors.iter().enumerate() {
                if i >= 5 {
                    eprintln!(
                        "  ... and {} more error{}",
                        errors.len() - 5,
                        if errors.len() - 5 == 1 { "" } else { "s" },
                    );
                    break;
                }
                eprintln!("  {} {}", style("✗").red(), err);
            }
            if total_cost > 0.0 {
                eprintln!("Cost: ${total_cost:.3}");
            }
        } else {
            // Verbose summary (non -o mode)
            eprintln!();
            if args.dry_run {
                eprintln!("{}", style("Dry run complete").yellow());
            } else {
                eprintln!("{}", style("QA complete").green().bold());
            }
            eprintln!("  Items: {item_count}");
            if pass_count > 0 {
                eprintln!("  Pass: {}", style(pass_count).green());
            }
            if fail_count > 0 {
                eprintln!("  Fail: {}", style(fail_count).red());
            }
            if review_count > 0 {
                eprintln!("  Needs review: {}", style(review_count).yellow());
            }
            if error_count > 0 {
                eprintln!("  Errors: {}", style(error_count).red());
            }
        }
    }

    // Write -o outputs
    if !args.outputs.is_empty() && !args.print_prompt {
        let results = collected_results.lock().await;
        for output_path in &args.outputs {
            ia_core::ai::qa_output::write_results(&results, output_path).context(format!(
                "failed to write output to {}",
                output_path.display()
            ))?;
            if quiet == 0 && !args.json {
                eprintln!(
                    "  {} Wrote {} ({} results)",
                    style("✓").green(),
                    output_path.display(),
                    results.len(),
                );
            }
        }
    }

    Ok(())
}

/// Run QA for a single item: fetch metadata, config, images, call LLM, optionally promote.
async fn run_qa_single(
    client: &IaClient,
    llm_client: &ia_core::ai::client::LlmClient,
    identifier: &str,
    opts: &QaSingleOpts<'_>,
) -> Result<ia_core::ai::qa::QaResult> {
    // 1. Fetch item metadata
    let item = client
        .get_item(identifier)
        .await
        .context(format!("failed to fetch metadata for {identifier}"))?;

    // 2. Get extracted metadata from the item (already fetched above)
    let extracted = ia_core::ai::extracted_metadata::extract_from_item(&item)
        .context(format!("no extracted metadata for {identifier}"))?;

    // 3. Resolve AI config: local file takes priority over collection chain
    let config = match opts.ai_config_path {
        Some(path) => ia_core::ai::ia_config::load_ai_config_from_file(path)
            .context("failed to load --ai-config file")?,
        None => {
            let (_collection, config) = ia_core::ai::ia_config::resolve_ai_config(client, &item)
                .await
                .context(format!("no AI config found for {identifier}"))?;
            config
        }
    };

    // 4. Find JP2 zip and list contents
    let zip_filename = ia_core::ai::zip::find_jp2_zip(identifier, &item.files)
        .ok_or_else(|| anyhow::anyhow!("no JP2 zip found for {identifier}"))?;

    let entries = ia_core::ai::zip::list_zip_contents(client, identifier, &zip_filename)
        .await
        .context(format!("failed to list zip contents for {identifier}"))?;

    let zip_paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();

    // 4a. Select pages via scandata (preferred) or fall back to index-based
    let scandata_pages = match ia_core::scandata::find_scandata_file_from_item(&item) {
        Some(sd_file) => {
            match ia_core::scandata::fetch_scandata(client, identifier, &sd_file).await {
                Ok(pages) => Some(pages),
                Err(e) => {
                    if opts.quiet == 0 && !opts.json {
                        eprintln!(
                            "    {} scandata parse failed, using index-based fallback: {e}",
                            style("⚠").yellow(),
                        );
                    }
                    None
                }
            }
        }
        None => {
            if opts.quiet == 0 && !opts.json {
                eprintln!(
                    "    {} no scandata.xml found, using index-based page selection",
                    style("⚠").yellow(),
                );
            }
            None
        }
    };

    // Build the list of (zip_member_path, Option<ScandataPage>) for selected pages
    let selected: Vec<(String, Option<ia_core::scandata::ScandataPage>)> =
        if let Some(ref all_pages) = scandata_pages {
            // Scandata-based: match extractor behavior
            let page_info = &config.result.page_info;
            let mut all_indices: Vec<usize> = Vec::new();

            for info in page_info {
                let type_str = match info.page_type {
                    ia_core::ai::ia_config::PageType::Cover => "cover",
                    ia_core::ai::ia_config::PageType::Title => "title",
                    ia_core::ai::ia_config::PageType::Normal => "normal",
                    ia_core::ai::ia_config::PageType::Other => "other",
                };
                let mut matching = ia_core::scandata::find_pages_by_type(all_pages, type_str);
                if let Some(count) = info.count {
                    matching.truncate(count);
                }
                all_indices.extend(matching);
            }

            // Dedup + sort by page order (matching extractor)
            all_indices.sort_unstable();
            all_indices.dedup();

            all_indices
                .iter()
                .filter_map(|&idx| {
                    let page = &all_pages[idx];
                    let zip_path = ia_core::scandata::leaf_to_zip_path(page.leaf_num, &zip_paths)?;
                    Some((zip_path, Some(page.clone())))
                })
                .collect()
        } else {
            // Fallback: index-based selection (old behavior)
            let old_selected = ia_core::ai::zip::select_pages(&entries, &config.result.page_info);
            old_selected.into_iter().map(|p| (p, None)).collect()
        };

    if selected.is_empty() {
        bail!(
            "no page images found for {identifier} \
             (zip had {} entries, config requested: {})",
            entries.len(),
            format_page_info(&config.result.page_info),
        );
    }

    // 4b. --print-prompt: show what would be sent to the LLM
    if opts.print_prompt {
        let mut user_content: Vec<QaContentPart> = selected
            .iter()
            .map(|(zip_path, sd_page)| {
                let url = ia_core::ai::zip::page_image_url(
                    client,
                    identifier,
                    &zip_filename,
                    zip_path,
                    "jpg",
                );
                let label = sd_page
                    .as_ref()
                    .map(|p| p.page_type.to_lowercase())
                    .filter(|t| t != "normal");
                QaContentPart::ImageUrl {
                    url,
                    member_path: zip_path.clone(),
                    label,
                }
            })
            .collect();

        let user_message = ia_core::ai::qa::build_qa_user_message(&extracted, &config);
        user_content.push(QaContentPart::Text { text: user_message });

        let system_prompt = ia_core::ai::qa::QA_SYSTEM_PROMPT;

        let messages = vec![
            QaPromptMessage::System {
                content: system_prompt.to_string(),
            },
            QaPromptMessage::User {
                content: user_content,
            },
        ];

        if opts.json {
            let prompt_info = QaPromptInfo {
                identifier: identifier.to_string(),
                provider: llm_client.config().provider.to_string(),
                model: opts.qa_opts.model.clone(),
                temperature: opts.qa_opts.temperature,
                max_tokens: opts.qa_opts.max_tokens,
                messages,
            };
            println!("{}", serde_json::to_string(&prompt_info)?);
        } else {
            for msg in &messages {
                match msg {
                    QaPromptMessage::System { content } => {
                        println!("{}", style("messages[0] role: system").bold());
                        for line in content.lines().take(5) {
                            println!("  {line}");
                        }
                        let total = content.lines().count();
                        if total > 5 {
                            println!("  ... ({} more lines)", total - 5);
                        }
                        println!();
                    }
                    QaPromptMessage::User { content } => {
                        let image_count = content
                            .iter()
                            .filter(|p| matches!(p, QaContentPart::ImageUrl { .. }))
                            .count();
                        println!(
                            "{} ({image_count} images + text)",
                            style("messages[1] role: user").bold()
                        );
                        for (i, part) in content.iter().enumerate() {
                            match part {
                                QaContentPart::ImageUrl {
                                    url,
                                    member_path,
                                    label,
                                } => {
                                    let label_str = label
                                        .as_ref()
                                        .map(|l| format!("  ({l})"))
                                        .unwrap_or_default();
                                    println!(
                                        "  [{}] {} {}{label_str}",
                                        i + 1,
                                        style("image_url:").cyan(),
                                        url
                                    );
                                    println!("      member: {member_path}");
                                }
                                QaContentPart::Text { text } => {
                                    println!("  [{}] {}:", i + 1, style("text").cyan());
                                    for line in text.lines().take(10) {
                                        println!("      {line}");
                                    }
                                    let total_lines = text.lines().count();
                                    if total_lines > 10 {
                                        println!("      ... ({} more lines)", total_lines - 10);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            println!();
            println!(
                "Provider: {}  Model: {}  Temperature: {}  Max tokens: {}",
                llm_client.config().provider,
                opts.qa_opts.model,
                opts.qa_opts.temperature,
                opts.qa_opts.max_tokens
            );
        }

        return Ok(ia_core::ai::qa::QaResult {
            identifier: identifier.to_string(),
            overall_confidence: 0.0,
            verdict: ia_core::ai::qa::QaVerdict::NeedsReview,
            extraction_model: extracted.result.ai_request_info.model.clone(),
            qa_model: opts.qa_opts.model.clone(),
            fields: indexmap::IndexMap::new(),
            token_usage: None,
            elapsed_ms: 0,
            existing_metadata: None,
            pages_sent: None,
        });
    }

    // 5. Download images, then crop/rotate/resize to match extractor behavior
    // Dry-run skips downloads — qa_item returns a placeholder without using images.
    let page_images = if opts.qa_opts.dry_run {
        ia_core::ai::qa::PageImages::Base64(Vec::new())
    } else if opts.image_urls {
        // URL mode: no processing possible (LLM fetches the images directly)
        let urls: Vec<(String, String)> = selected
            .iter()
            .map(|(zip_path, _)| {
                let url = ia_core::ai::zip::page_image_url(
                    client,
                    identifier,
                    &zip_filename,
                    zip_path,
                    "jpg",
                );
                (zip_path.clone(), url)
            })
            .collect();
        ia_core::ai::qa::PageImages::Urls(urls)
    } else {
        // Download + process each page
        let futs: Vec<_> = selected
            .iter()
            .map(|(zip_path, _sd_page)| {
                let zip_path = zip_path.clone();
                let zip_filename = zip_filename.clone();
                async move {
                    let raw_data = ia_core::ai::zip::download_zip_member_converted(
                        client,
                        identifier,
                        &zip_filename,
                        &zip_path,
                        "jpg",
                    )
                    .await
                    .context(format!("failed to download page image {zip_path}"))?;

                    // Derivative JP2s are already rotated+oriented by the derive
                    // pipeline — only resize for cost savings. (Crop/rotate would
                    // need the _orig_ imagestack, which is what the extractor uses.)
                    let processed = match ia_core::ai::image::resize_only(
                        &raw_data,
                        opts.image_quality.max_dimension(),
                    ) {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!(
                                "    {} resize failed for {zip_path}: {e}",
                                console::style("⚠").yellow(),
                            );
                            raw_data
                        }
                    };

                    Ok::<_, anyhow::Error>((zip_path, processed))
                }
            })
            .collect();
        let images = futures::future::try_join_all(futs).await?;
        ia_core::ai::qa::PageImages::Base64(images)
    };

    if !opts.json && opts.quiet == 0 && !opts.compact {
        let page_count = if opts.qa_opts.dry_run {
            selected.len()
        } else {
            page_images.len()
        };
        eprintln!(
            "  {} {} ({} pages, extraction: {})",
            style("●").cyan(),
            identifier,
            page_count,
            extracted.result.ai_request_info.model,
        );
    }

    // 6. Run QA
    let mut result = ia_core::ai::qa::qa_item(
        llm_client,
        identifier,
        &config,
        &extracted,
        &page_images,
        opts.qa_opts,
    )
    .await
    .context(format!("QA failed for {identifier}"))?;

    // 6b. Enrich result with existing metadata and pages_sent for output
    {
        // Existing metadata: serialize item metadata, pick the fields that were QA'd
        let item_json = serde_json::to_value(&item.metadata).unwrap_or_default();
        if let serde_json::Value::Object(all_meta) = item_json {
            let mut existing = serde_json::Map::new();
            for field_name in result.fields.keys() {
                if let Some(val) = all_meta.get(field_name) {
                    existing.insert(field_name.clone(), val.clone());
                }
            }
            if !existing.is_empty() {
                result.existing_metadata = Some(existing);
            }
        }

        // Pages sent: extract from selected pages vec
        let pages_sent: Vec<ia_core::ai::qa::PageSent> = selected
            .iter()
            .filter_map(|(_, sd_page)| {
                let page = sd_page.as_ref()?;
                Some(ia_core::ai::qa::PageSent {
                    leaf_num: page.leaf_num,
                    page_type: page.page_type.to_lowercase(),
                })
            })
            .collect();
        if !pages_sent.is_empty() {
            result.pages_sent = Some(pages_sent);
        }
    }

    // 7. Print per-item summary (non-JSON mode)
    if !opts.json && opts.quiet == 0 && !opts.compact {
        if opts.qa_opts.dry_run {
            // Dry-run: show extracted metadata that would be checked, no fake verdicts
            eprintln!(
                "    {} fields to verify:",
                style(result.fields.len()).bold(),
            );
            for (field_name, field_result) in &result.fields {
                let value_str = format_field_value(&field_result.extracted_value);
                if value_str.is_empty() {
                    eprintln!(
                        "      {}: {}",
                        style(field_name).bold(),
                        style("(empty)").dim()
                    );
                } else {
                    eprintln!("      {}: {}", style(field_name).bold(), value_str);
                }
            }
        } else {
            // Real run: show verdicts
            let verdict_style = match result.verdict {
                ia_core::ai::qa::QaVerdict::Pass => style("PASS").green().bold(),
                ia_core::ai::qa::QaVerdict::Fail => style("FAIL").red().bold(),
                ia_core::ai::qa::QaVerdict::NeedsReview => style("REVIEW").yellow().bold(),
            };
            eprintln!(
                "    {} (confidence: {:.0}%, {} fields)",
                verdict_style,
                result.overall_confidence * 100.0,
                result.fields.len(),
            );

            for (field_name, field_result) in &result.fields {
                let (icon, verdict_str) = match field_result.verdict {
                    ia_core::ai::qa::FieldVerdict::Correct => {
                        (style("✓").green(), style("correct").green())
                    }
                    ia_core::ai::qa::FieldVerdict::Incorrect => {
                        (style("✗").red(), style("incorrect").red())
                    }
                    ia_core::ai::qa::FieldVerdict::Uncertain => {
                        (style("?").yellow(), style("uncertain").yellow())
                    }
                };

                let value_str = format_field_value(&field_result.extracted_value);

                eprintln!(
                    "      {} {}: {} ({verdict_str}, {:.0}%)",
                    icon,
                    style(field_name).bold(),
                    value_str,
                    field_result.confidence * 100.0,
                );

                if let Some(correction) = &field_result.suggested_correction {
                    let correction_str = match correction {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    eprintln!("        → {}", style(correction_str).cyan());
                }
                if let Some(note) = &field_result.note {
                    eprintln!("        {}", style(note).dim());
                }
            }
        }
    }

    // 7b. Cost estimate
    if !opts.json && opts.quiet == 0 && !opts.compact {
        let num_pages = if opts.qa_opts.dry_run {
            selected.len()
        } else {
            page_images.len()
        };
        if let Some(usage) = &result.token_usage {
            // Actual usage from API response
            let total_tokens = usage.prompt_tokens + usage.completion_tokens;
            let cost = estimate_vision_cost(
                &opts.qa_opts.model,
                usage.prompt_tokens,
                usage.completion_tokens,
            );
            eprintln!(
                "      {} tokens: {} input + {} output{}",
                style("⊘").dim(),
                usage.prompt_tokens,
                usage.completion_tokens,
                if let Some(c) = cost {
                    format!(" (≈${c:.4})")
                } else {
                    format!(" ({total_tokens} total)")
                },
            );
        } else {
            // Estimate for dry-run
            let (est_input, est_output) =
                estimate_token_counts(num_pages, &extracted, &config, opts.image_quality);
            let cost = estimate_vision_cost(&opts.qa_opts.model, est_input, est_output);
            eprintln!(
                "      {} est. tokens: ~{} input + ~{} output{}",
                style("⊘").dim(),
                est_input,
                est_output,
                if let Some(c) = cost {
                    format!(" (≈${c:.4})")
                } else {
                    String::new()
                },
            );
        }
    }

    // 8. Promote if requested
    if let Some(promote) = opts.promote_opts {
        let promote_result =
            ia_core::ai::promote::promote_metadata(client, identifier, &result, promote)
                .await
                .context(format!("promotion failed for {identifier}"))?;

        if !opts.json && opts.quiet == 0 && !opts.compact {
            if promote_result.dry_run {
                let reason = match promote_result.fields_skipped.first() {
                    Some((_, reason)) if promote_result.fields_promoted.is_empty() => {
                        format!(" ({reason})")
                    }
                    _ => String::new(),
                };
                eprintln!(
                    "    Would promote {} field(s), skip {}{}",
                    promote_result.fields_promoted.len(),
                    promote_result.fields_skipped.len(),
                    reason,
                );
            } else if !promote_result.fields_promoted.is_empty() {
                eprintln!(
                    "    {} Promoted {} field(s): {}",
                    style("✓").green(),
                    promote_result.fields_promoted.len(),
                    promote_result.fields_promoted.join(", "),
                );
            } else if let Some((_, reason)) = promote_result.fields_skipped.first() {
                eprintln!(
                    "    {} Skipped promotion of {} field(s): {}",
                    style("⊘").dim(),
                    promote_result.fields_skipped.len(),
                    reason,
                );
            }
        }
    }

    Ok(result)
}

// ── Config implementation ───────────────────────────────────────────────

async fn run_config(client: &IaClient, args: ConfigArgs, quiet: u8) -> Result<()> {
    match args.command {
        ConfigCommand::Show(show_args) => run_config_show(client, show_args, quiet).await,
        ConfigCommand::Create(create_args) => run_config_create(client, create_args, quiet).await,
        ConfigCommand::Edit(edit_args) => run_config_edit(client, edit_args, quiet).await,
    }
}

async fn run_config_show(client: &IaClient, args: ConfigShowArgs, quiet: u8) -> Result<()> {
    let config = ia_core::ai::ia_config::fetch_ai_config(client, &args.collection)
        .await
        .context(format!("failed to fetch AI config for {}", args.collection))?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        if quiet == 0 {
            eprintln!(
                "{} AI config for {}",
                style("●").cyan(),
                style(&args.collection).bold(),
            );
        }
        println!("Model:  {}", config.result.model_name);
        println!("Pages:  {}", format_page_info(&config.result.page_info));
        println!("Schema: {}", config.result.schema.format.name);
        println!();
        println!("Prompt:");
        for line in config.result.prompt.lines() {
            println!("  {line}");
        }
        println!();
        println!("Schema fields:");
        if let Some(props) = config.result.schema.format.schema.get("properties") {
            if let Some(obj) = props.as_object() {
                for (name, spec) in obj {
                    let field_type = spec.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                    let desc = spec
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    println!("  {name}: {field_type} — {desc}");
                }
            }
        }
    }

    Ok(())
}

async fn run_config_create(client: &IaClient, args: ConfigCreateArgs, quiet: u8) -> Result<()> {
    let config = if let Some(ref path) = args.from_file {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read config file: {}", path.display()))?;
        serde_json::from_str(&content).context("failed to parse config file as AI Config JSON")?
    } else {
        let mut config = ia_core::ai::ia_config::default_ai_config();

        if let Some(ref model) = args.model {
            config.result.model_name = model.clone();
        }
        if let Some(ref prompt) = args.prompt {
            config.result.prompt = prompt.clone();
        }
        if let Some(ref prompt_file) = args.prompt_file {
            config.result.prompt = std::fs::read_to_string(prompt_file).context(format!(
                "failed to read prompt file: {}",
                prompt_file.display()
            ))?;
        }
        if let Some(ref pages) = args.pages {
            config.result.page_info = parse_page_spec(pages)?;
        }
        if let Some(ref schema_file) = args.schema_file {
            let content = std::fs::read_to_string(schema_file).context(format!(
                "failed to read schema file: {}",
                schema_file.display()
            ))?;
            let schema: serde_json::Value =
                serde_json::from_str(&content).context("failed to parse schema file as JSON")?;
            config.result.schema.format.schema = schema;
        }

        config
    };

    if args.dry_run {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&config)?);
        } else {
            if quiet == 0 {
                eprintln!("{}", style("Dry run — config not uploaded").yellow());
            }
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        return Ok(());
    }

    ia_core::ai::ia_config::create_ai_config(client, &args.collection, &config)
        .await
        .context(format!(
            "failed to create AI config for {}",
            args.collection
        ))?;

    if quiet == 0 && !args.json {
        eprintln!(
            "{} Created AI config for {}",
            style("✓").green(),
            style(&args.collection).bold(),
        );
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    }

    Ok(())
}

async fn run_config_edit(client: &IaClient, args: ConfigEditArgs, quiet: u8) -> Result<()> {
    let mut config = ia_core::ai::ia_config::fetch_ai_config(client, &args.collection)
        .await
        .context(format!("failed to fetch AI config for {}", args.collection))?;

    let mut changed = false;

    if let Some(ref model) = args.set_model {
        config.result.model_name = model.clone();
        changed = true;
    }
    if let Some(ref prompt) = args.set_prompt {
        config.result.prompt = prompt.clone();
        changed = true;
    }
    if let Some(ref prompt_file) = args.set_prompt_file {
        config.result.prompt = std::fs::read_to_string(prompt_file).context(format!(
            "failed to read prompt file: {}",
            prompt_file.display()
        ))?;
        changed = true;
    }
    if let Some(ref pages) = args.set_pages {
        config.result.page_info = parse_page_spec(pages)?;
        changed = true;
    }

    if !changed && !args.editor {
        bail!("no changes specified. Use --set-model, --set-prompt, --set-pages, or --editor.");
    }

    if args.editor {
        // Write config to temp file, open in $EDITOR, read back
        let tmp = tempfile::NamedTempFile::with_suffix(".json")?;
        std::fs::write(tmp.path(), serde_json::to_string_pretty(&config)?)?;
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(&editor)
            .arg(tmp.path())
            .status()
            .context(format!("failed to launch editor: {editor}"))?;
        if !status.success() {
            bail!("editor exited with non-zero status");
        }
        let content = std::fs::read_to_string(tmp.path())?;
        config = serde_json::from_str(&content)
            .context("failed to parse edited config as AI Config JSON")?;
    }

    ia_core::ai::ia_config::update_ai_config(client, &args.collection, &config)
        .await
        .context(format!(
            "failed to update AI config for {}",
            args.collection
        ))?;

    if quiet == 0 && !args.json {
        eprintln!(
            "{} Updated AI config for {}",
            style("✓").green(),
            style(&args.collection).bold(),
        );
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    }

    Ok(())
}

// ── Shelved: analyze + undo (behind ai-analyze feature) ─────────────────

#[cfg(feature = "ai-analyze")]
async fn run_undo(
    client: &IaClient,
    undo_args: UndoArgs,
    quiet: u8,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    let undo_writer = joblog_path
        .as_ref()
        .map(|p| ia_core::joblog::JoblogWriter::open(p))
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
            eprintln!("  Items errored: {}", style(summary.items_errored).red());
        }
    }
    if undo_args.json {
        println!("{}", serde_json::to_string(&summary)?);
    }
    Ok(())
}

#[cfg(feature = "ai-analyze")]
async fn run_analyze(
    client: &IaClient,
    args: AnalyzeArgs,
    quiet: u8,
    _jobs: usize,
    joblog_path: Option<PathBuf>,
) -> Result<()> {
    use ia_core::ai::pipeline::{PipelineConfig, ReviewMode};
    use ia_core::ai::types::{AiConfig, FocusConfig, ItemAnalysis};
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};

    if args.identifiers.is_empty() && args.itemlist.is_none() && args.search.is_none() {
        bail!(
            "no input specified. Provide identifiers, --itemlist, or --search.\n\
             Run 'ia ai analyze --help' for usage."
        );
    }

    let search_opts = SearchOpts {
        params: crate::commands::search::parse_extra_params(&args.search_parameters)?,
        ..SearchOpts::default()
    };
    let search = args.search.as_deref().map(|q| (q, &search_opts));
    let identifiers = crate::identifier::collect_identifiers(
        &args.identifiers,
        args.itemlist.as_deref(),
        search,
        client,
    )
    .await?;
    if identifiers.is_empty() {
        bail!(crate::identifier::empty_input_message(
            args.search.as_deref(),
            args.itemlist.as_deref(),
            "no identifiers to process",
        ));
    }

    let base_config = client.config().ai.clone().unwrap_or_default();
    let ai_config = AiConfig {
        base_url: args.base_url.unwrap_or(base_config.base_url),
        api_key: args.api_key.or(base_config.api_key),
        model: args.model.unwrap_or(base_config.model),
        temperature: args.temperature.unwrap_or(base_config.temperature),
        max_tokens: args.max_tokens.unwrap_or(base_config.max_tokens),
        provider: base_config.provider,
    };

    if ai_config.api_key.is_none() {
        bail!(
            "no LLM API key configured. Set one via:\n\
             \x20 --api-key <KEY>\n\
             \x20 IA_AI_API_KEY environment variable\n\
             \x20 [ai] api_key in ia.ini"
        );
    }

    let focus = FocusConfig {
        dates: args.dates_only,
        titles: args.titles_only,
        descriptions: args.descriptions_only,
        missing_fields: args.missing_fields,
        schema_fix: args.schema_fix,
        typos: args.typos,
        only_fields: if args.only_fields.is_empty() {
            None
        } else {
            Some(args.only_fields)
        },
        exclude_fields: if args.exclude_fields.is_empty() {
            None
        } else {
            Some(args.exclude_fields)
        },
        prompt_file: args.prompt_file,
        system_prompt_override: args.system_prompt,
    };

    let review_mode = if args.headless {
        ReviewMode::Headless
    } else if args.record_only {
        ReviewMode::RecordOnly
    } else {
        ReviewMode::Interactive
    };

    let joblog_writer = joblog_path
        .as_ref()
        .map(|p| ia_core::joblog::JoblogWriter::open(p))
        .transpose()?;
    let item_count = identifiers.len();
    let prefetch = args.prefetch;

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
        dry_run: args.dry_run,
        ai_jobs: args.ai_jobs,
        prefetch,
        max_tokens_budget: args.max_tokens_budget,
        joblog_writer,
        output_file: args.output,
        tui_review_tx,
        tui_review_rx,
        shutdown_tx: shutdown_tx_opt,
    };

    let ia_client = Arc::new(client.clone());

    let summary = if let Some((analysis_rx, reviewed_tx, shutdown_for_tui)) = tui_channels {
        let pipeline_fut =
            ia_core::ai::pipeline::run_pipeline(ia_client, identifiers, pipeline_config);
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
        ia_core::ai::pipeline::run_pipeline(ia_client, identifiers, pipeline_config).await?
    };

    if quiet == 0 && !args.json {
        print_analyze_summary(&summary, args.dry_run);
    }
    if args.json {
        println!("{}", serde_json::to_string(&summary)?);
    }

    Ok(())
}

#[cfg(feature = "ai-analyze")]
fn print_analyze_summary(summary: &ia_core::ai::pipeline::PipelineSummary, dry_run: bool) {
    eprintln!();
    if dry_run {
        eprintln!(
            "{}",
            style("Dry run complete (no changes applied)").yellow()
        );
    } else {
        eprintln!("{}", style("Complete").green().bold());
    }
    eprintln!("  Items analyzed: {}", summary.items_analyzed);
    if summary.items_with_changes > 0 {
        eprintln!("  Items with changes: {}", summary.items_with_changes);
    }
    if summary.changes_applied > 0 {
        eprintln!("  Changes applied: {}", summary.changes_applied);
    }
    if summary.changes_rejected > 0 {
        eprintln!("  Changes rejected: {}", summary.changes_rejected);
    }
    if summary.items_skipped > 0 {
        eprintln!("  Items skipped: {}", summary.items_skipped);
    }
    if summary.items_errored > 0 {
        eprintln!("  Items errored: {}", style(summary.items_errored).red());
    }
    if summary.total_prompt_tokens > 0 || summary.total_completion_tokens > 0 {
        eprintln!(
            "  Tokens: {} prompt + {} completion",
            summary.total_prompt_tokens, summary.total_completion_tokens
        );
    }
    eprintln!("  Elapsed: {:.1}s", summary.elapsed_secs);
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Format pageInfo as a human-readable spec string.
fn format_page_info(page_info: &[ia_core::ai::ia_config::PageInfo]) -> String {
    page_info
        .iter()
        .map(|p| match p.page_type {
            ia_core::ai::ia_config::PageType::Cover => "cover".to_string(),
            ia_core::ai::ia_config::PageType::Title => "title".to_string(),
            ia_core::ai::ia_config::PageType::Normal | ia_core::ai::ia_config::PageType::Other => {
                if let Some(count) = p.count {
                    format!("normal:{count}")
                } else {
                    "normal:1".to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse `--image-quality` value.
fn parse_image_quality(s: &str) -> std::result::Result<ia_core::ai::image::ImageQuality, String> {
    s.parse()
}

/// Parse a page specification string (e.g., "cover,normal:5") into PageInfo.
fn parse_page_spec(spec: &str) -> Result<Vec<ia_core::ai::ia_config::PageInfo>> {
    let mut result = Vec::new();

    for part in spec.split(',') {
        let part = part.trim();
        if part.eq_ignore_ascii_case("cover") {
            result.push(ia_core::ai::ia_config::PageInfo {
                page_type: ia_core::ai::ia_config::PageType::Cover,
                count: None,
            });
        } else if let Some(rest) = part
            .to_ascii_lowercase()
            .strip_prefix("normal:")
            .map(|r| r.to_string())
        {
            let rest = rest.as_str();
            let count: usize = rest
                .parse()
                .context(format!("invalid page count: {rest}"))?;
            result.push(ia_core::ai::ia_config::PageInfo {
                page_type: ia_core::ai::ia_config::PageType::Normal,
                count: Some(count),
            });
        } else if part.eq_ignore_ascii_case("normal") {
            result.push(ia_core::ai::ia_config::PageInfo {
                page_type: ia_core::ai::ia_config::PageType::Normal,
                count: Some(1),
            });
        } else {
            bail!("unrecognized page spec: {part:?} (expected 'cover' or 'normal:N')");
        }
    }

    Ok(result)
}

/// Format a JSON value as a human-readable field value string.
fn format_field_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            items.join("; ")
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Estimate input/output token counts for a QA vision request.
///
/// Anthropic vision: ~600 tokens per image after resize to 1536px max
/// (2 tiles at 768×768). Full-res would be ~1,600.
/// Text tokens: ~4 chars per token for system prompt + metadata + schema.
/// Output: ~100 tokens per field for structured JSON response.
fn estimate_token_counts(
    num_pages: usize,
    extracted: &ia_core::ai::extracted_metadata::ExtractedMetadata,
    config: &ia_core::ai::ia_config::IaAiConfig,
    quality: ia_core::ai::image::ImageQuality,
) -> (u64, u64) {
    let image_tokens = num_pages as u64 * quality.est_tokens_per_image();
    let system_len = ia_core::ai::qa::QA_SYSTEM_PROMPT.len() as u64;
    let metadata_len = serde_json::to_string(&extracted.result.metadata)
        .unwrap_or_default()
        .len() as u64;
    let schema_len = serde_json::to_string(&config.result.schema)
        .unwrap_or_default()
        .len() as u64;
    let text_tokens = (system_len + metadata_len + schema_len) / 4;
    let input_tokens = image_tokens + text_tokens;

    let num_fields = extracted.result.metadata.len() as u64;
    let output_tokens = num_fields * 100;

    (input_tokens, output_tokens)
}

/// Estimate USD cost from token counts and model name.
///
/// Returns `None` for unrecognized models. Pricing as of early 2026.
fn estimate_vision_cost(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    // (input $/M tokens, output $/M tokens)
    let (input_rate, output_rate) = if model.contains("sonnet") {
        (3.0, 15.0)
    } else if model.contains("opus") {
        (15.0, 75.0)
    } else if model.contains("haiku") {
        (0.80, 4.0)
    } else if model.contains("gpt-4o-mini") {
        (0.15, 0.60)
    } else if model.contains("gpt-4o") {
        (2.50, 10.0)
    } else if model.contains("gpt-5-nano") {
        (0.15, 0.60)
    } else {
        return None;
    };

    let cost =
        (input_tokens as f64 * input_rate + output_tokens as f64 * output_rate) / 1_000_000.0;
    Some(cost)
}
