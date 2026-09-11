use anyhow::{Context, Result};
use clap::Args;
use color_print::cstr;
use std::path::{Path, PathBuf};

#[derive(Args)]
#[command(
    long_about = "Generate roff man pages from the command tree. Writes one page per \
        command \u{2014} ia-cli.1, ia-cli-metadata.1, ia-cli-metadata-modify.1, and so on \
        \u{2014} so `man ia-cli-metadata-modify` works the way it does for git and cargo.\n\n\
        With no --out-dir, prints the top-level page to stdout.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Write the full page tree to a directory</dim>\n  <bold>$ ia man --out-dir man/</bold>\
         \n\n  <dim># Install system-wide (macOS/Linux)</dim>\n  <bold>$ ia man --out-dir /usr/local/share/man/man1/</bold>\
         \n\n  <dim># Print just the top-level page</dim>\n  <bold>$ ia man > ia-cli.1</bold>\n"
    ),
)]
pub struct ManArgs {
    /// Write one page per command into this directory (created if absent)
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,

    /// Override the command name used in generated pages
    #[arg(long)]
    pub rename: Option<String>,
}

pub fn run(args: ManArgs, cmd: &clap::Command) -> Result<()> {
    let cmd = match &args.rename {
        Some(name) => cmd.clone().name(name.clone()),
        None => cmd.clone(),
    };

    match &args.out_dir {
        None => {
            let mut out = std::io::stdout();
            clap_mangen::Man::new(cmd).render(&mut out)?;
        }
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
            let written = write_tree(&cmd, dir, None)?;
            eprintln!("wrote {written} man page(s) to {}", dir.display());
        }
    }
    Ok(())
}

/// Render `cmd` and every subcommand beneath it, returning the page count.
///
/// Follows the convention `git` and `cargo` use, where the page name and the
/// invocation differ: the file is `ia-cli-metadata-modify.1` and the title
/// reads `ia-cli-metadata-modify(1)`, but SYNOPSIS shows what you actually
/// type — `ia-cli metadata modify`. `man git-rebase` behaves the same way.
///
/// `page` is the parent's hyphenated page name, `invocation` its spaced
/// command path.
fn write_tree(cmd: &clap::Command, dir: &Path, parent: Option<(&str, &str)>) -> Result<usize> {
    let (page_name, invocation) = match parent {
        Some((page, invoked)) => (
            format!("{page}-{}", cmd.get_name()),
            format!("{invoked} {}", cmd.get_name()),
        ),
        None => (cmd.get_name().to_string(), cmd.get_name().to_string()),
    };

    // name drives the page title, bin_name drives SYNOPSIS.
    let rendered = cmd
        .clone()
        .name(page_name.clone())
        .bin_name(invocation.clone());

    let path = dir.join(format!("{page_name}.1"));
    let mut buf: Vec<u8> = Vec::new();
    clap_mangen::Man::new(rendered).render(&mut buf)?;
    std::fs::write(&path, buf).with_context(|| format!("failed to write {}", path.display()))?;

    let mut count = 1;
    for sub in cmd.get_subcommands() {
        // `help` is clap's built-in and has no useful page of its own.
        if sub.get_name() == "help" {
            continue;
        }
        count += write_tree(sub, dir, Some((&page_name, &invocation)))?;
    }
    Ok(count)
}
