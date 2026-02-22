use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod output;
#[cfg(feature = "tui")]
mod tui;

#[derive(Parser)]
#[command(name = "ia", version, about = "Internet Archive command-line tool")]
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
    #[arg(short = 'j', long, global = true, default_value = "5")]
    jobs: usize,

    /// Suppress output (repeat for more quiet: -q summary only, -qq silent)
    #[arg(short = 'q', long, global = true, action = clap::ArgAction::Count)]
    quiet: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download files from an item
    Download(commands::download::DownloadArgs),
    /// List files in an item
    #[command(alias = "ls")]
    List(commands::list::ListArgs),
    /// Display item metadata
    Metadata(commands::metadata::MetadataArgs),
    /// Search the Internet Archive
    Search(commands::search::SearchArgs),
    /// Show job log summary
    Status(commands::status::StatusArgs),
    /// Generate shell completions
    Completions(commands::completions::CompletionsArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle completions early (no client/config needed)
    if let Commands::Completions(args) = cli.command {
        let mut cmd = Cli::command();
        return commands::completions::run(args, &mut cmd);
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
        Commands::Download(args) => {
            commands::download::run(&client, args, cli.quiet, cli.jobs, cli.joblog, cli.retry_failed).await?
        }
        Commands::List(args) => commands::list::run(&client, args, cli.quiet).await?,
        Commands::Metadata(args) => commands::metadata::run(&client, args).await?,
        Commands::Search(args) => commands::search::run(&client, args, cli.quiet).await?,
        Commands::Status(args) => commands::status::run(args).await?,
        Commands::Completions(_) => unreachable!("handled above"),
    }

    Ok(())
}
