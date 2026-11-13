use std::pin::Pin;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;
use futures::StreamExt;

use ia_core::search::{self, SearchOpts, SearchResult, DEFAULT_ADVANCED_ROWS};
use ia_core::IaClient;

/// Backend selector for the `--num-found` count query.
enum NumFoundBackend {
    Scrape,
    Advanced,
    Fts { dsl: bool },
}

/// Handle `--num-found` for any search backend: print the total count and exit.
async fn handle_num_found(
    client: &IaClient,
    query: &str,
    backend: NumFoundBackend,
    params: &[(String, String)],
    timeout: Option<u64>,
    json: bool,
) -> Result<()> {
    let timeout = timeout.map(std::time::Duration::from_secs);
    let count = match backend {
        NumFoundBackend::Scrape => {
            search::num_found_with_timeout(client, query, params, timeout).await?
        }
        NumFoundBackend::Advanced => {
            search::advanced_num_found_with_timeout(client, query, params, timeout).await?
        }
        NumFoundBackend::Fts { dsl } => {
            search::fts_num_found_with_timeout(client, query, dsl, params, timeout).await?
        }
    };
    if json {
        println!("{}", serde_json::json!({"num_found": count}));
    } else {
        println!("{count}");
    }
    Ok(())
}

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

    /// Extra parameters (key:value or key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Give up on a request after this many seconds, if that is sooner than the
    /// client's own limits (30 s to connect, 60 s without response data)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum SearchCommand {
    /// Search via scrape API (cursor-based, auto-paginates)
    #[command(
        long_about = "Search the Internet Archive using the scrape API. Uses cursor-based \
            pagination and automatically streams all matching results. This is the default \
            backend when no subcommand is specified. A 429 (rate limited) on any request is retried up to three times, waiting \
            what the server's Retry-After header says (or a random wait up to a cap that doubles \
            from 1 s to 60 s), then the page fails with a rate-limit error.",
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
            single page of results. Use --rows to control page size (default 50) and \
            -p page=N to select a page. A 429 (rate limited) on any request is retried up to three times, waiting \
            what the server's Retry-After header says (or a random wait up to a cap that doubles \
            from 1 s to 60 s), then the page fails with a rate-limit error.",
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
            inside file contents, not just item metadata. Uses scroll-based pagination, except \
            with --from, which fetches one page. Each result is one matching file or page, so an \
            item can appear more than once; the identifier printed is the item's. An error the \
            API reports is printed and the command fails. A 429 (rate limited) on any request is retried up to three times, waiting \
            what the server's Retry-After header says (or a random wait up to a cap that doubles \
            from 1 s to 60 s), then the page fails with a rate-limit error.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Full-text search</dim>\n  <bold>$ ia search fts \"apollo 11 landing\"</bold>\
             \n\n  <dim># Identifiers only</dim>\n  <bold>$ ia search fts \"apollo 11 landing\" --itemlist</bold>\
             \n\n  <dim># Results 21 to 30, one request</dim>\n  <bold>$ ia search fts \"apollo 11 landing\" --from 20 --size 10</bold>\n"
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

    /// Results per page
    #[arg(short = 'r', long, default_value_t = DEFAULT_ADVANCED_ROWS)]
    pub rows: usize,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Debug, Args)]
pub struct FtsArgs {
    /// Search query
    pub query: String,

    /// Send the query as is, without the `!L` prefix (for Elasticsearch DSL)
    #[arg(long)]
    pub dsl: bool,

    /// Value for the API's `scope` parameter (also sent with --num-found)
    #[arg(long)]
    pub scope: Option<String>,

    /// Results per request (default: 1000)
    #[arg(long)]
    pub size: Option<usize>,

    /// Start at this offset and fetch one page of --size results, without scrolling
    #[arg(long)]
    pub from: Option<usize>,

    #[command(flatten)]
    pub shared: SharedSearchArgs,
}

#[derive(Args)]
#[command(
    about = "Search the Internet Archive",
    long_about = "Search the Internet Archive. Uses the scrape API by default. \
        Choose a subcommand for a different backend; the query and options then go \
        after the subcommand name (ia search scrape QUERY --json).",
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

    /// Extra parameters (key:value or key=value, repeatable)
    #[arg(short = 'p', long)]
    pub parameters: Vec<String>,

    /// Give up on a request after this many seconds, if that is sooner than the
    /// client's own limits (30 s to connect, 60 s without response data)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Option<SearchCommand>,
}

/// Refuse bare-form options and a query given before a subcommand name
/// (`ia search --timeout 5 scrape QUERY`): they belong to the bare form, and
/// the subcommand reads only its own, so they would otherwise be dropped.
///
/// clap's `args_conflicts_with_subcommands` cannot do this here: with it, the
/// optional bare-form query takes the subcommand name as its value.
fn refuse_options_before_subcommand(args: &SearchArgs) -> Result<()> {
    let Some(command) = &args.command else {
        return Ok(());
    };
    // Destructured in full so that a new bare-form field must be listed here.
    let SearchArgs {
        query,
        sort,
        field,
        itemlist,
        num_found,
        json,
        parameters,
        timeout,
        command: _,
    } = args;
    let mut given: Vec<String> = Vec::new();
    if let Some(q) = query {
        given.push(format!("'{q}'"));
    }
    for (name, present) in [
        ("--sort", !sort.is_empty()),
        ("--field", !field.is_empty()),
        ("--itemlist", *itemlist),
        ("--num-found", *num_found),
        ("--json", *json),
        ("--parameters", !parameters.is_empty()),
        ("--timeout", timeout.is_some()),
    ] {
        if present {
            given.push(name.to_string());
        }
    }
    if given.is_empty() {
        return Ok(());
    }
    let sub = match command {
        SearchCommand::Scrape(_) => "scrape",
        SearchCommand::Advanced(_) => "advanced",
        SearchCommand::Fts(_) => "fts",
    };
    bail!(
        "{} given before the subcommand '{sub}'; the query and options go after it: \
         ia search {sub} <QUERY> [OPTIONS]",
        given.join(", ")
    );
}

