//! `ia account`: look up, lock or unlock archive.org accounts.
//!
//! The work for one account is [`run_one`], which takes one input line and
//! returns an [`AccountResult`]; [`print_result`] formats it. The command
//! runs it for the positional `USER`, or for every line of a list read from
//! `--itemlist` or standard input, up to `--jobs` accounts at a time.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::{ArgGroup, Args};
use color_print::cstr;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::{Mutex, OnceCell};

use ia_core::account::{self, LookupType};
use ia_core::error::format_error_chain;
use ia_core::joblog::{self, JoblogEntry, JoblogWriter};
use ia_core::rate_limit::RateLimiter;
use ia_core::{IaClient, IaError};

/// Accounts processed at once when `--jobs` is not given; the same default
/// as `metadata modify`, the other command that writes through an
/// admin-facing API.
const DEFAULT_JOBS: usize = 2;

/// The job log `op` of a lock.
const LOCK_OP: &str = "account-lock";

/// The job log `op` of an unlock.
const UNLOCK_OP: &str = "account-unlock";

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
        requests: a lookup, then the lock or unlock with the item name the lookup returned.\n\n\
        Without USER, accounts are read one per line from --itemlist FILE (- reads standard \
        input) or from piped standard input. Lines are trimmed; blank lines, lines starting \
        with # and repeated lines are skipped. Accounts run --jobs at a time (default 2) and \
        each result line starts with its input line. Lines that name the same account (the \
        same item name) get one lock or unlock request between them. A 429 pauses every \
        worker for the server's Retry-After (at least 1 s; 30 s when absent).\n\n\
        --dry-run prints how each line would be looked up and sends nothing. With --lock or \
        --unlock, --joblog records each line, and a rerun with the same job log skips the lines \
        it records as done with the same action (--no-resume runs them all). Read actions \
        ignore --joblog.",
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
         \n  <bold>$ ia account @example-user --unlock --json</bold>\
         \n\n  <dim># Check how each line of a list would be looked up, sending nothing</dim>\
         \n  <bold>$ ia account --itemlist accounts.txt --lock --dry-run</bold>\
         \n\n  <dim># Lock every account in a list, recording each in a job log</dim>\
         \n  <bold>$ ia account --itemlist accounts.txt --lock --comment \"spam\" --joblog lock.jsonl</bold>\
         \n\n  <dim># Print whether each piped account is locked, four at a time</dim>\
         \n  <bold>$ cat accounts.txt | ia account --is-locked -j 4</bold>\n"
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

    /// Read accounts from FILE, one per line (- reads standard input)
    #[arg(long, value_name = "FILE", conflicts_with = "user")]
    pub itemlist: Option<PathBuf>,

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

    /// Print what would be done for each account; send nothing
    #[arg(long)]
    pub dry_run: bool,

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

    /// The job log `op` of a lock or unlock; `None` for a read action,
    /// which does not use the job log.
    #[must_use]
    pub fn joblog_op(self) -> Option<&'static str> {
        match self {
            Self::Lock => Some(LOCK_OP),
            Self::Unlock => Some(UNLOCK_OP),
            Self::Lookup
            | Self::GetEmail
            | Self::GetScreenname
            | Self::GetItemname
            | Self::IsLocked => None,
        }
    }

    /// The word a lock or unlock result line ends with.
    fn done_word(self) -> &'static str {
        match self {
            Self::Unlock => "unlocked",
            Self::Lock
            | Self::Lookup
            | Self::GetEmail
            | Self::GetScreenname
            | Self::GetItemname
            | Self::IsLocked => "locked",
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
    /// Another line named the same account and its lock or unlock was
    /// accepted; no request was sent for this line.
    SameAccount {
        /// The item name the other line's request was sent with.
        itemname: String,
        /// The input line that sent the request.
        first: String,
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
    /// What the action produced, or why it failed. The error is shared
    /// when several lines name one account whose lock or unlock failed.
    pub result: std::result::Result<Done, Arc<IaError>>,
}

/// The one lock or unlock request sent for an item name in a run.
#[derive(Debug)]
struct SentLock {
    /// The input line that sent it.
    first: String,
    /// The item name it was sent with.
    itemname: String,
    /// The server's answer, or why the request failed.
    result: std::result::Result<Value, Arc<IaError>>,
}

/// What every account in one run shares: the client, the action and its
/// comment, the rate limiter, and the lock or unlock requests already sent,
/// keyed by item name.
pub struct Batch<'a> {
    client: &'a IaClient,
    action: Action,
    comment: Option<&'a str>,
    limiter: RateLimiter,
    locks: Mutex<HashMap<String, Arc<OnceCell<SentLock>>>>,
}

