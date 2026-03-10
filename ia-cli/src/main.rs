use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use color_print::cstr;
use std::path::PathBuf;

mod commands;
mod output;
#[cfg(feature = "tui")]
mod tui;

const STYLES: clap::builder::Styles = clap::builder::Styles::styled()
    .header(
        clap::builder::styling::AnsiColor::Green
            .on_default()
            .bold(),
    )
    .usage(
        clap::builder::styling::AnsiColor::Green
            .on_default()
            .bold(),
    )
    .literal(
        clap::builder::styling::AnsiColor::Cyan
            .on_default()
            .bold(),
    )
    .placeholder(clap::builder::styling::AnsiColor::Cyan.on_default());

#[derive(Parser)]
#[command(
    name = "ia",
    version,
    about = "Internet Archive command-line tool",
    long_about = "A command-line tool for interacting with the Internet Archive (archive.org).\n\
        Upload and download files, search for items, view and edit metadata, and list file contents.",
    styles = STYLES,
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Download all files from an item</dim>\n  <bold>$ ia download nasa</bold>\
         \n\n  <dim># Search for items in a collection</dim>\n  <bold>$ ia search \"collection:nasa\"</bold>\
         \n\n  <dim># View metadata for an item</dim>\n  <bold>$ ia metadata nasa</bold>\n"
    ),
)]
struct Cli {
    /// Path to configuration file
    #[arg(short = 'c', long = "config-file", global = true)]
    config: Option<PathBuf>,

    /// Enable logging
    #[arg(short = 'l', long, global = true)]
    log: bool,

    /// Enable debug output
    #[arg(short = 'd', long, global = true)]
    debug: bool,

    /// Allow insecure (HTTP) connections
    #[arg(short = 'i', long, global = true)]
    insecure: bool,

    /// Host to connect to
    #[arg(short = 'H', long, global = true)]
    host: Option<String>,

    /// Custom string to append to the default User-Agent
    #[arg(long, global = true)]
    user_agent_suffix: Option<String>,

    /// Path to job log file
    #[arg(long, global = true)]
    joblog: Option<PathBuf>,

    /// Retry failed operations from job log
    #[arg(long, global = true)]
    retry_failed: bool,

    /// Concurrent operations
    #[arg(short = 'j', long, global = true, default_value = "2")]
    jobs: usize,

    /// Suppress output (repeat for more quiet: -q summary only, -qq silent)
    #[arg(short = 'q', long, global = true, action = clap::ArgAction::Count)]
    quiet: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// AI-assisted metadata cleanup
    Ai(commands::ai::AiArgs),
    /// Download files from one or more items
    #[command(visible_alias = "do")]
    Download(commands::download::DownloadArgs),
    /// List files in an item with filtering and formatting
    #[command(visible_alias = "ls")]
    List(commands::list::ListArgs),
    /// Read or modify item metadata
    #[command(visible_alias = "md")]
    Metadata(commands::metadata::MetadataArgs),
    /// Search the Internet Archive
    #[command(visible_alias = "se")]
    Search(commands::search::SearchArgs),
    /// Show job log summary and failed operations
    Status(commands::status::StatusArgs),
    /// Upload files to the Internet Archive
    #[command(visible_alias = "up")]
    Upload(commands::upload::UploadArgs),
    /// Generate shell completions for bash, zsh, fish, etc.
    Completions(commands::completions::CompletionsArgs),
    /// Configure credentials and settings
    #[command(visible_alias = "co")]
    Config(commands::config::ConfigArgs),
    /// Update ia to a specific or latest version
    #[cfg(feature = "self-update")]
    Update(commands::update::UpdateArgs),
}

/// Derive the list of global flags that consume the next argv element as a value
/// directly from clap's parser. This keeps the compound-args pre-scanner in sync
/// with Cli's actual global options — no hardcoded list to maintain.
fn value_taking_global_flags() -> Vec<String> {
    Cli::command()
        .get_arguments()
        .filter(|a| a.is_global_set() && a.get_action().takes_values())
        .flat_map(|a| {
            let mut flags = Vec::new();
            if let Some(l) = a.get_long() {
                flags.push(format!("--{l}"));
            }
            if let Some(s) = a.get_short() {
                flags.push(format!("-{s}"));
            }
            flags
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    // Pre-scan argv for compound metadata operations (+ separator)
    // before clap parsing, since clap would choke on bare + tokens.
    let raw_args: Vec<String> = std::env::args().collect();
    let value_flags = value_taking_global_flags();
    let compound_continuations =
        commands::metadata::extract_compound_from_argv(&raw_args, &value_flags)?;
    let cli = if let Some(ref split) = compound_continuations {
        Cli::try_parse_from(&split.filtered_argv)?
    } else {
        Cli::parse()
    };

    // Handle commands that don't need IA config/client
    match cli.command {
        Commands::Completions(args) => {
            let mut cmd = Cli::command();
            return commands::completions::run(args, &mut cmd);
        }
        #[cfg(feature = "self-update")]
        Commands::Update(args) => {
            return commands::update::run(args).await;
        }
        Commands::Config(args) => {
            // Config command handles its own config/client creation
            // because some subcommands (login) don't require existing credentials
            let config = if let Some(path) = &cli.config {
                ia_core::IaConfig::load_from_file(path)?
            } else {
                ia_core::IaConfig::load()?
            };
            return commands::config::run(args, config, cli.config.clone()).await;
        }
        _ => {}
    }

    // Initialize logging
    // Suppress tracing output during dashboard mode to prevent stderr writes
    // from interfering with the raw-mode TUI (causes diagonal scrolling text).
    let dashboard_active = matches!(&cli.command, Commands::Download(ref args) if args.dashboard);
    let log_level = if dashboard_active {
        "off"
    } else if cli.debug {
        "debug"
    } else if cli.log {
        "info"
    } else {
        "warn"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_writer(std::io::stderr)
        .init();

    // Load config
    let mut config = if let Some(path) = &cli.config {
        ia_core::IaConfig::load_from_file(path)?
    } else {
        ia_core::IaConfig::load()?
    };

    // CLI flag overrides
    if cli.insecure {
        config.general.secure = false;
    }
    if let Some(host) = &cli.host {
        config.general.host = host.clone();
    }
    if let Some(suffix) = &cli.user_agent_suffix {
        config.general.user_agent_suffix = Some(suffix.clone());
    }

    let client = ia_core::IaClient::from_config(config)?;

    match cli.command {
        Commands::Ai(args) => {
            commands::ai::run(&client, args, cli.quiet, cli.jobs, cli.joblog).await?
        }
        Commands::Download(args) => {
            commands::download::run(&client, args, cli.quiet, cli.jobs, cli.joblog, cli.retry_failed).await?
        }
        Commands::List(args) => commands::list::run(&client, args, cli.quiet).await?,
        Commands::Metadata(args) => {
            let conts = compound_continuations.map(|c| c.continuations);
            commands::metadata::run(&client, args, conts, cli.quiet, cli.jobs, cli.joblog.clone())
                .await?
        }
        Commands::Search(args) => commands::search::run(&client, args, cli.quiet).await?,
        Commands::Status(args) => commands::status::run(args).await?,
        Commands::Upload(args) => {
            commands::upload::run(&client, args, cli.quiet, cli.jobs, cli.joblog).await?
        }
        Commands::Completions(_) => unreachable!("handled above"),
        Commands::Config(_) => unreachable!("handled above"),
        #[cfg(feature = "self-update")]
        Commands::Update(_) => unreachable!("handled above"),
    }

    Ok(())
}
