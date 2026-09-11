//! Binary entry point.
//!
//! Everything lives in the library (`lib.rs`) so the CLI can be driven
//! in-process as well as from a shell.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ia_cli::run().await
}
