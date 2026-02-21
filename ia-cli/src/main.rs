use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod output;

#[derive(Parser)]
#[command(name = "ia", version, about = "Internet Archive command-line tool")]
struct Cli {
    /// Path to config file
    #[arg(short = 'c', long, global = true)]
    config: Option<PathBuf>,

    /// Enable logging
    #[arg(short = 'l', long, global = true)]
    log: bool,

    /// Enable debug output
    #[arg(short = 'd', long, global = true)]
    debug: bool,

    /// Path to job log file
    #[arg(long, global = true)]
    joblog: Option<PathBuf>,

    /// Retry failed operations from job log
    #[arg(long, global = true)]
    retry_failed: bool,

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = if cli.debug {
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
    let config = if let Some(path) = &cli.config {
        ia_core::IaConfig::load_from_file(path)?
    } else {
        ia_core::IaConfig::load()?
    };

    let client = ia_core::IaClient::from_config(config)?;

    match cli.command {
        Commands::Download(args) => {
            commands::download::run(&client, args, cli.quiet, cli.joblog, cli.retry_failed).await?
        }
        Commands::List(args) => commands::list::run(&client, args, cli.quiet).await?,
        Commands::Metadata(args) => commands::metadata::run(&client, args).await?,
        Commands::Search(args) => commands::search::run(&client, args, cli.quiet).await?,
    }

    Ok(())
}
