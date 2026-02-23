use anyhow::Result;
use clap::Args;
use clap_complete::Shell;
use color_print::cstr;

#[derive(Args)]
#[command(
    long_about = "Generate shell completion scripts. Prints a completion script to stdout \u{2014} \
        redirect it to the appropriate file for your shell.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Generate completions for fish</dim>\n  <bold>$ ia completions fish > ~/.config/fish/completions/ia.fish</bold>\
         \n\n  <dim># Generate completions for bash</dim>\n  <bold>$ ia completions bash > ~/.local/share/bash-completion/completions/ia</bold>\n"
    ),
)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: Shell,

    /// Override the binary name in generated completions
    #[arg(long)]
    pub rename: Option<String>,
}

pub fn run(args: CompletionsArgs, cmd: &mut clap::Command) -> Result<()> {
    let name = args.rename.as_deref().unwrap_or("ia");
    clap_complete::generate(args.shell, cmd, name, &mut std::io::stdout());
    Ok(())
}
