use std::collections::HashMap;
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use comfy_table::{presets, Cell, Color as TableColor, Table};
use console::style;
use futures::{stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};

use ia_core::joblog::{self, JoblogEntry, JoblogWriter};
use ia_core::tasks::{self, TaskEntry, TaskSubmission, TasksQuery, TasksSummary};
use ia_core::IaClient;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
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
         \n  <bold>$ ia tasks submit derive my-item</bold>\
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
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Submit a derive task</dim>\
             \n  <bold>$ ia tasks submit derive my-item</bold>\
             \n\n  <dim># Submit with a comment</dim>\
             \n  <bold>$ ia tasks submit make_dark my-item --comment \"curation request\"</bold>\
             \n\n  <dim># Submit to multiple items</dim>\
             \n  <bold>$ ia tasks submit derive --itemlist items.txt --comment \"re-derive\"</bold>\n"
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
             \n  <bold>$ ia tasks --cmd derive.php --color red --json | ia tasks rerun -</bold>\n"
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
    /// Task command (e.g. derive, make_dark)
    #[arg()]
    pub cmd: String,

    /// Item identifier (omit for batch mode with --itemlist/--search)
    #[arg()]
    pub identifier: Option<String>,

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

    /// Poll until task completes
    #[arg(long)]
    pub wait: bool,

    /// Initial poll interval in seconds for --wait
    #[arg(long, default_value = "2")]
    pub wait_interval: u64,

    /// Max retries on 429 rate-limit responses
    #[arg(long, default_value = "10")]
    pub max_retries: u32,

    /// Raw API parameter KEY=VALUE (repeatable)
    #[arg(short = 'p', long = "parameter")]
    pub parameter: Vec<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print what would be submitted without sending
    #[arg(long)]
    pub dry_run: bool,

    /// Read identifiers from file
    #[arg(long)]
    pub itemlist: Option<std::path::PathBuf>,

    /// Use search results as input
    #[arg(long)]
    pub search: Option<String>,
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
    /// Task ID(s) to rerun (use - for stdin)
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
    retry_failed: bool,
) -> Result<()> {
    match args.command {
        Some(TasksCommand::Submit(submit_args)) => {
            run_submit(client, submit_args, quiet, jobs, joblog, retry_failed).await
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

    // If bare `ia tasks` (no identifier, no submitter override), get user email
    // Auto-fill submitter when bare `ia tasks` (no identifier, no task_id)
    let query = if !has_identifier && query.task_id.is_none() && query.submitter.is_none() {
        let account = ia_core::auth::whoami(client).await?;
        TasksQuery {
            submitter: Some(account.email),
            ..query
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
    table.load_preset(presets::NOTHING);
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

async fn run_submit(
    client: &IaClient,
    args: SubmitArgs,
    quiet: u8,
    jobs: usize,
    joblog: Option<std::path::PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    let mut task_args = HashMap::new();
    for a in &args.task_args {
        let (k, v) = a
            .split_once('=')
            .or_else(|| a.split_once(':'))
            .ok_or_else(|| anyhow::anyhow!("invalid --args value (expected KEY=VALUE): {a}"))?;
        task_args.insert(k.to_string(), v.to_string());
    }

    let mut extra_params = Vec::new();
    for p in &args.parameter {
        let (k, v) = p
            .split_once('=')
            .or_else(|| p.split_once(':'))
            .ok_or_else(|| anyhow::anyhow!("invalid --parameter value (expected KEY=VALUE): {p}"))?;
        extra_params.push((k.to_string(), v.to_string()));
    }

    let mut identifiers = collect_submit_identifiers(&args, client).await?;

    // Handle --retry-failed
    if retry_failed {
        if let Some(ref path) = joblog {
            let entries =
                joblog::read(path).context(format!("failed to read joblog: {}", path.display()))?;
            let failed = joblog::failed_items(&entries);
            if failed.is_empty() {
                if quiet == 0 {
                    eprintln!("{} No failed items in joblog", style("ok").green());
                }
                return Ok(());
            }
            identifiers = failed;
        } else {
            bail!("--retry-failed requires --joblog");
        }
    }

    if identifiers.is_empty() {
        bail!("no identifiers provided");
    }

    if args.wait && identifiers.len() > 1 {
        bail!(
            "--wait is not supported in batch mode (got {} identifiers). \
             Submit individually with --wait, or use scripting to poll each task.",
            identifiers.len()
        );
    }

    if args.dry_run {
        let cmd_display = if args.cmd.ends_with(".php") {
            args.cmd.clone()
        } else {
            format!("{}.php", args.cmd)
        };
        for id in &identifiers {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "identifier": id,
                        "cmd": cmd_display,
                        "args": task_args,
                        "comment": args.comment,
                        "priority": args.priority,
                        "reduced_priority": args.reduced_priority,
                        "dry_run": true,
                    }))?
                );
            } else {
                eprintln!(
                    "{} {} → {}{}",
                    style("dry-run").dim(),
                    id,
                    cmd_display,
                    if task_args.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", task_args.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "))
                    }
                );
            }
        }
        if !args.json && quiet < 2 {
            eprintln!("\n{} would submit {} task(s)", style("dry-run").dim(), identifiers.len());
        }
        return Ok(());
    }

    // Single mode
    if identifiers.len() == 1 {
        let submission = TaskSubmission {
            identifier: identifiers[0].clone(),
            cmd: args.cmd.clone(),
            args: if task_args.is_empty() {
                None
            } else {
                Some(task_args)
            },
            comment: args.comment.clone(),
            priority: if args.priority == 0 {
                None
            } else {
                Some(args.priority)
            },
            reduced_priority: args.reduced_priority,
            extra_params,
        };

        let result = submit_with_retry(client, &submission, args.max_retries, quiet).await?;

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
            let interval_secs = args.wait_interval;
            let max_interval = Duration::from_secs(60);
            let mut interval = Duration::from_secs(interval_secs);

            let spinner = if quiet == 0 && !args.json {
                let sp = ProgressBar::new_spinner();
                sp.set_style(
                    ProgressStyle::with_template("{spinner:.cyan} {msg}")
                        .unwrap()
                        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
                );
                sp.enable_steady_tick(Duration::from_millis(120));
                sp.set_message(format!("Task {} — waiting", result.task_id));
                Some(sp)
            } else {
                None
            };

            loop {
                let query = tasks::TasksQuery {
                    task_id: Some(result.task_id),
                    catalog: Some(true),
                    ..Default::default()
                };

                let value = tasks::get_tasks(client, &query).await?;
                let still_active = value.catalog.iter().any(|e| e.task_id == result.task_id);

                if !still_active {
                    if let Some(ref sp) = spinner {
                        sp.finish_with_message(format!("Task {} — completed", result.task_id));
                    }
                    if args.json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "task_id": result.task_id,
                                "status": "completed",
                            }))?
                        );
                    } else if quiet == 0 {
                        eprintln!("Task {} completed", result.task_id);
                    }
                    break;
                }

                // Update spinner with current state
                if let Some(ref sp) = spinner {
                    let state = value
                        .catalog
                        .iter()
                        .find(|e| e.task_id == result.task_id)
                        .map(|e| match e.color.as_str() {
                            "green" => "queued",
                            "blue" => "running",
                            "red" => "error",
                            "brown" => "paused",
                            other => other,
                        })
                        .unwrap_or("unknown");
                    sp.set_message(format!("Task {} — {}", result.task_id, state));
                }

                // Check if errored — stop waiting
                let is_error = value
                    .catalog
                    .iter()
                    .any(|e| e.task_id == result.task_id && e.color == "red");
                if is_error {
                    if let Some(ref sp) = spinner {
                        sp.finish_with_message(format!("Task {} — error", result.task_id));
                    }
                    if args.json {
                        let entry = value.catalog.iter().find(|e| e.task_id == result.task_id);
                        if let Some(entry) = entry {
                            println!("{}", serde_json::to_string(entry)?);
                        }
                    } else if quiet == 0 {
                        eprintln!("Task {} error", result.task_id);
                    }
                    std::process::exit(1);
                }

                tokio::time::sleep(interval).await;
                interval = (interval * 2).min(max_interval);
            }
        }

        return Ok(());
    }

    // Batch mode — concurrent submission
    let joblog_writer = joblog
        .as_ref()
        .map(|p| JoblogWriter::open(p))
        .transpose()
        .context("failed to open joblog")?;
    let joblog_writer = joblog_writer.map(std::sync::Arc::new);

    let client = client.clone();
    let cmd = args.cmd.clone();
    let comment = args.comment.clone();
    let json_output = args.json;
    let max_retries = args.max_retries;
    let reduced_priority = args.reduced_priority;
    let priority = args.priority;

    let results: Vec<(String, Result<tasks::TaskSubmitResponse>)> = stream::iter(identifiers)
        .map(|id| {
            let client = client.clone();
            let cmd = cmd.clone();
            let task_args = task_args.clone();
            let comment = comment.clone();
            let extra_params = extra_params.clone();
            async move {
                let submission = TaskSubmission {
                    identifier: id.clone(),
                    cmd,
                    args: if task_args.is_empty() {
                        None
                    } else {
                        Some(task_args)
                    },
                    comment,
                    priority: if priority == 0 { None } else { Some(priority) },
                    reduced_priority,
                    extra_params,
                };
                let result = submit_with_retry(&client, &submission, max_retries, quiet).await;
                (id, result)
            }
        })
        .buffer_unordered(jobs)
        .collect()
        .await;

    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for (id, result) in &results {
        match result {
            Ok(resp) => {
                succeeded += 1;
                if json_output {
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
                if let Some(ref writer) = joblog_writer {
                    let entry = JoblogEntry::new("task-submit", id, "").ok(0, 0);
                    writer.write(&entry);
                }
            }
            Err(e) => {
                failed += 1;
                if json_output {
                    let json_err = serde_json::json!({
                        "identifier": id,
                        "success": false,
                        "error": e.to_string(),
                    });
                    eprintln!("{}", serde_json::to_string(&json_err)?);
                } else {
                    eprintln!("{} {}: {}", style("error").red(), id, e);
                }
                if let Some(ref writer) = joblog_writer {
                    let entry = JoblogEntry::new("task-submit", id, "").error(&e.to_string(), 0);
                    writer.write(&entry);
                }
            }
        }
    }

    if !json_output && quiet < 2 {
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

    Ok(())
}

async fn collect_submit_identifiers(args: &SubmitArgs, client: &IaClient) -> Result<Vec<String>> {
    let mut ids = Vec::new();

    if let Some(ref id) = args.identifier {
        ids.push(id.clone());
    }

    if let Some(ref path) = args.itemlist {
        let content = std::fs::read_to_string(path)
            .context(format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                ids.push(trimmed.to_string());
            }
        }
    }

    if let Some(ref query) = args.search {
        let opts = ia_core::search::SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    Ok(ids)
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
                        &submission.identifier,
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

async fn collect_rerun_task_ids(args: &RerunArgs, client: &IaClient) -> Result<Vec<u64>> {
    let mut ids = Vec::new();

    for arg in &args.task_ids {
        if arg == "-" {
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
