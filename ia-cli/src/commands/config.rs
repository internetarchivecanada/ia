use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

/// Configure Internet Archive credentials and settings.
#[derive(Debug, Args)]
#[command(
    long_about = "Configure Internet Archive credentials and settings.\n\n\
        Log in to archive.org, view configuration, validate credentials, and \
        retrieve account information.",
    after_long_help = cstr!(
        "<bold><green>Examples:</green></bold>\n  \
         <dim># Interactive login (prompts for email and password)</dim>\n  \
         ia config login\n\n  \
         <dim># Non-interactive login</dim>\n  \
         ia config login -u user@example.com -p mypassword\n\n  \
         <dim># Show current config (secrets redacted)</dim>\n  \
         ia config show\n\n  \
         <dim># Check if stored credentials are valid</dim>\n  \
         ia config check\n\n  \
         <dim># Show account info</dim>\n  \
         ia config whoami"
    )
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Log in to archive.org and save credentials
    #[command(
        long_about = "Log in to archive.org and save credentials to the config file.\n\n\
            Authenticates with archive.org using your email and password, then \
            writes S3 keys and cookies to the config file. If a config file already \
            exists, existing settings (host, logging, etc.) are preserved.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Interactive login</dim>\n  \
             ia config login\n\n  \
             <dim># Non-interactive login</dim>\n  \
             ia config login -u user@example.com -p mypassword\n\n  \
             <dim># Login using .netrc credentials</dim>\n  \
             ia config login --netrc"
        )
    )]
    Login(LoginArgs),

    /// Print current configuration
    #[command(
        long_about = "Print the current configuration as JSON.\n\n\
            Shows all config sections (s3, cookies, general, logging, ai). \
            Secrets (S3 keys, cookies, AI API key) are redacted by default.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Show config with redacted secrets</dim>\n  \
             ia config show\n\n  \
             <dim># Machine-readable JSON output</dim>\n  \
             ia config show --json"
        )
    )]
    Show(ShowArgs),

    /// Validate stored S3 credentials
    #[command(
        long_about = "Check if the stored S3 credentials are valid by contacting archive.org.\n\n\
            Exits with code 0 if valid, 1 if invalid.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Check credentials</dim>\n  \
             ia config check\n\n  \
             <dim># Check with JSON output</dim>\n  \
             ia config check --json"
        )
    )]
    Check(CheckArgs),

    /// Show account information
    #[command(
        long_about = "Retrieve and display account information from archive.org.\n\n\
            Shows your screenname, email, and itemname.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Show account info</dim>\n  \
             ia config whoami\n\n  \
             <dim># JSON output</dim>\n  \
             ia config whoami --json"
        )
    )]
    Whoami(WhoamiArgs),

    /// Print cookies in Netscape format
    #[command(
        name = "print-cookies",
        long_about = "Print stored cookies in Netscape cookie format.\n\n\
            Outputs cookies suitable for use with curl, wget, or other tools \
            that accept Netscape-format cookie files.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Print cookies</dim>\n  \
             ia config print-cookies\n\n  \
             <dim># Save to cookie file for curl</dim>\n  \
             ia config print-cookies > cookies.txt\n  \
             curl -b cookies.txt https://archive.org/..."
        )
    )]
    PrintCookies(PrintCookiesArgs),

    /// Print the Authorization header
    #[command(
        name = "print-auth",
        long_about = "Print the Authorization header value for S3 API requests.\n\n\
            Outputs the header in the format: Authorization: LOW {access}:{secret}\n\
            Useful for scripting with curl or other HTTP tools.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Print auth header</dim>\n  \
             ia config print-auth\n\n  \
             <dim># Use with curl</dim>\n  \
             curl -H \"$(ia config print-auth)\" https://s3.us.archive.org/..."
        )
    )]
    PrintAuth(PrintAuthArgs),
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Email address for login
    #[arg(short, long)]
    pub username: Option<String>,

    /// Password for login
    #[arg(short, long)]
    pub password: Option<String>,

    /// Read credentials from ~/.netrc
    #[arg(short, long)]
    pub netrc: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Output as JSON (machine-readable, no color)
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct WhoamiArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PrintCookiesArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PrintAuthArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

