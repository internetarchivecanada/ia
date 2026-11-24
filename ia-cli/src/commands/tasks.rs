use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use comfy_table::{presets, Cell, Color as TableColor, Table};
use console::style;
use futures::{stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};

use ia_core::joblog::{self, JoblogEntry, JoblogWriter};
use ia_core::spreadsheet::read_spreadsheet;
use ia_core::tasks::{self, TaskEntry, TaskSubmission, TasksQuery, TasksSummary};
use ia_core::IaClient;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    about = "Manage archive.org catalog tasks",
    long_about = "Manage archive.org catalog tasks. Lists, submits, reruns, and inspects tasks.\n\n\
        With no subcommand, lists your pending tasks (or tasks for a given item).",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># List your pending tasks</dim>\
         \n  <bold>$ ia tasks</bold>\
         \n\n  <dim># List tasks for an item (active + completed)</dim>\
         \n  <bold>$ ia tasks my-item</bold>\
         \n\n  <dim># Filter by command</dim>\
         \n  <bold>$ ia tasks --cmd derive.php</bold>\
         \n\n  <dim># Submit a derive task</dim>\
         \n  <bold>$ ia tasks submit my-item --cmd derive</bold>\
         \n\n  <dim># View a task log</dim>\
         \n  <bold>$ ia tasks log 1234567</bold>\n"
    ),
    subcommand_required = false,
)]
pub struct TasksArgs {
    /// Item identifier
    #[arg()]
    pub identifier: Option<String>,

    /// Filter by task command
    #[arg(long)]
    pub cmd: Option<String>,

    /// Filter by submitter email
    #[arg(long)]
    pub submitter: Option<String>,

    /// Filter by server
    #[arg(long)]
    pub server: Option<String>,

    /// Filter by priority
    #[arg(long)]
    pub priority: Option<i32>,

    /// Filter by task arguments (supports wildcards)
    #[arg(long)]
    pub args: Option<String>,

    /// Filter by status color (green/blue/red/brown)
    #[arg(long)]
    pub color: Option<String>,

    /// Filter by task ID
    #[arg(long)]
    pub task_id: Option<u64>,

    /// Show tasks submitted after this date/time
    #[arg(long)]
    pub since: Option<String>,

    /// Show tasks submitted before this date/time
    #[arg(long)]
    pub before: Option<String>,

    /// Limit number of results
    #[arg(long)]
    pub limit: Option<u32>,

    /// Only show active tasks (catalog)
    #[arg(long)]
    pub active_only: bool,

    /// Only show completed tasks (history)
    #[arg(long)]
    pub completed_only: bool,

    /// Hide summary counts header
    #[arg(long)]
    pub no_summary: bool,

    /// Raw API parameter KEY=VALUE (repeatable)
    #[arg(short = 'p', long = "parameter")]
    pub parameter: Vec<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<TasksCommand>,
}