pub async fn run(client: &IaClient, args: SearchArgs, quiet: u8) -> Result<()> {
    refuse_options_before_subcommand(&args)?;
    match args.command {
        Some(SearchCommand::Scrape(sub)) => {
            run_scrape(client, sub.query, sub.sort, sub.field, sub.shared, quiet).await
        }
        Some(SearchCommand::Advanced(sub)) => {
            run_advanced(
                client, sub.query, sub.sort, sub.field, sub.rows, sub.shared, quiet,
            )
            .await
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
    let extra_params = parse_extra_params(&shared.parameters)?;

    if shared.num_found {
        return handle_num_found(
            client,
            &query,
            NumFoundBackend::Scrape,
            &extra_params,
            shared.timeout,
            shared.json,
        )
        .await;
    }

    let opts = build_search_opts(&field, &sort, &shared)?;
    let stream = ia_core::search::scrape(client, &query, &opts);
    run_output(stream, &shared, &field, &query, quiet).await
}

async fn run_advanced(
    client: &IaClient,
    query: String,
    sort: Vec<String>,
    field: Vec<String>,
    rows: usize,
    shared: SharedSearchArgs,
    quiet: u8,
) -> Result<()> {
    let extra_params = parse_extra_params(&shared.parameters)?;

    if shared.num_found {
        return handle_num_found(
            client,
            &query,
            NumFoundBackend::Advanced,
            &extra_params,
            shared.timeout,
            shared.json,
        )
        .await;
    }

    let mut opts = build_search_opts(&field, &sort, &shared)?;
    // If --parameters provides "rows", use that value for page size and result
    // count so `--parameters 'rows=3'` behaves the same as `--rows 3`.
    let effective_rows = opts
        .params
        .iter()
        .find(|(k, _)| k == "rows")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(rows);
    opts.rows = effective_rows;
    // Single page only: limit result count to one page worth of rows.
    if opts.count == 0 {
        opts.count = effective_rows;
    }
    let stream = ia_core::search::advanced(client, &query, &opts);
    run_output(stream, &shared, &field, &query, quiet).await
}

async fn run_fts(client: &IaClient, args: FtsArgs, quiet: u8) -> Result<()> {
    let mut params = parse_extra_params(&args.shared.parameters)?;
    // `--scope` narrows the count as well as the results.
    if let Some(ref scope) = args.scope {
        params.push(("scope".to_string(), scope.clone()));
    }

    if args.shared.num_found {
        return handle_num_found(
            client,
            &args.query,
            NumFoundBackend::Fts { dsl: args.dsl },
            &params,
            args.shared.timeout,
            args.shared.json,
        )
        .await;
    }

    // ia-core sends these as JSON numbers; `--from` turns scroll off.
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
) -> Result<SearchOpts> {
    // In non-JSON mode with no explicit fields, request only identifiers
    let fields = if !field.is_empty() {
        field.to_vec()
    } else if !shared.json {
        vec!["identifier".to_string()]
    } else {
        vec![]
    };

    let extra_params = parse_extra_params(&shared.parameters)?;

    Ok(SearchOpts {
        fields,
        sorts: sort.to_vec(),
        count: 0,
        rows: 0,
        timeout: shared.timeout,
        params: extra_params,
        dsl: false,
    })
}

pub(crate) fn parse_extra_params(parameters: &[String]) -> Result<Vec<(String, String)>> {
    parameters
        .iter()
        .map(|p| {
            let (k, v) = p
                .split_once('=')
                .or_else(|| p.split_once(':'))
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid parameter (expected KEY=VALUE or KEY:VALUE): {p}")
                })?;
            Ok((k.to_string(), v.to_string()))
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
            let mut obj = if fields_requested.is_empty() {
                item.fields.clone()
            } else {
                item.fields
                    .iter()
                    .filter(|(k, _)| fields_requested.iter().any(|f| f.as_str() == k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            obj.insert(
                "identifier".to_string(),
                serde_json::Value::String(item.identifier),
            );
            println!("{}", serde_json::to_string(&obj)?);
        } else if shared.itemlist {
            println!("{}", item.identifier);
        } else if quiet == 0 && !item.fields.is_empty() {
            print!("{}", style(&item.identifier).bold());
            for (key, value) in &item.fields {
                if !fields_requested.is_empty() && !fields_requested.iter().any(|f| f == key) {
                    continue;
                }
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
        eprintln!("\n{}  {} results", style("search").bold(), count,);
    }

    if count == 0 {
        bail!("no results found for query: {query}");
    }

    Ok(())
}
