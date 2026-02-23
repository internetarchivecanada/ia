use anyhow::{bail, Result};
use clap::Args;
use console::style;
use futures::StreamExt;

use ia_core::search::{SearchOpts, SearchResult};
use ia_core::IaClient;

#[derive(Args)]
pub struct SearchArgs {
    /// Search query
    pub query: String,

    /// Output identifiers only (one per line)
    #[arg(long)]
    pub itemlist: bool,

    /// Print count only
    #[arg(long)]
    pub num_found: bool,

    /// Sort fields (repeatable, e.g., "downloads desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// Use full-text search backend
    #[arg(long)]
    pub fts: bool,

    /// Extra parameters (key=value)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Maximum number of results
    #[arg(short = 'n', long)]
    pub count: Option<usize>,
}

pub async fn run(client: &IaClient, args: SearchArgs, quiet: u8) -> Result<()> {
    // --num-found: just print count and exit
    if args.num_found {
        let count = ia_core::search::num_found(client, &args.query).await?;
        println!("{count}");
        return Ok(());
    }

    let extra_params: Vec<(String, String)> = args
        .parameters
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    // In non-JSON mode with no explicit fields, request only identifiers
    // to keep output lightweight. In JSON mode, let the core default (*)
    // flow through so full docs are returned.
    let fields = if !args.field.is_empty() {
        args.field.clone()
    } else if !args.json {
        vec!["identifier".to_string()]
    } else {
        vec![]
    };

    let opts = SearchOpts {
        fields,
        sorts: args.sort.clone(),
        count: args.count.unwrap_or(0),
        timeout: args.timeout,
        params: extra_params,
    };

    let mut stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = ia_core::Result<SearchResult>> + Send + '_>,
    > = if args.fts {
        ia_core::search::fts(client, &args.query, &opts)
    } else {
        ia_core::search::scrape(client, &args.query, &opts)
    };

    let mut count = 0u64;

    while let Some(result) = stream.next().await {
        let item = result?;
        count += 1;

        if args.json {
            // JSON output: full doc as a single line
            let mut obj = item.fields.clone();
            obj.insert(
                "identifier".to_string(),
                serde_json::Value::String(item.identifier),
            );
            println!("{}", serde_json::to_string(&obj).unwrap_or_default());
        } else if args.itemlist {
            // Identifier-only output (for piping)
            println!("{}", item.identifier);
        } else if quiet == 0 && !item.fields.is_empty() {
            // Pretty output with fields
            print!("{}", style(&item.identifier).bold());
            for (key, value) in &item.fields {
                let display = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                print!("  {}={}", style(key).dim(), display);
            }
            println!();
        } else {
            println!("{}", item.identifier);
        }
    }

    if quiet < 2 && !args.itemlist && !args.json {
        eprintln!(
            "\n{}  {} results",
            style("search").bold(),
            count,
        );
    }

    if count == 0 {
        bail!("no results found for query: {}", args.query);
    }

    Ok(())
}