impl<'a> Batch<'a> {
    /// A run of `action` with `comment`, before any request.
    #[must_use]
    pub fn new(client: &'a IaClient, action: Action, comment: Option<&'a str>) -> Self {
        Self {
            client,
            action,
            comment,
            limiter: RateLimiter::new(),
            locks: Mutex::new(HashMap::new()),
        }
    }
}

/// Run the batch's action for one account: look it up, then read a field
/// or send the lock or unlock. A 429 pauses every worker sharing the
/// batch's rate limiter for the server's wait and sends the same request
/// again, with no cap. A lock or unlock is sent once per item name in the
/// batch; a later line for the same account reports the first line's
/// result as [`Done::SameAccount`] or its error.
pub async fn run_one(batch: &Batch<'_>, input: &str) -> AccountResult {
    let start = Instant::now();
    let lookup = LookupType::detect(input);
    let result = perform(batch, input, lookup).await;
    AccountResult {
        input: input.to_string(),
        lookup,
        action: batch.action,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        result,
    }
}

async fn perform(
    batch: &Batch<'_>,
    input: &str,
    lookup: LookupType,
) -> std::result::Result<Done, Arc<IaError>> {
    let found = with_rate_limit(&batch.limiter, || {
        account::lookup_by(batch.client, lookup, input)
    })
    .await
    .map_err(|e| Arc::new(name_input(e, input)))?;
    let missing = |field: &str| {
        Arc::new(IaError::AccountApi {
            account: input.to_string(),
            message: format!("lookup returned no {field}"),
        })
    };
    let field = |key: &'static str, value: Option<&str>| {
        value
            .map(|v| Done::Field {
                key,
                value: v.to_string(),
            })
            .ok_or_else(|| missing(key))
    };
    match batch.action {
        Action::Lookup => Ok(Done::Values(found.values)),
        Action::GetEmail => field("canonical_email", found.canonical_email()),
        Action::GetScreenname => field("screenname", found.screenname()),
        Action::GetItemname => field("itemname", found.itemname()),
        Action::IsLocked => found
            .locked()
            .map(Done::Locked)
            .ok_or_else(|| missing("locked")),
        Action::Lock | Action::Unlock => {
            lock_once(batch, input, found.itemname().unwrap_or_default()).await
        }
    }
}