#[derive(Debug, Subcommand)]
pub enum TasksCommand {
    /// Submit a new task
    #[command(
        long_about = "Submit a new task.\n\n\
            With --joblog, each task is recorded as soon as IA accepts or refuses it, \
            and a rerun with the same joblog skips every task it records as accepted: \
            the same item, command and --args, whatever the comment or priority.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Submit a derive task</dim>\
             \n  <bold>$ ia tasks submit my-item --cmd derive</bold>\
             \n\n  <dim># Submit with a comment</dim>\
             \n  <bold>$ ia tasks submit my-item --cmd make_dark --comment \"curation request\"</bold>\
             \n\n  <dim># Submit to multiple items</dim>\
             \n  <bold>$ ia tasks submit --cmd derive --itemlist items.txt --comment \"re-derive\"</bold>\
             \n\n  <dim># Batch submit from a spreadsheet</dim>\
             \n  <bold>$ ia tasks submit --spreadsheet jobs.csv</bold>\
             \n\n  <dim># Submit from piped identifiers</dim>\
             \n  <bold>$ cat ids.txt | ia tasks submit --cmd derive</bold>\
             \n\n  <dim># Record submissions; a rerun skips the tasks already accepted</dim>\
             \n  <bold>$ ia tasks submit --cmd derive --itemlist items.txt --joblog derive.jsonl</bold>\n"
        ),
    )]
    Submit(SubmitArgs),

    /// View task execution log
    #[command(
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># View a task log</dim>\
             \n  <bold>$ ia tasks log 1234567</bold>\
             \n\n  <dim># Save to file</dim>\
             \n  <bold>$ ia tasks log 1234567 > task.log</bold>\n"
        ),
    )]
    Log(LogArgs),

    /// Rerun failed tasks
    #[command(
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Rerun a single failed task</dim>\
             \n  <bold>$ ia tasks rerun 1234567</bold>\
             \n\n  <dim># Rerun all failed derive tasks</dim>\
             \n  <bold>$ ia tasks rerun --cmd derive.php</bold>\
             \n\n  <dim># Rerun from pipeline</dim>\
             \n  <bold>$ ia tasks --cmd derive.php --color red --json | ia tasks rerun</bold>\n"
        ),
    )]
    Rerun(RerunArgs),

    /// Check task submission rate limits
    #[command(
        name = "rate-limit",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Check derive rate limits</dim>\
             \n  <bold>$ ia tasks rate-limit</bold>\
             \n\n  <dim># Check a specific command</dim>\
             \n  <bold>$ ia tasks rate-limit make_dark</bold>\n"
        ),
    )]
    RateLimit(RateLimitArgs),
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// Item identifier (omit for batch mode with --itemlist/--search/--spreadsheet)
    #[arg(conflicts_with_all = ["itemlist", "search"])]
    pub identifier: Option<String>,

    /// Task command (e.g. derive, make_dark)
    #[arg(long, required_unless_present = "spreadsheet")]
    pub cmd: Option<String>,

    /// Task arguments as KEY=VALUE (repeatable)
    #[arg(long = "args")]
    pub task_args: Vec<String>,

    /// Explanation for task submission
    #[arg(long)]
    pub comment: Option<String>,

    /// Task priority (-10 to 10)
    #[arg(long, default_value = "0")]
    pub priority: i32,

    /// Submit at reduced priority
    #[arg(long)]
    pub reduced_priority: bool,

    /// Poll until the task completes; exit non-zero if it fails (one task only)
    #[arg(long)]
    pub wait: bool,

    /// Initial poll interval in seconds for --wait
    #[arg(long, default_value = "2")]
    pub wait_interval: u64,

    /// Max retries on 429 rate-limit responses
    #[arg(long, default_value = "10")]
    pub max_retries: u32,

    /// Raw API parameter KEY=VALUE (repeatable; applies to every task)
    #[arg(short = 'p', long = "parameter")]
    pub parameter: Vec<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print what would be submitted without sending
    #[arg(long)]
    pub dry_run: bool,

    /// Read identifiers from file
    #[arg(long, conflicts_with_all = ["identifier", "search"])]
    pub itemlist: Option<std::path::PathBuf>,

    /// Use search results as input
    #[arg(long, conflicts_with_all = ["identifier", "itemlist"])]
    pub search: Option<String>,

    /// Extra search parameters for --search (KEY:VALUE or KEY=VALUE, repeatable;
    /// e.g. --search-parameter sorts='addeddate desc'). Note: -p sets a task
    /// parameter, not a search parameter.
    #[arg(long = "search-parameter", value_name = "PARAMETERS")]
    pub search_parameters: Vec<String>,

    /// Batch submit tasks from a spreadsheet (CSV/TSV/XLSX/ODS/JSONL)
    #[arg(long, conflicts_with_all = ["identifier", "itemlist", "search"])]
    pub spreadsheet: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// Task ID
    #[arg()]
    pub task_id: u64,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RerunArgs {
    /// Task ID(s) to rerun (stdin auto-detected when piped)
    #[arg()]
    pub task_ids: Vec<String>,

    /// Filter by task command
    #[arg(long)]
    pub cmd: Option<String>,

    /// Filter by status color
    #[arg(long)]
    pub color: Option<String>,

    /// Filter by submitter
    #[arg(long)]
    pub submitter: Option<String>,

    /// Filter by identifier
    #[arg(long)]
    pub identifier: Option<String>,

    /// Max retries per rerun request
    #[arg(long, default_value = "3")]
    pub max_retries: u32,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RateLimitArgs {
    /// Task command to check (default: derive.php)
    #[arg(default_value = "derive")]
    pub cmd: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run(
    client: &IaClient,
    args: TasksArgs,
    quiet: u8,
    jobs: usize,
    joblog: Option<std::path::PathBuf>,
    no_resume: bool,
) -> Result<()> {
    match args.command {
        Some(TasksCommand::Submit(submit_args)) => {
            run_submit(client, submit_args, quiet, jobs, joblog, no_resume).await
        }
        Some(TasksCommand::Log(log_args)) => run_log(client, log_args).await,
        Some(TasksCommand::Rerun(rerun_args)) => run_rerun(client, rerun_args, quiet, jobs).await,
        Some(TasksCommand::RateLimit(rl_args)) => run_rate_limit(client, rl_args).await,
        None => run_list(client, &args, quiet).await,
    }
}

// ─── List (bare command) ─────────────────────────────────────────────────────

async fn run_list(client: &IaClient, args: &TasksArgs, quiet: u8) -> Result<()> {
    // Validate mutual exclusion
    if args.active_only && args.completed_only {
        bail!("--active-only and --completed-only are mutually exclusive");
    }

    // Parse --parameter escape hatch
    let extra_params: Vec<(String, String)> = args
        .parameter
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=').or_else(|| p.split_once(':'))?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    // Check if completed-only requires identifier or task_id
    if args.completed_only
        && args.identifier.is_none()
        && args.task_id.is_none()
        && !extra_params.iter().any(|(k, _)| k == "task_id")
    {
        bail!("--completed-only requires an identifier or task_id parameter");
    }

    // Build query with smart defaults matching Python behavior
    let has_identifier = args.identifier.is_some();
    let query = TasksQuery {
        identifier: args.identifier.clone(),
        cmd: args.cmd.clone(),
        task_id: args.task_id,
        submittime_after: args.since.clone(),
        submittime_before: args.before.clone(),
        submitter: args.submitter.clone(),
        server: args.server.clone(),
        priority: args.priority,
        color: args.color.clone(),
        args: args.args.clone(),
        catalog: if args.completed_only {
            Some(false)
        } else {
            Some(true)
        },
        history: if args.active_only {
            Some(false)
        } else if has_identifier || args.completed_only {
            Some(true)
        } else {
            Some(false)
        },
        summary: if args.no_summary { Some(false) } else { None },
        limit: if args.no_summary { args.limit } else { None },
        extra_params,
    };

    // If bare `ia tasks` (no identifier, no submitter override), get user email from config
    let query = if !has_identifier && query.task_id.is_none() && query.submitter.is_none() {
        let email = client
            .config()
            .cookies
            .get("logged-in-user")
            .map(|e| urlencoding::decode(e).unwrap_or_default().into_owned())
            .filter(|e| !e.is_empty());
        match email {
            Some(email) => TasksQuery {
                submitter: Some(email),
                ..query
            },
            None => bail!("could not determine your email address; run `ia config login` to configure authentication"),
        }
    } else {
        query
    };

    let (summary, mut entries) = tasks::list_tasks(client, &query).await?;

    // Client-side limit (when not using server-side)
    if let Some(limit) = args.limit {
        if !args.no_summary {
            entries.truncate(limit as usize);
        }
    }

    if args.json {
        print_json_listing(&summary, &entries, args)?;
    } else {
        print_table_listing(&summary, &entries, quiet, args.no_summary);
    }

    Ok(())
}

fn print_json_listing(
    summary: &TasksSummary,
    entries: &[TaskEntry],
    args: &TasksArgs,
) -> anyhow::Result<()> {
    if !args.no_summary {
        let summary_json = serde_json::json!({
            "summary": summary,
        });
        println!("{}", serde_json::to_string(&summary_json)?);
    }
    for entry in entries {
        println!("{}", serde_json::to_string(entry)?);
    }
    Ok(())
}

fn print_table_listing(summary: &TasksSummary, entries: &[TaskEntry], quiet: u8, no_summary: bool) {
    let has_counts =
        summary.queued > 0 || summary.running > 0 || summary.error > 0 || summary.paused > 0;

    if has_counts && !no_summary && quiet < 2 {
        let mut parts = Vec::new();
        if summary.queued > 0 {
            parts.push(format!("Queued: {}", style(summary.queued).green()));
        }
        if summary.running > 0 {
            parts.push(format!("Running: {}", style(summary.running).cyan()));
        }
        if summary.error > 0 {
            parts.push(format!("Error: {}", style(summary.error).red()));
        }
        if summary.paused > 0 {
            parts.push(format!("Paused: {}", style(summary.paused).yellow()));
        }
        eprintln!("{}", parts.join("  "));
    }

    if entries.is_empty() {
        if quiet == 0 {
            eprintln!("No tasks found.");
        }
        return;
    }

    if quiet >= 2 {
        return;
    }

    let mut table = Table::new();
    table.load_style(presets::NOTHING);
    table.set_header(vec![
        Cell::new("TASK_ID").fg(TableColor::DarkGrey),
        Cell::new("IDENTIFIER").fg(TableColor::DarkGrey),
        Cell::new("CMD").fg(TableColor::DarkGrey),
        Cell::new("STATUS").fg(TableColor::DarkGrey),
        Cell::new("SUBMITTED").fg(TableColor::DarkGrey),
        Cell::new("SERVER").fg(TableColor::DarkGrey),
    ]);

    for entry in entries {
        let status_color = match entry.color.as_str() {
            "green" => TableColor::Green,
            "blue" => TableColor::Cyan,
            "red" => TableColor::Red,
            "brown" => TableColor::Yellow,
            "done" => TableColor::DarkGrey,
            _ => TableColor::White,
        };
        let status_label = match entry.color.as_str() {
            "green" => "queued",
            "blue" => "running",
            "red" => "error",
            "brown" => "paused",
            "done" => "done",
            other => other,
        };

        table.add_row(vec![
            Cell::new(entry.task_id),
            Cell::new(&entry.identifier),
            Cell::new(&entry.cmd),
            Cell::new(status_label).fg(status_color),
            Cell::new(&entry.submittime),
            Cell::new(&entry.server),
        ]);
    }

    println!("{table}");
}

// ─── Log ─────────────────────────────────────────────────────────────────────

async fn run_log(client: &IaClient, args: LogArgs) -> Result<()> {
    let log_text = tasks::get_task_log(client, args.task_id).await?;

    if args.json {
        let output = serde_json::json!({
            "task_id": args.task_id,
            "log": log_text,
        });
        println!("{}", serde_json::to_string(&output)?);
    } else if std::io::stdout().is_terminal() {
        // Light colorization for TTY
        for line in log_text.lines() {
            if line.contains("error") || line.contains("ERROR") || line.contains("FATAL") {
                println!("{}", style(line).red());
            } else if line.contains("Task started at:") || line.contains("Task finished at:") {
                println!("{}", style(line).cyan());
            } else if line.starts_with("---") {
                println!("{}", style(line).dim());
            } else {
                println!("{line}");
            }
        }
    } else {
        // Raw passthrough for pipes/redirects
        print!("{log_text}");
    }

    Ok(())
}

// ─── Rate Limit ──────────────────────────────────────────────────────────────

async fn run_rate_limit(client: &IaClient, args: RateLimitArgs) -> Result<()> {
    let cmd = if args.cmd.ends_with(".php") {
        args.cmd.clone()
    } else {
        format!("{}.php", args.cmd)
    };

    let info = tasks::get_rate_limit(client, &cmd).await?;

    if args.json {
        println!("{}", serde_json::to_string(&info)?);
    } else {
        println!("Command:        {}", info.cmd);
        println!("Task limit:     {}", info.task_limits);
        println!("In-flight:      {}", info.tasks_inflight);
        println!("Blocked:        {}", info.tasks_blocked_by_offline);
    }

    Ok(())
}

// ─── Submit ──────────────────────────────────────────────────────────────────

/// Joblog `op` for a task submission.
const SUBMIT_OP: &str = "task-submit";

/// The command as the Tasks API receives it: `.php` appended when missing.
fn php_cmd(cmd: &str) -> String {
    if cmd.ends_with(".php") {
        cmd.to_string()
    } else {
        format!("{cmd}.php")
    }
}

/// What identifies a task in the joblog, stored in the entry's `file` field:
/// the command as sent, then, when the task has arguments, a space and the
/// arguments as a JSON object with sorted keys (`derive.php`,
/// `derive.php {"remove_derived":"*.jpg"}`). The comment, priority and raw
/// `-p` parameters are not part of it: they change how a task is queued, not
/// which task it is.
fn task_key(sub: &TaskSubmission) -> String {
    let cmd = php_cmd(&sub.cmd);
    match sub.args.as_ref().filter(|a| !a.is_empty()) {
        None => cmd,
        Some(args) => {
            let sorted: BTreeMap<&String, &String> = args.iter().collect();
            let json = serde_json::to_string(&sorted).unwrap_or_default();
            format!("{cmd} {json}")
        }
    }
}

/// Parse repeatable `KEY=VALUE` (or `KEY:VALUE`) flag values.
fn parse_key_values(values: &[String], flag: &str) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|a| {
            a.split_once('=')
                .or_else(|| a.split_once(':'))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| anyhow::anyhow!("invalid {flag} value (expected KEY=VALUE): {a}"))
        })
        .collect()
}

