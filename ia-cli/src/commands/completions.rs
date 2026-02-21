use anyhow::Result;
use clap::Args;
use clap_complete::Shell;

#[derive(Args)]
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
