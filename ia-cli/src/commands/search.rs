use std::pin::Pin;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::StreamExt;

use ia_core::search::{SearchOpts, SearchResult};
use ia_core::IaClient;

/// Shared output/control options available on all search subcommands.
#[derive(Debug, Args)]
pub struct SharedSearchArgs {
    /// Output identifiers only (one per line)
    #[arg(long)]
    pub itemlist: bool,

    /// Print result count only
    #[arg(short = 'n', long)]
    pub num_found: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// Extra parameters (key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum SearchCommand {
    /// Search via scrape API (cursor-based, auto-paginates)
    #[command(
        long_about = "Search the Internet Archive using the scrape API. Uses cursor-based \
            pagination and automatically streams all matching results. This is the default \
            backend when no subcommand is specified.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Search a collection</dim>\n  <bold>$ ia search scrape \"collection:nasa\"</bold>\
             \n\n  <dim># Sort by downloads</dim>\n  <bold>$ ia search scrape \"mediatype:audio\" --sort \"downloads desc\"</bold>\
             \n\n  <dim># Get identifiers only</dim>\n  <bold>$ ia search scrape \"date:1969\" --itemlist</bold>\n"
        ),
    )]
    Scrape(ScrapeArgs),

    /// Search via advanced search API (page-based, single page)
    #[command(
        long_about = "Search the Internet Archive using the advanced search API. Returns a \
            single page of results. Use -p rows=N to control page size and -p page=N to \
            select a page.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Single page of results</dim>\n  <bold>$ ia search advanced \"collection:nasa\"</bold>\
             \n\n  <dim># With sorting and field selection</dim>\n  <bold>$ ia search advanced \"mediatype:texts\" --sort \"date desc\" --field title --field date</bold>\n"
        ),
    )]
    Advanced(AdvancedArgs),

    /// Full-text search (scroll-based, auto-paginates)
    #[command(
        long_about = "Search the Internet Archive using the full-text search backend. Searches \
            inside file contents, not just item metadata. Uses scroll-based pagination.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Full-text search</dim>\n  <bold>$ ia search fts \"apollo 11 landing\"</bold>\
             \n\n  <dim># With Elasticsearch DSL</dim>\n  <bold>$ ia search fts --dsl '{\"match\": {\"text\": \"moon landing\"}}'</bold>\
             \n\n  <dim># Scoped to an index</dim>\n  <bold>$ ia search fts \"nasa\" --scope nasa_index</bold>\n"
        ),
    )]
    Fts(FtsArgs),
}

#[derive(Debug, Args)]
pub struct ScrapeArgs {
    /// Search query
    pub query: String,

    /// Sort fields (repeatable, e.g., "downloads desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Debug, Args)]
pub struct AdvancedArgs {
    /// Search query
    pub query: String,

    /// Sort fields (repeatable, e.g., "date desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Debug, Args)]
pub struct FtsArgs {
    /// Search query
    pub query: String,

    /// Use raw Elasticsearch DSL (skip !L prefix)
    #[arg(long)]
    pub dsl: bool,

    /// Index/scope filter
    #[arg(long)]
    pub scope: Option<String>,

    /// Results per scroll batch (default: 1000)
    #[arg(long)]
    pub size: Option<usize>,

    /// Starting offset
    #[arg(long)]
    pub from: Option<usize>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Args)]
#[command(
    long_about = "Search the Internet Archive. Uses the scrape API by default. \
        Choose a subcommand for a different backend.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Search (uses scrape by default)</dim>\n  <bold>$ ia search \"collection:nasa\"</bold>\
         \n\n  <dim># Explicit backend</dim>\n  <bold>$ ia search scrape \"collection:nasa\"</bold>\
         \n\n  <dim># Advanced search (single page)</dim>\n  <bold>$ ia search advanced \"mediatype:texts\"</bold>\
         \n\n  <dim># Full-text search</dim>\n  <bold>$ ia search fts \"apollo 11\"</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct SearchArgs {
    /// Search query (uses scrape API by default)
    #[arg()]
    pub query: Option<String>,

    // Bare-mode options (when no subcommand specified, defaults to scrape)
    /// Sort fields (repeatable, e.g., "downloads desc")
    #[arg(short = 's', long)]
    pub sort: Vec<String>,

    /// Fields to return (repeatable, default: all)
    #[arg(short = 'f', long, visible_alias = "fields")]
    pub field: Vec<String>,

    /// Output identifiers only (one per line)
    #[arg(long)]
    pub itemlist: bool,

    /// Print result count only
    #[arg(short = 'n', long)]
    pub num_found: bool,

    /// Output results as JSON (one object per line)
    #[arg(long)]
    pub json: bool,

    /// Extra parameters (key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Option<SearchCommand>,
}