async fn run_submit(
    client: &IaClient,
    args: SubmitArgs,
    quiet: u8,
    jobs: usize,
    joblog: Option<std::path::PathBuf>,
    no_resume: bool,
) -> Result<()> {
    let task_args = parse_key_values(&args.task_args, "--args")?;
    let extra_params = parse_key_values(&args.parameter, "--parameter")?;

    let mut submissions = match args.spreadsheet {
        Some(ref sheet) => spreadsheet_submissions(sheet, &args, &task_args, &extra_params)?,
        None => identifier_submissions(client, &args, &task_args, &extra_params).await?,
    };

    if let (false, Some(path)) = (no_resume, joblog.as_deref()) {
        let skipped = skip_submitted(&mut submissions, path)?;
        if skipped > 0 && quiet == 0 {
            eprintln!(
                "{} skipping {} already-submitted task(s) from joblog",
                style("resuming:").cyan().bold(),
                skipped,
            );
        }
    }

    // The input was not empty (each mode refuses that), so an empty list
    // here means the joblog records every task as submitted. With --wait
    // there is no task of this run to poll, and exit 0 would claim it
    // completed.
    if submissions.is_empty() {
        if args.wait {
            bail!(
                "--wait has nothing to poll: the joblog records every task as already \
                 submitted. Check it with `ia tasks <identifier>`, or rerun with --no-resume \
                 to submit it again."
            );
        }
        return Ok(());
    }

    if args.wait && submissions.len() > 1 {
        bail!(
            "--wait is not supported in batch mode (got {} tasks). \
             Submit individually with --wait, or use scripting to poll each task.",
            submissions.len()
        );
    }

    if args.dry_run {
        return print_dry_run(&submissions, &args, quiet);
    }

    let writer = joblog
        .as_deref()
        .map(JoblogWriter::open)
        .transpose()
        .context("failed to open joblog")?;

    if submissions.len() == 1 && args.spreadsheet.is_none() {
        let sub = submissions.remove(0);
        return submit_single(client, &sub, &args, quiet, writer.as_ref()).await;
    }
    submit_batch(client, submissions, &args, quiet, jobs, writer.as_ref()).await
}

