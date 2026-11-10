//! `ia account`: look up, lock or unlock archive.org accounts.
//!
//! The work for one account is [`run_one`], which takes one input string
//! and returns an [`AccountResult`]; [`print_result`] formats it. The
//! command runs it once for the positional `USER`.

use std::future::Future;
use std::time::Instant;

use anyhow::Result;
use clap::{ArgGroup, Args};
use color_print::cstr;
use serde::Serialize;
use serde_json::{Map, Value};

use ia_core::account::{self, LookupType};
use ia_core::error::format_error_chain;
use ia_core::rate_limit::RateLimiter;
use ia_core::{IaClient, IaError};

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    about = "Look up, lock or unlock archive.org accounts",
    long_about = "Look up, lock or unlock archive.org accounts. The account API is admin-only.\n\n\
        Requests carry the logged-in-user and logged-in-sig cookies from the [cookies] section \
        of the config file (written by `ia config login`), and no S3 keys. The cookies are sent \
        only when the host is archive.org, a subdomain of it, or a loopback address.\n\n\
        USER is looked up by its form: a value that starts with @ is an item name; otherwise a \
        value shaped like an email address is an email address; anything else is a screen name. \
        Every answer is checked to be the account asked for, ignoring letter case.\n\n\
        With no action flag, prints the whole account as JSON. --lock and --unlock send two \
        requests: a lookup, then the lock or unlock with the item name the lookup returned.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Print an account as JSON</dim>\
         \n  <bold>$ ia account person@example.com</bold>\
         \n\n  <dim># Print the item name of a screen name</dim>\
         \n  <bold>$ ia account example-user --get-itemname</bold>\
         \n\n  <dim># Check whether an account is locked</dim>\
         \n  <bold>$ ia account @example-user --is-locked</bold>\
         \n\n  <dim># Lock an account with a comment</dim>\
         \n  <bold>$ ia account person@example.com --lock --comment \"spam\"</bold>\
         \n\n  <dim># Unlock an account, one JSON object on stdout</dim>\
         \n  <bold>$ ia account @example-user --unlock --json</bold>\n"
    ),
    group(
        ArgGroup::new("action")
            .args(["get_email", "get_screenname", "get_itemname", "is_locked", "lock", "unlock"])
            .multiple(false)
    ),
    group(ArgGroup::new("write").args(["lock", "unlock"]).multiple(false)),
)]
pub struct AccountArgs {
    /// Account: an email address, a screen name, or an @item name
    #[arg()]
    pub user: Option<String>,

    /// Print the account's canonical email address
    #[arg(short = 'g', long)]
    pub get_email: bool,

    /// Print the account's screen name
    #[arg(short = 's', long)]
    pub get_screenname: bool,

    /// Print the account's item name (it starts with @)
    #[arg(long)]
    pub get_itemname: bool,

    /// Print whether the account is locked: true or false
    #[arg(long)]
    pub is_locked: bool,

    /// Lock the account
    #[arg(short = 'L', long)]
    pub lock: bool,

    /// Unlock the account
    #[arg(short = 'u', long)]
    pub unlock: bool,

    /// Comment recorded with the lock or unlock (not sent when empty)
    #[arg(long, requires = "write")]
    pub comment: Option<String>,

    /// Output one JSON object per account on stdout
    #[arg(long)]
    pub json: bool,
}

// ─── Per-account work ────────────────────────────────────────────────────────

/// What `ia account` does with each account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Print the whole account.
    Lookup,
    /// Print `canonical_email`.
    GetEmail,
    /// Print `screenname`.
    GetScreenname,
    /// Print `itemname`.
    GetItemname,
    /// Print `locked`.
    IsLocked,
    /// Lock the account.
    Lock,
    /// Unlock the account.
    Unlock,
}

impl Action {
    /// The action the flags select; [`Action::Lookup`] when none is given.
    #[must_use]
    pub fn from_args(args: &AccountArgs) -> Self {
        if args.get_email {
            Self::GetEmail
        } else if args.get_screenname {
            Self::GetScreenname
        } else if args.get_itemname {
            Self::GetItemname
        } else if args.is_locked {
            Self::IsLocked
        } else if args.lock {
            Self::Lock
        } else if args.unlock {
            Self::Unlock
        } else {
            Self::Lookup
        }
    }