/// Send the batch's lock or unlock for `itemname`, once per item name in
/// the batch. The key is the item name exactly as the server returned it:
/// every spelling of one account looks up to the same string, and two
/// item names that differ only in case may be two accounts. The first line
/// to get here sends it; lines that get here later, or while it is in
/// flight, wait for it and report its result as their own.
async fn lock_once(
    batch: &Batch<'_>,
    input: &str,
    itemname: &str,
) -> std::result::Result<Done, Arc<IaError>> {
    let locked = batch.action == Action::Lock;
    let send = || async {
        with_rate_limit(&batch.limiter, || {
            account::set_locked(batch.client, itemname, locked, batch.comment)
        })
        .await
        .map_err(|e| Arc::new(name_input(e, input)))
    };

    // An absent item name is refused by set_locked before any request; it
    // names no account, so it is not shared with other lines.
    if itemname.is_empty() {
        return send().await.map(|response| Done::LockSet {
            itemname: String::new(),
            response,
        });
    }

    let cell = {
        let mut locks = batch.locks.lock().await;
        Arc::clone(locks.entry(itemname.to_string()).or_default())
    };
    let sent = cell
        .get_or_init(|| async {
            SentLock {
                first: input.to_string(),
                itemname: itemname.to_string(),
                result: send().await,
            }
        })
        .await;

    // Input lines are unique within a run, so the line that sent the
    // request is the one whose text it records.
    if sent.first == input {
        return sent.result.clone().map(|response| Done::LockSet {
            itemname: sent.itemname.clone(),
            response,
        });
    }
    match &sent.result {
        Ok(_) => Ok(Done::SameAccount {
            itemname: sent.itemname.clone(),
            first: sent.first.clone(),
        }),
        Err(e) => Err(name_shared(e, input)),
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

/// [`name_input`] for an error shared by several lines: an
/// [`IaError::AccountApi`] is copied with this line as its account; any
/// other error is shared as it is.
fn name_shared(err: &Arc<IaError>, input: &str) -> Arc<IaError> {
    match err.as_ref() {
        IaError::AccountApi { message, .. } => Arc::new(IaError::AccountApi {
            account: input.to_string(),
            message: message.clone(),
        }),
        _ => Arc::clone(err),
    }
}

// ─── Input ───────────────────────────────────────────────────────────────────

/// The accounts to run and whether they came from a list (`--itemlist` or
/// standard input) rather than the positional `USER`.
#[derive(Debug)]
struct Accounts {
    lines: Vec<String>,
    list: bool,
}

/// The error for a command given no source of accounts.
fn no_account_given() -> IaError {
    IaError::InvalidArgument(
        "no account given: pass USER, --itemlist <FILE>, or pipe accounts on standard input".into(),
    )
}

/// An I/O error with what was being read, so the message names the file.
fn read_error(what: &str, err: &std::io::Error) -> IaError {
    IaError::Io(std::io::Error::new(
        err.kind(),
        format!("cannot read {what}: {err}"),
    ))
}

fn read_stdin() -> std::result::Result<String, IaError> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| read_error("standard input", &e))?;
    Ok(text)
}

/// The accounts in a list: a leading byte-order mark (U+FEFF, which some
/// editors write and `trim` keeps) removed; each line trimmed; blank lines
/// and lines starting with `#` skipped; repeated lines dropped, the first
/// kept. Lines are not validated as item identifiers, since email
/// addresses and screen names are not identifiers.
fn parse_list(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| seen.insert(*line))
        .map(str::to_string)
        .collect()
}

/// Read the accounts from the positional `USER`; else `--itemlist` (`-`
/// for standard input); else standard input when it is not a terminal.
fn read_accounts(args: &AccountArgs) -> std::result::Result<Accounts, IaError> {
    if let Some(user) = &args.user {
        let user = user.trim();
        if user.is_empty() {
            return Err(IaError::InvalidArgument("USER is empty".into()));
        }
        return Ok(Accounts {
            lines: vec![user.to_string()],
            list: false,
        });
    }
    let stdin_is_terminal = std::io::stdin().is_terminal();
    let (text, source) = match &args.itemlist {
        Some(path) if path.as_os_str() == "-" => {
            if stdin_is_terminal {
                return Err(IaError::InvalidArgument(
                    "--itemlist - needs standard input from a pipe or file".into(),
                ));
            }
            (read_stdin()?, "standard input".to_string())
        }
        Some(path) => {
            let shown = path.display().to_string();
            let text = std::fs::read_to_string(path).map_err(|e| read_error(&shown, &e))?;
            (text, shown)
        }
        None if !stdin_is_terminal => (read_stdin()?, "standard input".to_string()),
        None => return Err(no_account_given()),
    };
    let lines = parse_list(&text);
    if lines.is_empty() {
        return Err(IaError::InvalidArgument(format!(
            "no accounts found in {source}"
        )));
    }
    Ok(Accounts { lines, list: true })
}

// ─── Job log ─────────────────────────────────────────────────────────────────

/// The input lines a rerun with `op` skips: those whose latest entry, in
/// file order, among `account-lock` and `account-unlock` is `ok` with `op`.
fn done_in_joblog(path: &Path, op: &str) -> std::result::Result<HashSet<String>, IaError> {
    let entries = joblog::read(path).map_err(|e| match e {
        IaError::Io(io) => read_error(&format!("joblog {}", path.display()), &io),
        other => other,
    })?;
    let latest: HashMap<&str, &JoblogEntry> = entries
        .iter()
        .filter(|e| e.op == LOCK_OP || e.op == UNLOCK_OP)
        .map(|e| (e.item.as_str(), e))
        .collect();
    Ok(latest
        .into_iter()
        .filter(|(_, e)| e.status == "ok" && e.op == op)
        .map(|(item, _)| item.to_string())
        .collect())
}