/// One submission per identifier from the positional argument, `--itemlist`
/// or `--search`.
async fn identifier_submissions(
    client: &IaClient,
    args: &SubmitArgs,
    task_args: &[(String, String)],
    extra_params: &[(String, String)],
) -> Result<Vec<TaskSubmission>> {
    let Some(ref cmd) = args.cmd else {
        bail!("task command is required (e.g. derive, make_dark)");
    };
    let task_args: HashMap<String, String> = task_args.iter().cloned().collect();

    let positional: Vec<String> = args.identifier.iter().cloned().collect();
    let search_opts = ia_core::search::SearchOpts {
        params: crate::commands::search::parse_extra_params(&args.search_parameters)?,
        ..ia_core::search::SearchOpts::default()
    };
    let search = args.search.as_deref().map(|q| (q, &search_opts));
    let identifiers = crate::identifier::collect_identifiers(
        &positional,
        args.itemlist.as_deref(),
        search,
        client,
    )
    .await?;
    if identifiers.is_empty() {
        bail!("no identifiers provided");
    }

    Ok(identifiers
        .into_iter()
        .map(|identifier| TaskSubmission {
            identifier,
            cmd: cmd.clone(),
            args: (!task_args.is_empty()).then(|| task_args.clone()),
            comment: args.comment.clone(),
            priority: (args.priority != 0).then_some(args.priority),
            reduced_priority: args.reduced_priority,
            extra_params: extra_params.to_vec(),
        })
        .collect())
}