    /// The name used in `--json` output: `lookup`, `get-email`,
    /// `get-screenname`, `get-itemname`, `is-locked`, `lock` or `unlock`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Lookup => "lookup",
            Self::GetEmail => "get-email",
            Self::GetScreenname => "get-screenname",
            Self::GetItemname => "get-itemname",
            Self::IsLocked => "is-locked",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
        }
    }
}

/// What an action produced for one account.
#[derive(Debug)]
pub enum Done {
    /// The account's whole `values` object.
    Values(Map<String, Value>),
    /// One string field: its key in `values` and its value.
    Field {
        /// The key, such as `canonical_email`.
        key: &'static str,
        /// The value.
        value: String,
    },
    /// The account's `locked` field.
    Locked(bool),
    /// A lock or unlock was accepted.
    LockSet {
        /// The item name the request was sent with.
        itemname: String,
        /// The server's answer, as [`account::set_locked`] returns it.
        response: Value,
    },
}

/// The result of one account, for [`print_result`].
#[derive(Debug)]
pub struct AccountResult {
    /// The account as given.
    pub input: String,
    /// How it was looked up.
    pub lookup: LookupType,
    /// What was done.
    pub action: Action,
    /// Time from the first request to the result.
    pub elapsed_ms: u64,
    /// What the action produced, or why it failed.
    pub result: std::result::Result<Done, IaError>,
}

/// Run `action` for one account: look it up, then read a field or send the
/// lock or unlock. A 429 pauses every worker sharing `limiter` for the
/// server's wait and sends the same request again, with no cap.
pub async fn run_one(
    client: &IaClient,
    input: &str,
    action: Action,
    comment: Option<&str>,
    limiter: &RateLimiter,
) -> AccountResult {
    let start = Instant::now();
    let lookup = LookupType::detect(input);
    let result = perform(client, input, lookup, action, comment, limiter)
        .await
        .map_err(|e| name_input(e, input));
    AccountResult {
        input: input.to_string(),
        lookup,
        action,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        result,
    }
}

async fn perform(
    client: &IaClient,
    input: &str,
    lookup: LookupType,
    action: Action,
    comment: Option<&str>,
    limiter: &RateLimiter,
) -> std::result::Result<Done, IaError> {
    let found = with_rate_limit(limiter, || account::lookup_by(client, lookup, input)).await?;
    let missing = |field: &str| IaError::AccountApi {
        account: input.to_string(),
        message: format!("lookup returned no {field}"),
    };
    let field = |key: &'static str, value: Option<&str>| {
        value
            .map(|v| Done::Field {
                key,
                value: v.to_string(),
            })
            .ok_or_else(|| missing(key))
    };
    match action {
        Action::Lookup => Ok(Done::Values(found.values)),
        Action::GetEmail => field("canonical_email", found.canonical_email()),
        Action::GetScreenname => field("screenname", found.screenname()),
        Action::GetItemname => field("itemname", found.itemname()),
        Action::IsLocked => found
            .locked()
            .map(Done::Locked)
            .ok_or_else(|| missing("locked")),
        Action::Lock | Action::Unlock => {
            // An absent item name is refused by set_locked before any request.
            let itemname = found.itemname().unwrap_or_default().to_string();
            let locked = action == Action::Lock;
            let response = with_rate_limit(limiter, || {
                account::set_locked(client, &itemname, locked, comment)
            })
            .await?;
            Ok(Done::LockSet { itemname, response })
        }
    }
}

/// Send `request` until it is answered with something other than a 429,
/// pausing every worker that shares `limiter` for each 429's wait.
async fn with_rate_limit<T, F, Fut>(
    limiter: &RateLimiter,
    mut request: F,
) -> std::result::Result<T, IaError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, IaError>>,
{
    loop {
        limiter.wait_if_paused().await;
        match request().await {
            Err(IaError::RateLimited { retry_after }) => {
                limiter
                    .pause_for(retry_after, |secs| {
                        eprintln!("Rate limited. Pausing all workers for {secs}s...");
                    })
                    .await;
            }
            other => return other,
        }
    }
}

