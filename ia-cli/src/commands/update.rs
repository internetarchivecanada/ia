use anyhow::{Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;

#[derive(Args)]
#[command(
    about = "Update ia to the latest version",
    long_about = "Check for a newer version of ia on GitHub Releases and update the binary in-place.\n\n\
        This command is only available in standalone release builds. If you installed ia via \
        cargo install or a package manager, use that tool to update instead.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Update to the latest version</dim>\n  <bold>$ ia update</bold>\
         \n\n  <dim># Check for updates without installing</dim>\n  <bold>$ ia update --check</bold>\
         \n\n  <dim># Machine-readable output</dim>\n  <bold>$ ia update --check --json</bold>\n"
    ),
)]
pub struct UpdateArgs {
    /// Only check for updates, don't install
    #[arg(long)]
    pub check: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: UpdateArgs) -> Result<()> {
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");
    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;

    if args.check {
        return run_check(current_version, &args).await;
    }

    if !args.json {
        println!("Checking for updates...");
    }

    let result = ia_core::update::perform_update(
        current_version,
        target,
        &current_exe,
        ia_core::update::GITHUB_API_BASE,
        false,
    )
    .await;

    match result {
        Ok(update_result) => {
            if update_result.current_version == update_result.new_version
                || !ia_core::update::is_newer(
                    &update_result.current_version,
                    &update_result.new_version,
                )
            {
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "up_to_date",
                            "current_version": update_result.current_version,
                        })
                    );
                } else {
                    println!(
                        "  {} ia {} is already the latest version.",
                        style("\u{2713}").green(),
                        update_result.current_version,
                    );
                }
            } else if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "updated",
                        "current_version": update_result.current_version,
                        "new_version": update_result.new_version,
                    })
                );
            } else {
                println!(
                    "  {} Updated ia from {} to {}",
                    style("\u{2713}").green(),
                    style(&update_result.current_version).dim(),
                    style(&update_result.new_version).green().bold(),
                );
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("update failed");
            }
        }
    }

    Ok(())
}

async fn run_check(current_version: &str, args: &UpdateArgs) -> Result<()> {
    let check =
        ia_core::update::check_for_update(current_version, ia_core::update::GITHUB_API_BASE).await;

    match check {
        Ok(info) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "current_version": info.current_version,
                        "latest_version": info.latest_version,
                        "update_available": info.update_available,
                    })
                );
            } else if info.update_available {
                println!("Current version: {}", info.current_version);
                println!(
                    "Latest version:  {}",
                    style(&info.latest_version).green().bold()
                );
                println!(
                    "\nUpdate available! Run {} to install.",
                    style("ia update").cyan()
                );
            } else {
                println!(
                    "  {} ia {} is already the latest version.",
                    style("\u{2713}").green(),
                    info.current_version,
                );
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("failed to check for updates");
            }
        }
    }

    Ok(())
}