pub async fn run(client: &IaClient, args: SearchArgs, quiet: u8) -> Result<()> {
    match args.command {
        Some(SearchCommand::Scrape(sub)) => {
            run_scrape(client, sub.query, sub.sort, sub.field, sub.shared, quiet).await
        }
        Some(SearchCommand::Advanced(sub)) => {
            run_advanced(client, sub.query, sub.sort, sub.field, sub.shared, quiet).await
        }
        Some(SearchCommand::Fts(sub)) => run_fts(client, sub, quiet).await,
        None => {
            // Bare mode: default to scrape
            let query = args.query.ok_or_else(|| {
                anyhow::anyhow!("search query required. Run 'ia search --help' for usage.")
            })?;
            let shared = SharedSearchArgs {
                itemlist: args.itemlist,
                num_found: args.num_found,
                json: args.json,
                parameters: args.parameters,
                timeout: args.timeout,
            };
            run_scrape(client, query, args.sort, args.field, shared, quiet).await
        }
    }
}

async fn run_scrape(
    client: &IaClient,
    query: String,
    sort: Vec<String>,
    field: Vec<String>,
    shared: SharedSearchArgs,
    quiet: u8,
) -> Result<()> {
    // --num-found: just print count and exit
    if shared.num_found {
        let count = ia_core::search::num_found(client, &query).await?;
        if shared.json {
            println!("{}", serde_json::json!({"num_found": count}));
        } else {
            println!("{count}");
        }
        return Ok(());
    }

    let opts = build_search_opts(&field, &sort, &shared);
    let stream = ia_core::search::scrape(client, &query, &opts);
    run_output(stream, &shared, &field, &query, quiet).await
}

async fn run_advanced(
    client: &IaClient,
    query: String,
    sort: Vec<String>,
    field: Vec<String>,
    shared: SharedSearchArgs,
    quiet: u8,
) -> Result<()> {
    if shared.num_found {
        let count = ia_core::search::num_found(client, &query).await?;
        if shared.json {
            println!("{}", serde_json::json!({"num_found": count}));
        } else {
            println!("{count}");
        }
        return Ok(());
    }

    let mut opts = build_search_opts(&field, &sort, &shared);
    // Single page only: set count to page size (default 500 via rows param)
    // The advanced backend auto-paginates in ia-core, so we limit to one page
    // by setting count to the page size.
    if opts.count == 0 {
        // Default to one page of 500 results
        opts.count = 500;
    }
    let stream = ia_core::search::advanced(client, &query, &opts);
    run_output(stream, &shared, &field, &query, quiet).await
}

async fn run_fts(client: &IaClient, args: FtsArgs, quiet: u8) -> Result<()> {
    if args.shared.num_found {
        // FTS doesn't have a dedicated count endpoint; just run the search
        // and count results. For now, error out.
        bail!("--num-found is not supported for full-text search");
    }

    let extra_params = parse_extra_params(&args.shared.parameters);

    // Add FTS-specific params
    let mut params = extra_params;
    if let Some(ref scope) = args.scope {
        params.push(("scope".to_string(), scope.clone()));
    }
    if let Some(size) = args.size {
        params.push(("size".to_string(), size.to_string()));
    }
    if let Some(from) = args.from {
        params.push(("from".to_string(), from.to_string()));
    }

    let opts = SearchOpts {
        fields: vec![],
        sorts: vec![],
        count: 0,
        rows: 0,
        timeout: args.shared.timeout,
        params,
        dsl: args.dsl,
    };

    let stream = ia_core::search::fts(client, &args.query, &opts);
    run_output(stream, &args.shared, &[], &args.query, quiet).await
}

fn build_search_opts(
    field: &[String],
    sort: &[String],
    shared: &SharedSearchArgs,
) -> SearchOpts {
    // In non-JSON mode with no explicit fields, request only identifiers
    let fields = if !field.is_empty() {
        field.to_vec()
    } else if !shared.json {
        vec!["identifier".to_string()]
    } else {
        vec![]
    };

    let extra_params = parse_extra_params(&shared.parameters);

    SearchOpts {
        fields,
        sorts: sort.to_vec(),
        count: 0,
        rows: 0,
        timeout: shared.timeout,
        params: extra_params,
        dsl: false,
    }
}

fn parse_extra_params(parameters: &[String]) -> Vec<(String, String)> {
    parameters
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

async fn run_output(
    mut stream: Pin<Box<dyn futures::Stream<Item = ia_core::Result<SearchResult>> + Send + '_>>,
    shared: &SharedSearchArgs,
    fields_requested: &[String],
    query: &str,
    quiet: u8,
) -> Result<()> {
    let mut count = 0u64;

    while let Some(result) = stream.next().await {
        let item = result?;
        count += 1;

        if shared.json {
            let mut obj = item.fields.clone();
            obj.insert(
                "identifier".to_string(),
                serde_json::Value::String(item.identifier),
            );
            println!("{}", serde_json::to_string(&obj).unwrap_or_default());
        } else if shared.itemlist {
            println!("{}", item.identifier);
        } else if quiet == 0 && !item.fields.is_empty() {
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

    if quiet < 2 && !shared.itemlist && !shared.json {
        eprintln!(
            "\n{}  {} results",
            style("search").bold(),
            count,
        );
    }

    if count == 0 {
        bail!("no results found for query: {query}");
    }

    // Suppress unused variable warning
    let _ = fields_requested;

    Ok(())
}