/// Name the account as given in an [`IaError::AccountApi`], so an error
/// from the lock request (which names the item name) points at the input.
fn name_input(err: IaError, input: &str) -> IaError {
    match err {
        IaError::AccountApi { message, .. } => IaError::AccountApi {
            account: input.to_string(),
            message,
        },
        other => other,
    }
}

// ─── Output ──────────────────────────────────────────────────────────────────

/// One `--json` line.
#[derive(Serialize)]
struct Record<'a> {
    account: &'a str,
    lookup: &'static str,
    action: &'static str,
    status: &'static str,
    #[serde(flatten)]
    fields: Map<String, Value>,
    elapsed_ms: u64,
}

/// Print one account's result: a `--json` line on stdout; or, for a human,
/// a read value on stdout at every quiet level, a lock or unlock line on
/// stdout unless `-q`, an error on stderr unless `-qq`.
pub fn print_result(result: &AccountResult, json: bool, quiet: u8) -> Result<()> {
    if json {
        let mut fields = Map::new();
        let status = match &result.result {
            Ok(done) => {
                match done {
                    Done::Values(values) => {
                        fields.insert("values".into(), Value::Object(values.clone()));
                    }
                    Done::Field { key, value } => {
                        fields.insert((*key).into(), Value::String(value.clone()));
                    }
                    Done::Locked(locked) => {
                        fields.insert("locked".into(), Value::Bool(*locked));
                    }
                    Done::LockSet { itemname, response } => {
                        fields.insert("itemname".into(), Value::String(itemname.clone()));
                        fields.insert("response".into(), response.clone());
                    }
                }
                "ok"
            }
            Err(e) => {
                let body = serde_json::to_value(e.to_json_error().error)?;
                fields.insert("error".into(), body);
                "error"
            }
        };
        let record = Record {
            account: &result.input,
            lookup: result.lookup.field(),
            action: result.action.name(),
            status,
            fields,
            elapsed_ms: result.elapsed_ms,
        };
        println!("{}", serde_json::to_string(&record)?);
        return Ok(());
    }

    match &result.result {
        Ok(Done::Values(values)) => println!("{}", serde_json::to_string_pretty(values)?),
        Ok(Done::Field { value, .. }) => println!("{value}"),
        Ok(Done::Locked(locked)) => println!("{locked}"),
        Ok(Done::LockSet { .. }) => {
            if quiet == 0 {
                let word = match result.action {
                    Action::Unlock => "unlocked",
                    Action::Lock
                    | Action::Lookup
                    | Action::GetEmail
                    | Action::GetScreenname
                    | Action::GetItemname
                    | Action::IsLocked => "locked",
                };
                println!("{}: {word}", result.input);
            }
        }
        Err(e) => {
            if quiet < 2 {
                eprintln!("error: {}: {}", result.input, format_error_chain(e));
            }
        }
    }
    Ok(())
}

// ─── Command ─────────────────────────────────────────────────────────────────

/// A command-level error: `{"error": {…}}` on stderr and exit 1 with
/// `--json`, else returned for `main` to print.
fn command_error(err: IaError, json: bool) -> Result<()> {
    if json {
        ia_core::write_json_error(&err);
        std::process::exit(1);
    }
    Err(err.into())
}

/// Run `ia account`. `_jobs` is the global `--jobs`; one positional account
/// needs no concurrency.
pub async fn run(
    client: &IaClient,
    args: AccountArgs,
    quiet: u8,
    _jobs: Option<usize>,
) -> Result<()> {
    let action = Action::from_args(&args);
    let Some(user) = args
        .user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    else {
        return command_error(
            IaError::InvalidArgument(
                "no account given: pass USER, --itemlist <FILE>, or pipe accounts on standard input"
                    .into(),
            ),
            args.json,
        );
    };

    // Checked once before any account, so a missing cookie or a host the
    // cookies may not go to stops the command rather than failing each one.
    if let Err(e) = account::cookie_header(client) {
        return command_error(e, args.json);
    }

    let limiter = RateLimiter::new();
    let result = run_one(client, user, action, args.comment.as_deref(), &limiter).await;
    print_result(&result, args.json, quiet)?;
    if result.result.is_err() {
        std::process::exit(1);
    }
    Ok(())
}
