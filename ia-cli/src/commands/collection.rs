use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

use ia_core::collection::{create_collection, CreateCollectionResult};
use ia_core::IaClient;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    about = "Manage Internet Archive collections",
    long_about = "Create and manage Internet Archive collections. Collections are items with \
        mediatype=collection that group related items together."
)]
pub struct CollectionArgs {
    #[command(subcommand)]
    pub command: CollectionCommand,
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    /// Create a new collection
    #[command(
        long_about = "Create a new Internet Archive collection via S3. Requires title, \
            description, subject, and parent collection. Optionally upload a collection image.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Create a simple collection</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource</bold>\
             \n\n  <dim># Create with an image</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    --image logo.png</bold>\
             \n\n  <dim># Create with extra metadata</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"Desc\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    -m hidden:true -m num-top-dl:5</bold>\n"
        ),
    )]
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Collection identifier
    pub identifier: String,

    /// Collection title
    #[arg(short = 't', long)]
    pub title: String,

    /// Collection description
    #[arg(long)]
    pub description: String,

    /// Subject/topic
    #[arg(short = 's', long)]
    pub subject: String,

    /// Parent collection identifier
    #[arg(short = 'C', long)]
    pub collection: String,

    /// Path to collection image file
    #[arg(short = 'I', long)]
    pub image: Option<PathBuf>,

    /// Additional metadata (repeatable, KEY:VALUE)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Enable derive (default: derive is off for collections)
    #[arg(long)]
    pub derive: bool,

    /// Validate everything without sending the request
    #[arg(long)]
    pub dry_run: bool,

    /// Output JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Run ─────────────────────────────────────────────────────────────────────

pub async fn run(client: &IaClient, args: CollectionArgs, quiet: u8) -> Result<()> {
    match args.command {
        CollectionCommand::Create(create_args) => run_create(client, create_args, quiet).await,
    }
}

async fn run_create(client: &IaClient, args: CreateArgs, quiet: u8) -> Result<()> {
    // Build metadata list from required flags + extra -m pairs
    let mut metadata: Vec<(String, String)> = vec![
        ("title".into(), args.title),
        ("description".into(), args.description),
        ("subject".into(), args.subject),
        ("collection".into(), args.collection),
    ];

    // Parse and append extra metadata
    for m in &args.metadata {
        let (key, value) = m
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid KEY:VALUE format: '{m}'"))?;
        metadata.push((key.to_string(), value.to_string()));
    }

    let image_path = args.image.as_deref();

    let result = create_collection(
        client,
        &args.identifier,
        &metadata,
        image_path,
        args.derive,
        args.dry_run,
    )
    .await;

    match result {
        Ok(ref r) => print_success(r, args.json, args.dry_run, quiet),
        Err(ref e) => {
            if args.json {
                let json = serde_json::json!({
                    "error": true,
                    "identifier": args.identifier,
                    "message": e.to_string(),
                });
                eprintln!("{}", serde_json::to_string(&json)?);
            }
        }
    }

    result.map(|_| ()).map_err(Into::into)
}

fn print_success(result: &CreateCollectionResult, json: bool, dry_run: bool, quiet: u8) {
    if json {
        let json = serde_json::json!({
            "identifier": result.identifier,
            "status": result.status,
            "url": result.url,
        });
        println!("{}", serde_json::to_string(&json).unwrap());
    } else if quiet == 0 {
        let prefix = if dry_run {
            style("dry-run:").yellow().bold()
        } else {
            style("created:").green().bold()
        };
        println!("{prefix} {}", result.url);
    }
}
