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
        long_about = "Create a new Internet Archive collection via S3. Only the identifier and \
            parent collection are required. Title, description, and subject are recommended but \
            optional.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Minimal collection (identifier + parent only)</dim>\
             \n  <bold>$ ia collection create my-collection --collection opensource</bold>\
             \n\n  <dim># Recommended: include title, description, subject</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    -D \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource</bold>\
             \n\n  <dim># With an image and extra metadata</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    -D \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    --image logo.png -m hidden:true</bold>\n"
        ),
    )]
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Collection identifier
    pub identifier: String,

    /// Parent collection identifier
    #[arg(short = 'C', long)]
    pub collection: String,

    /// Collection title
    #[arg(short = 't', long)]
    pub title: Option<String>,

    /// Collection description
    #[arg(short = 'D', long)]
    pub description: Option<String>,

    /// Subject/topic
    #[arg(short = 's', long)]
    pub subject: Option<String>,

    /// Path to collection image file
    #[arg(short = 'I', long)]
    pub image: Option<PathBuf>,

    /// Additional metadata (repeatable, KEY:VALUE)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

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
    // Build metadata: collection is required, others are optional
    let mut metadata: Vec<(String, String)> = vec![("collection".into(), args.collection.clone())];

    if let Some(ref title) = args.title {
        metadata.push(("title".into(), title.clone()));
    }
    if let Some(ref description) = args.description {
        metadata.push(("description".into(), description.clone()));
    }
    if let Some(ref subject) = args.subject {
        metadata.push(("subject".into(), subject.clone()));
    }

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
        args.dry_run,
    )
    .await;

    match result {
        Ok(ref r) => print_success(r, &metadata, &args, quiet)?,
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

fn print_success(
    result: &CreateCollectionResult,
    metadata: &[(String, String)],
    args: &CreateArgs,
    quiet: u8,
) -> Result<()> {
    if args.json {
        let mut json = serde_json::json!({
            "identifier": result.identifier,
            "status": result.status,
            "url": result.url,
        });
        if args.dry_run {
            // Include metadata in JSON dry-run output
            let meta_obj: serde_json::Map<String, serde_json::Value> = metadata
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            json["mediatype"] = serde_json::Value::String("collection".into());
            json["metadata"] = serde_json::Value::Object(meta_obj);
            if let Some(ref img) = args.image {
                json["image"] = serde_json::Value::String(img.display().to_string());
            }
        }
        println!("{}", serde_json::to_string(&json)?);
    } else if quiet == 0 {
        if args.dry_run {
            println!("{} {}", style("dry-run:").yellow().bold(), result.url);
            println!();
            println!("  {:<14} {}", style("identifier:").dim(), result.identifier);
            println!("  {:<14} collection", style("mediatype:").dim());
            for (key, value) in metadata {
                println!("  {:<14} {}", style(format!("{key}:")).dim(), value);
            }
            if let Some(ref img) = args.image {
                println!("  {:<14} {}", style("image:").dim(), img.display());
            }
        } else {
            println!("{} {}", style("created:").green().bold(), result.url);
        }
    }
    Ok(())
}