fn dirs_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}

/// Run the config command.
pub async fn run(
    args: ConfigArgs,
    config: ia_core::IaConfig,
    config_path: Option<PathBuf>,
) -> Result<()> {
    match args.command {
        ConfigCommand::Show(show_args) => {
            let json_value = config.to_json(true);
            if show_args.json {
                println!("{}", serde_json::to_string(&json_value)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&json_value)?);
            }
            Ok(())
        }
        ConfigCommand::Login(login_args) => {
            let (email, password) = if login_args.netrc {
                let netrc_path = dirs_path()
                    .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
                    .join(".netrc");
                ia_core::auth::parse_netrc(&netrc_path)?
            } else {
                let email = match login_args.username {
                    Some(u) => u,
                    None => {
                        if !atty::is(atty::Stream::Stdin) {
                            anyhow::bail!(
                                "no username provided and stdin is not a terminal.\n\
                                 Use -u/--username and -p/--password for non-interactive login."
                            );
                        }
                        eprint!("Email address: ");
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        input.trim().to_string()
                    }
                };
                let password = match login_args.password {
                    Some(p) => p,
                    None => {
                        if !atty::is(atty::Stream::Stdin) {
                            anyhow::bail!(
                                "no password provided and stdin is not a terminal.\n\
                                 Use -u/--username and -p/--password for non-interactive login."
                            );
                        }
                        rpassword::prompt_password_stderr("Password: ")?
                    }
                };
                (email, password)
            };

            // Build a minimal client for the login request
            let client = ia_core::IaClient::from_config(config)?;
            let auth = ia_core::auth::login(&client, &email, &password).await?;

            // Determine where to write the config
            let write_path = config_path
                .unwrap_or_else(ia_core::IaConfig::find_or_default_config_path);

            ia_core::IaConfig::write_config_file(&auth, &write_path)?;

            if login_args.json {
                let json = serde_json::json!({
                    "config_file": write_path.display().to_string(),
                    "screenname": auth.screenname,
                });
                println!("{}", serde_json::to_string(&json)?);
            } else {
                eprintln!(
                    "{} Config saved to {}",
                    style("✓").green().bold(),
                    style(write_path.display()).cyan()
                );
            }

            Ok(())
        }
        ConfigCommand::Check(check_args) => {
            let client = ia_core::IaClient::from_config(config)?;
            match ia_core::auth::check_keys(&client).await {
                Ok(info) => {
                    if check_args.json {
                        let json = serde_json::json!({
                            "valid": true,
                            "screenname": info.screenname,
                            "email": info.email,
                            "itemname": info.itemname,
                        });
                        println!("{}", serde_json::to_string(&json)?);
                    } else {
                        eprintln!(
                            "{} Credentials valid ({})",
                            style("✓").green().bold(),
                            style(&info.screenname).cyan()
                        );
                    }
                    Ok(())
                }
                Err(e) => {
                    if check_args.json {
                        let json = serde_json::json!({
                            "valid": false,
                            "error": e.to_string(),
                        });
                        println!("{}", serde_json::to_string(&json)?);
                        std::process::exit(1);
                    } else {
                        eprintln!("{} {}", style("✗").red().bold(), e);
                        std::process::exit(1);
                    }
                }
            }
        }

        ConfigCommand::Whoami(whoami_args) => {
            let client = ia_core::IaClient::from_config(config)?;
            let info = ia_core::auth::whoami(&client).await?;

            if whoami_args.json {
                let json = serde_json::json!({
                    "screenname": info.screenname,
                    "email": info.email,
                    "itemname": info.itemname,
                });
                println!("{}", serde_json::to_string(&json)?);
            } else {
                println!("Screenname: {}", style(&info.screenname).cyan());
                println!("Email:      {}", info.email);
                if let Some(itemname) = &info.itemname {
                    println!("Itemname:   {}", itemname);
                }
            }

            Ok(())
        }
        ConfigCommand::PrintCookies(_print_cookies_args) => todo!("print-cookies"),
        ConfigCommand::PrintAuth(_print_auth_args) => todo!("print-auth"),
    }
}
