use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

#[derive(Args)]
#[command(
    about = "Update ia to a specific or latest version",
    long_about = "Check for updates, list available versions, or install a specific version of ia.\n\n\
        This command is only available in standalone release builds. If you installed ia via \
        cargo install or a package manager, use that tool to update instead.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Update to the latest version</dim>\n  <bold>$ ia update</bold>\
         \n\n  <dim># Check for updates without installing</dim>\n  <bold>$ ia update --check</bold>\
         \n\n  <dim># List available versions</dim>\n  <bold>$ ia update list</bold>\
         \n\n  <dim># Install a specific version</dim>\n  <bold>$ ia update install 0.5.1</bold>\
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

    #[command(subcommand)]
    pub subcommand: Option<UpdateSubcommand>,
}

#[derive(Subcommand)]
pub enum UpdateSubcommand {
    /// List available versions
    #[command(
        long_about = "List available versions of ia from GitHub Releases.\n\n\
            Shows the 5 most recent versions by default. Use --all to see all versions \
            above the minimum installable version.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># List recent versions</dim>\n  <bold>$ ia update list</bold>\
             \n\n  <dim># List all versions</dim>\n  <bold>$ ia update list --all</bold>\
             \n\n  <dim># JSON output</dim>\n  <bold>$ ia update list --json</bold>\n"
        ),
    )]
    List(ListArgs),

    /// Install a specific version
    #[command(
        long_about = "Install a specific version of ia by version number.\n\n\
            Downloads the release from GitHub and replaces the current binary. \
            Cannot install versions below the minimum supported version.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Install a specific version</dim>\n  <bold>$ ia update install 0.5.1</bold>\
             \n\n  <dim># JSON output</dim>\n  <bold>$ ia update install 0.5.1 --json</bold>\n"
        ),
    )]
    Install(InstallArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// Show all versions (not just the 5 most recent)
    #[arg(long)]
    pub all: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct InstallArgs {
    /// Version to install (e.g., 0.5.1)
    pub version: String,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: UpdateArgs) -> Result<()> {
    let parent_json = args.json;
    match args.subcommand {
        Some(UpdateSubcommand::List(list_args)) => run_list(list_args, parent_json).await,
        Some(UpdateSubcommand::Install(install_args)) => {
            run_install(install_args, parent_json).await
        }
        None => run_default(args).await,
    }
}

/// Original behavior: update to latest or check for updates.
async fn run_default(args: UpdateArgs) -> Result<()> {
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

async fn run_list(args: ListArgs, parent_json: bool) -> Result<()> {
    let json = args.json || parent_json;
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");

    let releases =
        ia_core::update::list_releases(ia_core::update::GITHUB_API_BASE, current_version, target)
            .await;

    match releases {
        Ok(mut releases) => {
            // Default: show only the 5 most recent
            if !args.all && releases.len() > 5 {
                releases.drain(..releases.len() - 5);
            }

            if json {
                println!("{}", serde_json::to_string(&releases)?);
            } else {
                for release in &releases {
                    if release.installed {
                        println!(
                            "{} {} {}",
                            style("\u{2192}").green(),
                            style(&release.version).green().bold(),
                            style("(installed)").dim(),
                        );
                    } else if !release.has_asset {
                        println!(
                            "  {} {}",
                            release.version,
                            style("(no binary for this platform)").dim(),
                        );
                    } else {
                        println!("  {}", release.version);
                    }
                }
                if releases.is_empty() {
                    println!("No versions available.");
                }
            }
        }
        Err(e) => {
            if json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("failed to list versions");
            }
        }
    }

    Ok(())
}

async fn run_install(args: InstallArgs, parent_json: bool) -> Result<()> {
    let json = args.json || parent_json;
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");
    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;

    // Validate the version floor before printing progress — avoids a
    // misleading "Installing..." message when the version is rejected.
    let clean_version = args.version.strip_prefix('v').unwrap_or(&args.version);
    if !ia_core::update::is_at_or_above_minimum(clean_version) {
        let err = ia_core::error::IaError::UpdateBelowMinimum {
            version: clean_version.to_string(),
            minimum: ia_core::update::MIN_INSTALLABLE_VERSION.to_string(),
        };
        if json {
            ia_core::write_json_error(&err);
            std::process::exit(1);
        } else {
            return Err(err).context("install failed");
        }
    }

    if !json {
        println!("Installing ia {}...", style(clean_version).bold());
    }

    let result = ia_core::update::install_version(
        &args.version,
        current_version,
        target,
        &current_exe,
        ia_core::update::GITHUB_API_BASE,
        false,
    )
    .await;

    match result {
        Ok(update_result) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "installed",
                        "previous_version": update_result.current_version,
                        "installed_version": update_result.new_version,
                    })
                );
            } else {
                println!(
                    "  {} Installed ia {} (was {})",
                    style("\u{2713}").green(),
                    style(&update_result.new_version).green().bold(),
                    style(&update_result.current_version).dim(),
                );
            }
        }
        Err(e) => {
            if json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("install failed");
            }
        }
    }

    Ok(())
}