/// The job log entry for one finished line: `ok` with bytes 0 and the
/// elapsed time, or `error` with the message.
fn joblog_entry(op: &str, result: &AccountResult) -> JoblogEntry {
    let entry = JoblogEntry::new(op, &result.input, "");
    match &result.result {
        Ok(_) => entry.ok(0, result.elapsed_ms),
        Err(e) => entry.error(&format_error_chain(e.as_ref()), 0),
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

/// One `--json --dry-run` line.
#[derive(Serialize)]
struct DryRunRecord<'a> {
    account: &'a str,
    lookup: &'static str,
    action: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<&'a str>,
    dry_run: bool,
}

/// Print one account's result: a `--json` line on stdout; or, for a human,
/// a read value on stdout at every quiet level, a lock or unlock line on
/// stdout unless `-q`, an error on stderr unless `-qq`. With `list`, each
/// stdout line starts with `<input>: ` and the whole account is one line of
/// JSON, so results match their input lines.
pub fn print_result(result: &AccountResult, json: bool, quiet: u8, list: bool) -> Result<()> {
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
                    Done::SameAccount { itemname, first } => {
                        fields.insert("itemname".into(), Value::String(itemname.clone()));
                        fields.insert("same_as".into(), Value::String(first.clone()));
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

    let input = &result.input;
    let read_value = |value: &str| {
        if list {
            println!("{input}: {value}");
        } else {
            println!("{value}");
        }
    };
    match &result.result {
        Ok(Done::Values(values)) => {
            if list {
                read_value(&serde_json::to_string(values)?);
            } else {
                read_value(&serde_json::to_string_pretty(values)?);
            }
        }
        Ok(Done::Field { value, .. }) => read_value(value),
        Ok(Done::Locked(locked)) => read_value(&locked.to_string()),
        Ok(Done::LockSet { .. }) => {
            if quiet == 0 {
                println!("{input}: {}", result.action.done_word());
            }
        }
        Ok(Done::SameAccount { first, .. }) => {
            if quiet == 0 {
                println!(
                    "{input}: {} (same account as {first})",
                    result.action.done_word()
                );
            }
        }
        Err(e) => {
            if quiet < 2 {
                eprintln!("error: {input}: {}", format_error_chain(e.as_ref()));
            }
        }
    }
    Ok(())
}

/// Print what a dry run would do for one account. A comment is shown for a
/// lock or unlock when it is not empty, since an empty one is not sent; the
/// human line shows it quoted and escaped (`{:?}`), so quotes and newlines
/// in it cannot break the line.
fn print_dry_run(input: &str, action: Action, comment: Option<&str>, json: bool) -> Result<()> {
    let lookup = LookupType::detect(input).field();
    let comment = comment.filter(|c| !c.is_empty() && action.joblog_op().is_some());
    if json {
        let record = DryRunRecord {
            account: input,
            lookup,
            action: action.name(),
            status: "dry_run",
            comment,
            dry_run: true,
        };
        println!("{}", serde_json::to_string(&record)?);
    } else {
        let shown = comment
            .map(|c| format!(", comment {c:?}"))
            .unwrap_or_default();
        println!(
            "dry-run {input}: {} (lookup by {lookup}){shown}",
            action.name()
        );
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

/// Run `ia account`. `jobs` is the global `--jobs` (default 2); `joblog`
/// and `no_resume` are the global `--joblog` and `--no-resume`, used only
/// by `--lock` and `--unlock`.
pub async fn run(
    client: &IaClient,
    args: AccountArgs,
    quiet: u8,
    jobs: Option<usize>,
    joblog: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
    let action = Action::from_args(&args);
    let Accounts { mut lines, list } = match read_accounts(&args) {
        Ok(accounts) => accounts,
        Err(e) => return command_error(e, args.json),
    };

    // Only lock and unlock use the job log; a read action neither reads
    // nor writes it.
    let joblog = action.joblog_op().zip(joblog);
    if let (Some((op, path)), false) = (&joblog, no_resume) {
        if path.exists() {
            let done = match done_in_joblog(path, op) {
                Ok(done) => done,
                Err(e) => return command_error(e, args.json),
            };
            let before = lines.len();
            lines.retain(|line| !done.contains(line));
            let skipped = before - lines.len();
            if skipped > 0 && quiet == 0 {
                eprintln!("resuming: skipping {skipped} account(s) already done in joblog");
            }
        }
    }

    let comment = args.comment.as_deref();
    if args.dry_run {
        for line in &lines {
            print_dry_run(line, action, comment, args.json)?;
        }
        if !args.json && quiet < 2 {
            eprintln!(
                "dry-run: would {} {} account(s), nothing sent",
                action.name(),
                lines.len()
            );
        }
        return Ok(());
    }
    if lines.is_empty() {
        // Every line is already done in the job log.
        return Ok(());
    }

    // Checked once before any account, so a missing cookie or a host the
    // cookies may not go to stops the command rather than failing each one.
    if let Err(e) = account::cookie_header(client) {
        return command_error(e, args.json);
    }

    let writer = match &joblog {
        Some((op, path)) => match JoblogWriter::open(path) {
            Ok(writer) => Some((*op, writer)),
            Err(e) => return command_error(e, args.json),
        },
        None => None,
    };

    let batch = Batch::new(client, action, comment);
    let mut results = stream::iter(&lines)
        .map(|line| run_one(&batch, line))
        .buffer_unordered(jobs.unwrap_or(DEFAULT_JOBS));
    let (mut succeeded, mut failed) = (0usize, 0usize);
    let mut print_error = None;
    while let Some(result) = results.next().await {
        // The entry is written before the result is printed, so a lock the
        // server accepted is recorded even when printing fails or a closed
        // stdout ends the process (SIGPIPE has its default action; see
        // `reset_sigpipe` in lib.rs).
        if let Some((op, writer)) = &writer {
            writer.write(&joblog_entry(op, &result));
        }
        if result.result.is_ok() {
            succeeded += 1;
        } else {
            failed += 1;
        }
        // A print error does not stop the run: the requests still in
        // flight finish and get their entries, and the error is returned
        // afterwards.
        if print_error.is_none() {
            print_error = print_result(&result, args.json, quiet, list).err();
        }
    }
    if let Some(e) = print_error {
        return Err(e);
    }

    if list && lines.len() > 1 && !args.json && quiet < 2 {
        eprintln!("{succeeded} succeeded, {failed} failed");
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list_trims_skips_and_dedups() {
        let text = "# heading\n  a  \n\n  # note\nb\na\n#c\nB\n";
        assert_eq!(parse_list(text), ["a", "b", "B"]);
    }

    #[test]
    fn parse_list_strips_a_leading_byte_order_mark() {
        assert_eq!(parse_list("\u{feff}# note\n@a\n"), ["@a"]);
        assert_eq!(parse_list("\u{feff}@a\nb\n"), ["@a", "b"]);
    }

    fn entry(op: &str, item: &str, ok: bool) -> String {
        let e = JoblogEntry::new(op, item, "");
        let e = if ok { e.ok(0, 1) } else { e.error("x", 0) };
        format!("{}\n", serde_json::to_string(&e).unwrap_or_default())
    }

    #[test]
    fn done_in_joblog_takes_the_latest_lock_or_unlock_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("log.jsonl");
        let text = [
            entry(LOCK_OP, "a", true),
            entry(UNLOCK_OP, "a", true),
            entry(UNLOCK_OP, "b", true),
            entry(LOCK_OP, "b", true),
            entry(LOCK_OP, "c", true),
            entry(LOCK_OP, "c", false),
            entry(LOCK_OP, "d", true),
            entry("task-submit", "d", false),
        ]
        .concat();
        std::fs::write(&path, text).unwrap();
        let mut lock: Vec<String> = done_in_joblog(&path, LOCK_OP)
            .unwrap()
            .into_iter()
            .collect();
        lock.sort();
        assert_eq!(lock, ["b", "d"]);
        let unlock: Vec<String> = done_in_joblog(&path, UNLOCK_OP)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(unlock, ["a"]);
    }
}