/// One submission per spreadsheet row.
///
/// Expected columns: `identifier`, `cmd`, `comment` (optional), `priority` (optional).
/// Task arguments use indexed columns with `args.` prefix (e.g. `args.remove_derived`).
/// `--args` values override a row's, and `-p` values apply to every row.
fn spreadsheet_submissions(
    spreadsheet_path: &Path,
    args: &SubmitArgs,
    cli_task_args: &[(String, String)],
    extra_params: &[(String, String)],
) -> Result<Vec<TaskSubmission>> {
    let records = read_spreadsheet(spreadsheet_path).context(format!(
        "failed to read spreadsheet: {}",
        spreadsheet_path.display()
    ))?;

    if records.is_empty() {
        bail!("spreadsheet contains no records");
    }

    let mut submissions: Vec<TaskSubmission> = Vec::new();
    for (identifier, fields) in &records {
        let cmd = fields.get("cmd").cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "spreadsheet row for '{}' is missing required 'cmd' column",
                identifier
            )
        })?;

        let mut task_args: HashMap<String, String> = fields
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("args.")
                    .filter(|_| !v.is_empty())
                    .map(|arg_name| (arg_name.to_string(), v.clone()))
            })
            .collect();
        task_args.extend(cli_task_args.iter().cloned());

        let comment = fields
            .get("comment")
            .cloned()
            .or_else(|| args.comment.clone());

        let priority = fields
            .get("priority")
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or(args.priority);

        submissions.push(TaskSubmission {
            identifier: identifier.clone(),
            cmd,
            args: (!task_args.is_empty()).then_some(task_args),
            comment,
            priority: (priority != 0).then_some(priority),
            reduced_priority: args.reduced_priority,
            extra_params: extra_params.to_vec(),
        });
    }
    Ok(submissions)
}

