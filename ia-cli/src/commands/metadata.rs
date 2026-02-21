use anyhow::{Context, Result};
use clap::Args;

use ia_core::IaClient;

#[derive(Args)]
pub struct MetadataArgs {
    /// Item identifier
    pub identifier: String,

    /// Check if item exists (exit 0 if yes, 1 if no)
    #[arg(short = 'e', long)]
    pub exists: bool,

    /// List file formats in the item
    #[arg(short = 'F', long)]
    pub formats: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,
}

pub async fn run(client: &IaClient, args: MetadataArgs) -> Result<()> {
    // --exists: just check existence and set exit code
    if args.exists {
        let exists = client
            .item_exists(&args.identifier)
            .await
            .context(format!("failed to check existence of {}", args.identifier))?;
        if !exists {
            std::process::exit(1);
        }
        return Ok(());
    }

    let item = client
        .get_item(&args.identifier)
        .await
        .context(format!("failed to fetch metadata for {}", args.identifier))?;

    // --formats: list unique formats
    if args.formats {
        let mut formats: Vec<String> = item
            .files
            .iter()
            .filter_map(|f| f.format.clone())
            .collect();
        formats.sort();
        formats.dedup();
        for fmt in formats {
            println!("{fmt}");
        }
        return Ok(());
    }

    // Default: compact JSON (one line per item), --pretty for indented output
    let json = if args.pretty {
        serde_json::to_string_pretty(&item)?
    } else {
        serde_json::to_string(&item)?
    };
    println!("{json}");

    Ok(())
}
