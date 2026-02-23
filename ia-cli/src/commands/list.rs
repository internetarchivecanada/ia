use anyhow::{Context, Result};
use clap::Args;
use color_print::cstr;
use comfy_table::{Cell, Color, Table};
use console::style;

use ia_core::files::{self, FileFilter};
use ia_core::types::FileSource;
use ia_core::IaClient;

#[derive(Args)]
#[command(
    long_about = "List files in an Internet Archive item. Displays a table of files with name, \
        size, and format by default. Use --columns to customize output, --glob to filter, or \
        --all for full file metadata as JSON.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># List files in an item</dim>\n  <bold>$ ia list nasa</bold>\
         \n\n  <dim># Show only original files with download URLs</dim>\n  <bold>$ ia list nasa --source original --location</bold>\n"
    ),
)]
pub struct ListArgs {
    /// Item identifier
    pub identifier: String,

    /// Columns to show (comma-separated: name,size,format,source,md5,mtime)
    #[arg(long)]
    pub columns: Option<String>,

    /// Filter files by glob pattern
    #[arg(short = 'g', long)]
    pub glob: Option<String>,

    /// Print full download URLs
    #[arg(long)]
    pub location: bool,

    /// Show all file metadata as JSON
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Print column headers
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Filter by source type (original, derivative, metadata)
    #[arg(long, value_parser = parse_source)]
    pub source: Option<FileSource>,
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!("unknown source: {s}")),
    }
}

pub async fn run(client: &IaClient, args: ListArgs, quiet: u8) -> Result<()> {
    let item = client
        .get_item(&args.identifier)
        .await
        .context(format!("failed to fetch metadata for {}", args.identifier))?;

    let filter = FileFilter {
        glob: args.glob,
        source: args.source,
        ..Default::default()
    };

    let file_list = files::list(&item, &filter);

    if file_list.is_empty() {
        if quiet < 2 {
            eprintln!("no files match filters");
        }
        return Ok(());
    }

    // --all: print full JSON for each file
    if args.all {
        for f in &file_list {
            println!("{}", serde_json::to_string_pretty(f)?);
        }
        return Ok(());
    }

    // --location: print download URLs
    if args.location {
        for f in &file_list {
            let encoded = urlencoding::encode(&f.name);
            println!(
                "{}/download/{}/{}",
                client.url(""),
                args.identifier,
                encoded
            );
        }
        return Ok(());
    }

    // Determine which columns to show
    let default_cols = vec!["name", "size", "format", "source"];
    let columns: Vec<&str> = if let Some(ref cols) = args.columns {
        cols.split(',').map(|c| c.trim()).collect()
    } else {
        default_cols
    };

    // Table output
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);

    if args.verbose {
        let headers: Vec<Cell> = columns
            .iter()
            .map(|c| Cell::new(c.to_uppercase()).fg(Color::Cyan))
            .collect();
        table.set_header(headers);
    }

    for f in &file_list {
        let row: Vec<String> = columns
            .iter()
            .map(|col| match *col {
                "name" => f.name.clone(),
                "size" => f.size.map(format_size).unwrap_or_default(),
                "format" => f.format.clone().unwrap_or_default(),
                "source" => f.source.clone().unwrap_or_default(),
                "md5" => f.md5.clone().unwrap_or_default(),
                "mtime" => f
                    .mtime
                    .map(|t| {
                        chrono::DateTime::from_timestamp(t as i64, 0)
                            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| t.to_string())
                    })
                    .unwrap_or_default(),
                "sha1" => f.sha1.clone().unwrap_or_default(),
                other => f
                    .extra
                    .get(other)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            })
            .collect();
        table.add_row(row);
    }

    println!("{table}");

    // Summary
    if quiet == 0 {
        let total = files::total_size(&file_list);
        eprintln!(
            "\n{}  {} files ({})",
            style(&args.identifier).bold(),
            file_list.len(),
            format_size(total),
        );
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