/// Drop the submissions the joblog records as accepted, and return how many
/// were dropped.
///
/// A task is done when the joblog has an `ok` `task-submit` entry for its
/// item whose `file` is the task's key (see [`task_key`]). An `ok` entry with
/// an empty `file` was written by a version that did not record the task; it
/// covers every task for that item, so an older joblog never causes a task
/// to be submitted twice. Any `ok` entry counts, not only the latest: a task
/// IA accepted is not sent again from the same joblog.
fn skip_submitted(submissions: &mut Vec<TaskSubmission>, path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let entries =
        joblog::read(path).context(format!("failed to read joblog: {}", path.display()))?;
    let done: HashSet<(&str, &str)> = entries
        .iter()
        .filter(|e| e.op == SUBMIT_OP && e.status == "ok")
        .map(|e| (e.item.as_str(), e.file.as_str()))
        .collect();
    if done.is_empty() {
        return Ok(0);
    }
    let before = submissions.len();
    submissions.retain(|s| {
        let item = s.identifier.as_str();
        !done.contains(&(item, "")) && !done.contains(&(item, task_key(s).as_str()))
    });
    Ok(before - submissions.len())
}

/// Write one joblog entry for a finished submission.
fn record(
    writer: Option<&JoblogWriter>,
    sub: &TaskSubmission,
    result: &Result<tasks::TaskSubmitResponse>,
    elapsed: Duration,
) {
    let Some(writer) = writer else {
        return;
    };
    let entry = JoblogEntry::new(SUBMIT_OP, &sub.identifier, &task_key(sub));
    let entry = match result {
        Ok(_) => entry.ok(0, elapsed.as_millis() as u64),
        Err(e) => entry.error(&e.to_string(), 0),
    };
    writer.write(&entry);
}

fn print_dry_run(submissions: &[TaskSubmission], args: &SubmitArgs, quiet: u8) -> Result<()> {
    for sub in submissions {
        let cmd_display = php_cmd(&sub.cmd);
        let sorted_args: Option<BTreeMap<&String, &String>> =
            sub.args.as_ref().map(|a| a.iter().collect());
        if args.json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "identifier": sub.identifier,
                    "cmd": cmd_display,
                    "args": sorted_args.unwrap_or_default(),
                    "comment": sub.comment,
                    "priority": sub.priority.unwrap_or(0),
                    "reduced_priority": sub.reduced_priority,
                    "dry_run": true,
                }))?
            );
        } else {
            let args_display = match sorted_args {
                Some(a) if !a.is_empty() => format!(
                    " ({})",
                    a.iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                _ => String::new(),
            };
            eprintln!(
                "{} {} → {}{}",
                style("dry-run").dim(),
                sub.identifier,
                cmd_display,
                args_display
            );
        }
    }
    if !args.json && quiet < 2 {
        eprintln!(
            "\n{} would submit {} task(s)",
            style("dry-run").dim(),
            submissions.len()
        );
    }
    Ok(())
}

/// Submit one task given by identifier, record it, and wait on it with `--wait`.
async fn submit_single(
    client: &IaClient,
    sub: &TaskSubmission,
    args: &SubmitArgs,
    quiet: u8,
    writer: Option<&JoblogWriter>,
) -> Result<()> {
    let start = Instant::now();
    let result = submit_with_retry(client, sub, args.max_retries, quiet).await;
    record(writer, sub, &result, start.elapsed());
    let result = result?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "value": result,
            }))?
        );
    } else if quiet == 0 {
        eprintln!("Task submitted: {}", style(result.task_id).cyan());
        eprintln!("Log: {}", style(&result.log).dim());
    }

    if args.wait {
        wait_on_task(client, result.task_id, args, quiet).await?;
    }
    Ok(())
}

/// Submit tasks concurrently. Each result is printed and recorded in the
/// joblog as it arrives, so an interrupted run keeps a record of every task
/// IA accepted before the interruption.
async fn submit_batch(
    client: &IaClient,
    submissions: Vec<TaskSubmission>,
    args: &SubmitArgs,
    quiet: u8,
    jobs: usize,
    writer: Option<&JoblogWriter>,
) -> Result<()> {
    let max_retries = args.max_retries;
    let mut results = stream::iter(submissions)
        .map(|sub| async move {
            let start = Instant::now();
            let result = submit_with_retry(client, &sub, max_retries, quiet).await;
            (sub, result, start.elapsed())
        })
        .buffer_unordered(jobs);

    let mut succeeded = 0u32;
    let mut failed = 0u32;
    let mut accepted = Vec::new();

    while let Some((sub, result, elapsed)) = results.next().await {
        record(writer, &sub, &result, elapsed);
        let id = &sub.identifier;
        match result {
            Ok(resp) => {
                succeeded += 1;
                accepted.push(resp.task_id);
                if args.json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "identifier": id,
                            "task_id": resp.task_id,
                            "success": true,
                        }))?
                    );
                } else if quiet == 0 {
                    eprintln!("{} {} (task {})", style("ok").green(), id, resp.task_id);
                }
            }
            Err(e) => {
                failed += 1;
                if args.json {
                    let json_err = serde_json::json!({
                        "identifier": id,
                        "success": false,
                        "error": e.to_string(),
                    });
                    eprintln!("{}", serde_json::to_string(&json_err)?);
                } else {
                    eprintln!("{} {}: {}", style("error").red(), id, e);
                }
            }
        }
    }

    if !args.json && quiet < 2 {
        eprintln!(
            "\n{} submitted, {} failed",
            style(succeeded).green(),
            if failed > 0 {
                style(failed).red().to_string()
            } else {
                style(failed).dim().to_string()
            }
        );
    }

    if failed > 0 {
        std::process::exit(1);
    }

    // `--wait` is refused for more than one task, so this is the one task of
    // a one-row spreadsheet.
    if args.wait {
        if let [task_id] = accepted[..] {
            wait_on_task(client, task_id, args, quiet).await?;
        }
    }

    Ok(())
}

/// Poll a submitted task until it leaves the catalog (completed) or turns red
/// (failed, which is an error).
async fn wait_on_task(client: &IaClient, task_id: u64, args: &SubmitArgs, quiet: u8) -> Result<()> {
    let spinner = if quiet == 0 && !args.json {
        let sp = ProgressBar::new_spinner();
        sp.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        sp.enable_steady_tick(Duration::from_millis(120));
        sp.set_message(format!("Task {task_id} — waiting"));
        Some(sp)
    } else {
        None
    };

    let failed = tasks::wait_for_task_with_progress(
        client,
        task_id,
        Duration::from_secs(args.wait_interval),
        None,
        |progress| {
            let Some(ref sp) = spinner else {
                return;
            };
            match progress {
                tasks::TaskWaitProgress::Active(entry) => {
                    let state = match entry.color.as_str() {
                        "green" => "queued",
                        "blue" => "running",
                        "brown" => "paused",
                        other => other,
                    };
                    sp.set_message(format!("Task {task_id} — {state}"));
                }
                tasks::TaskWaitProgress::RateLimited { wait, .. } => {
                    sp.set_message(format!(
                        "Task {task_id} — rate limited, polling again in {}s",
                        wait.as_secs()
                    ));
                }
                _ => {}
            }
        },
    )
    .await;
    let failed = match failed {
        Ok(failed) => failed,
        Err(e) => {
            if let Some(ref sp) = spinner {
                sp.abandon();
            }
            return Err(e.into());
        }
    };

    match failed {
        None => {
            if let Some(ref sp) = spinner {
                sp.finish_with_message(format!("Task {task_id} — completed"));
            }
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "task_id": task_id,
                        "status": "completed",
                    }))?
                );
            } else if quiet == 0 {
                eprintln!("Task {task_id} completed");
            }
            Ok(())
        }
        Some(entry) => {
            if let Some(ref sp) = spinner {
                sp.finish_with_message(format!("Task {task_id} — error"));
            }
            if args.json {
                println!("{}", serde_json::to_string(&entry)?);
            }
            bail!("task {task_id} failed (error state); see ia tasks log {task_id}");
        }
    }
}

/// Retry submit on 429 rate-limit responses from the Tasks API.
///
/// Note: the reqwest-middleware retry stack may also retry 429s before the
/// response reaches `submit_task`. This loop handles cases where the middleware
/// exhausts its own attempts and the 429 surfaces as `IaError::RateLimited`.
async fn submit_with_retry(
    client: &IaClient,
    submission: &TaskSubmission,
    max_retries: u32,
    quiet: u8,
) -> Result<tasks::TaskSubmitResponse> {
    let mut retries = 0u32;
    let mut backoff = Duration::from_secs(2);
    let max_backoff = Duration::from_secs(60);

    loop {
        match tasks::submit_task(client, submission).await {
            Ok(result) => return Ok(result),
            Err(ia_core::error::IaError::RateLimited { retry_after }) => {
                retries += 1;
                if retries > max_retries {
                    bail!("max retries ({max_retries}) exceeded for rate limiting");
                }

                // Honor Retry-After header if present, otherwise exponential backoff
                let wait = if retry_after > 0 {
                    Duration::from_secs(retry_after)
                } else {
                    backoff
                };
                if quiet == 0 {
                    eprintln!(
                        "Rate limited for {}. Retrying in {}s... ({retries}/{max_retries})",
                        submission.identifier,
                        wait.as_secs()
                    );
                }
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ─── Rerun ───────────────────────────────────────────────────────────────────

async fn run_rerun(client: &IaClient, args: RerunArgs, quiet: u8, jobs: usize) -> Result<()> {
    let task_ids = collect_rerun_task_ids(&args, client).await?;

    if task_ids.is_empty() {
        bail!("no task IDs provided");
    }

    let client = client.clone();
    let max_retries = args.max_retries;
    let json_output = args.json;

    let results: Vec<(u64, Result<String>)> = stream::iter(task_ids.clone())
        .map(|task_id| {
            let client = client.clone();
            async move {
                let result = rerun_with_retry(&client, task_id, max_retries, quiet).await;
                (task_id, result)
            }
        })
        .buffer_unordered(jobs)
        .collect()
        .await;

    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for (task_id, result) in &results {
        match result {
            Ok(identifier) => {
                succeeded += 1;
                if json_output {
                    println!(
                        "{}",
                        serde_json::json!({
                            "task_id": task_id,
                            "identifier": identifier,
                            "success": true,
                        })
                    );
                } else if quiet == 0 {
                    eprintln!(
                        "{} Rerun task {task_id} ({})",
                        style("ok").green(),
                        identifier
                    );
                }
            }
            Err(e) => {
                failed += 1;
                if json_output {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "task_id": task_id,
                            "success": false,
                            "error": e.to_string(),
                        })
                    );
                } else {
                    eprintln!("{} Task {task_id}: {e}", style("error").red());
                }
            }
        }
    }

    if task_ids.len() > 1 && !json_output && quiet < 2 {
        eprintln!(
            "\n{} rerun, {} failed",
            style(succeeded).green(),
            if failed > 0 {
                style(failed).red().to_string()
            } else {
                style(failed).dim().to_string()
            }
        );
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Retry rerun on 429 rate-limit responses (see `submit_with_retry` for details).
async fn rerun_with_retry(
    client: &IaClient,
    task_id: u64,
    max_retries: u32,
    quiet: u8,
) -> Result<String> {
    let mut retries = 0u32;
    let mut backoff = Duration::from_secs(2);
    let max_backoff = Duration::from_secs(60);

    loop {
        match tasks::rerun_task(client, task_id).await {
            Ok(identifier) => return Ok(identifier),
            Err(ia_core::error::IaError::RateLimited { retry_after }) => {
                retries += 1;
                if retries > max_retries {
                    bail!("max retries ({max_retries}) exceeded for task {task_id}");
                }

                let wait = if retry_after > 0 {
                    Duration::from_secs(retry_after)
                } else {
                    backoff
                };
                if quiet == 0 {
                    eprintln!(
                        "Rate limited for task {task_id}. Retrying in {}s... ({retries}/{max_retries})",
                        wait.as_secs()
                    );
                }
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Read task IDs from stdin (shared between explicit `-` and auto-detect).
fn read_task_ids_from_stdin(ids: &mut Vec<u64>) -> Result<()> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Ok(id) = trimmed.parse::<u64>() {
            ids.push(id);
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(id) = value.get("task_id").and_then(|v| v.as_u64()) {
                ids.push(id);
                continue;
            }
        }
        tracing::debug!("skipping unrecognized stdin line: {trimmed}");
    }
    Ok(())
}

async fn collect_rerun_task_ids(args: &RerunArgs, client: &IaClient) -> Result<Vec<u64>> {
    let mut ids = Vec::new();

    for arg in &args.task_ids {
        if arg == "-" {
            eprintln!(
                "{}: explicit '-' for stdin is deprecated; stdin is auto-detected when piped",
                style("warning").yellow()
            );
            read_task_ids_from_stdin(&mut ids)?;
        } else {
            ids.push(
                arg.parse::<u64>()
                    .context(format!("invalid task ID: {arg}"))?,
            );
        }
    }

    let has_filters = args.cmd.is_some()
        || args.submitter.is_some()
        || args.identifier.is_some()
        || args.color.is_some();

    // Auto-detect stdin when piped and no IDs or filters provided
    if ids.is_empty() && args.task_ids.is_empty() && !has_filters && !std::io::stdin().is_terminal()
    {
        read_task_ids_from_stdin(&mut ids)?;
    }

    if has_filters && ids.is_empty() {
        let query = TasksQuery {
            identifier: args.identifier.clone(),
            cmd: args.cmd.clone(),
            submitter: args.submitter.clone(),
            color: Some(args.color.clone().unwrap_or_else(|| "red".into())),
            catalog: Some(true),
            ..Default::default()
        };
        let (_, entries) = tasks::list_tasks(client, &query).await?;
        for entry in entries {
            ids.push(entry.task_id);
        }
    }

    Ok(ids)
}
