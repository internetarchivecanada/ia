use std::collections::HashMap;
use std::time::Duration;

use bytes::BytesMut;
use futures::StreamExt;
use serde::{Deserialize, Deserializer, Serialize};

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// Query parameters for the Tasks API.
#[derive(Debug, Default, Clone)]
pub struct TasksQuery {
    /// Filter by item identifier.
    pub identifier: Option<String>,
    /// Filter by task command name (e.g. `"derive.php"`).
    pub cmd: Option<String>,
    /// Maximum number of results to return.
    pub limit: Option<u32>,
    /// Filter by the user who submitted the task.
    pub submitter: Option<String>,
    /// Filter by task arguments (JSON string match).
    pub args: Option<String>,
    /// Filter by the server executing the task.
    pub server: Option<String>,
    /// Filter by task priority level.
    pub priority: Option<i32>,
    /// Filter by task state color (`"green"`, `"blue"`, `"red"`, `"brown"`).
    pub color: Option<String>,
    /// Look up a specific task by its numeric ID.
    pub task_id: Option<u64>,
    /// Filter by submittime >= value (parseable date/time string).
    pub submittime_after: Option<String>,
    /// Filter by submittime <= value (parseable date/time string).
    pub submittime_before: Option<String>,
    /// Include active (catalog) tasks in the response.
    pub catalog: Option<bool>,
    /// Include completed (history) tasks in the response.
    pub history: Option<bool>,
    /// Include aggregate task counts in the response.
    pub summary: Option<bool>,
    /// Additional query parameters passed through to the API.
    pub extra_params: Vec<(String, String)>,
}

/// Response envelope from the Tasks API.
#[derive(Debug, Clone, Deserialize)]
pub struct TasksResponse {
    pub success: bool,
    pub value: TasksValue,
}

/// The `value` field of the Tasks API response.
#[derive(Debug, Clone, Deserialize)]
pub struct TasksValue {
    pub summary: TasksSummary,
    #[serde(default)]
    pub catalog: Vec<TaskEntry>,
}

/// Aggregate task counts.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct TasksSummary {
    #[serde(default)]
    pub queued: u32,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub error: u32,
    #[serde(default)]
    pub paused: u32,
}

/// Deserialize a JSON `null` or missing field as an empty `String`.
///
/// `#[serde(default)]` alone handles *missing* fields but not explicit `null` values.
/// The IA Tasks API returns `"server": null` for queued tasks (and potentially other
/// nullable string fields), so we need this to avoid skipping those entries.
fn nullable_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// A single task entry from the catalog or history.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskEntry {
    /// Unique numeric task identifier.
    pub task_id: u64,
    /// Item identifier this task operates on.
    pub identifier: String,
    /// Task command (e.g. `"derive.php"`, `"fixer.php"`).
    pub cmd: String,
    /// Email of the user who submitted the task.
    #[serde(default, deserialize_with = "nullable_string")]
    pub submitter: String,
    /// When the task was submitted (ISO-8601 or epoch string).
    #[serde(default, deserialize_with = "nullable_string")]
    pub submittime: String,
    /// Server executing the task (empty if queued).
    #[serde(default, deserialize_with = "nullable_string")]
    pub server: String,
    /// Task state: `"green"` (queued), `"blue"` (running), `"red"` (error), `"brown"` (paused).
    #[serde(default, deserialize_with = "nullable_string")]
    pub color: String,
    /// Execution priority (higher = sooner, negative = deprioritized).
    #[serde(default)]
    pub priority: i32,
    /// Task-specific arguments as freeform JSON.
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    /// Entry source: `"catalog"` (active) or `"history"` (completed).
    #[serde(default)]
    pub category: Option<String>,
    /// Completion timestamp (epoch seconds), present only for history entries.
    #[serde(default)]
    pub finished: Option<u64>,
}

/// Task submission request body.
#[derive(Debug, Clone)]
pub struct TaskSubmission {
    /// Item identifier to run the task against.
    pub identifier: String,
    /// Task command name (`.php` suffix appended automatically if missing).
    pub cmd: String,
    /// Task arguments as key-value pairs.
    pub args: Option<HashMap<String, String>>,
    /// Comment merged into args as `args["comment"]`.
    pub comment: Option<String>,
    /// Task priority (default determined by server).
    pub priority: Option<i32>,
    /// Send `X-Accept-Reduced-Priority: 1` header to avoid rate-limit errors.
    pub reduced_priority: bool,
    /// Additional JSON fields merged into the request body.
    pub extra_params: Vec<(String, String)>,
}

/// Response from a successful task submission.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskSubmitResponse {
    /// Numeric ID of the newly created task.
    pub task_id: u64,
    /// Server log message from task creation.
    pub log: String,
}

/// Rate limit information for a task command.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitInfo {
    /// The task command these limits apply to.
    pub cmd: String,
    /// Maximum concurrent tasks allowed for this command.
    pub task_limits: u32,
    /// Number of tasks currently running for this command.
    pub tasks_inflight: u32,
    /// Tasks blocked because target servers are offline.
    pub tasks_blocked_by_offline: u32,
}

/// Build query parameters from a `TasksQuery`, adding auth header.
fn build_tasks_request(
    client: &IaClient,
    query: &TasksQuery,
) -> Result<reqwest_middleware::RequestBuilder> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let mut req = client
        .http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"));

    if let Some(ref id) = query.identifier {
        req = req.query(&[("identifier", id.as_str())]);
    }
    if let Some(ref cmd) = query.cmd {
        req = req.query(&[("cmd", cmd.as_str())]);
    }
    if let Some(limit) = query.limit {
        req = req.query(&[("limit", &limit.to_string())]);
    }
    if let Some(ref submitter) = query.submitter {
        req = req.query(&[("submitter", submitter.as_str())]);
    }
    if let Some(ref args) = query.args {
        req = req.query(&[("args", args.as_str())]);
    }
    if let Some(ref server) = query.server {
        req = req.query(&[("server", server.as_str())]);
    }
    if let Some(priority) = query.priority {
        req = req.query(&[("priority", &priority.to_string())]);
    }
    if let Some(ref color) = query.color {
        req = req.query(&[("color", color.as_str())]);
    }
    if let Some(task_id) = query.task_id {
        req = req.query(&[("task_id", &task_id.to_string())]);
    }
    if let Some(ref after) = query.submittime_after {
        req = req.query(&[("submittime>=", after.as_str())]);
    }
    if let Some(ref before) = query.submittime_before {
        req = req.query(&[("submittime<=", before.as_str())]);
    }
    if let Some(catalog) = query.catalog {
        req = req.query(&[("catalog", if catalog { "1" } else { "0" })]);
    }
    if let Some(history) = query.history {
        req = req.query(&[("history", if history { "1" } else { "0" })]);
    }
    if let Some(summary) = query.summary {
        req = req.query(&[("summary", if summary { "1" } else { "0" })]);
    }
    for (key, value) in &query.extra_params {
        req = req.query(&[(key.as_str(), value.as_str())]);
    }

    Ok(req)
}

/// Fetch tasks from the archive.org Tasks API (read-only).
pub async fn get_tasks(client: &IaClient, query: &TasksQuery) -> Result<TasksValue> {
    let req = build_tasks_request(client, query)?;
    let resp = req.send().await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let parsed: TasksResponse = serde_json::from_str(&body)?;
    if !parsed.success {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: "Tasks API returned success: false".into(),
        });
    }
    Ok(parsed.value)
}

/// Parse a single JSONL value into summary or entries.
fn parse_jsonl_value(
    value: serde_json::Value,
    summary: &mut TasksSummary,
    entries: &mut Vec<TaskEntry>,
) {
    match value.get("category").and_then(|c| c.as_str()) {
        Some("summary") => {
            if let Ok(s) = serde_json::from_value::<TasksSummary>(value) {
                *summary = s;
            }
        }
        Some("catalog") | Some("history") => match serde_json::from_value::<TaskEntry>(value) {
            Ok(entry) => entries.push(entry),
            Err(e) => tracing::debug!("skipping malformed task entry: {e}"),
        },
        _ => {
            tracing::debug!("skipping line with unknown category");
        }
    }
}

/// List tasks using streaming JSONL (`limit=0`).
///
/// Parses the response line-by-line. Summary lines are merged into
/// `TasksSummary`. Catalog and history entries are collected into a flat
/// `Vec<TaskEntry>`. Malformed lines are skipped with a debug log.
pub async fn list_tasks(
    client: &IaClient,
    query: &TasksQuery,
) -> Result<(TasksSummary, Vec<TaskEntry>)> {
    let mut streaming_query = query.clone();
    if streaming_query.limit.is_none() {
        streaming_query.limit = Some(0);
    }

    let req = build_tasks_request(client, &streaming_query)?;
    let resp = req.send().await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let mut stream = resp.bytes_stream();
    let mut buf = BytesMut::new();
    let mut summary = TasksSummary::default();
    let mut entries = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| IaError::Http {
            status: 0,
            message: format!("stream error: {e}"),
        })?;
        buf.extend_from_slice(&chunk);

        // Process complete lines
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes = buf.split_to(pos + 1);
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("skipping malformed JSONL line: {e}");
                    continue;
                }
            };

            parse_jsonl_value(value, &mut summary, &mut entries);
        }
    }

    // Process any remaining data in buffer (last line without trailing newline)
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if !line.is_empty() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                parse_jsonl_value(value, &mut summary, &mut entries);
            }
        }
    }

    Ok((summary, entries))
}

/// Submit a task to the Tasks API (POST).
///
/// The `comment` field is merged into `args["comment"]` before sending.
/// Automatically appends `.php` to `cmd` if not already present.
pub async fn submit_task(
    client: &IaClient,
    submission: &TaskSubmission,
) -> Result<TaskSubmitResponse> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let cmd = if submission.cmd.ends_with(".php") {
        submission.cmd.clone()
    } else {
        format!("{}.php", submission.cmd)
    };

    let mut args_map: HashMap<String, String> = submission.args.clone().unwrap_or_default();
    if let Some(ref comment) = submission.comment {
        args_map.insert("comment".into(), comment.clone());
    }

    let mut body = serde_json::json!({
        "identifier": submission.identifier,
        "cmd": cmd,
    });
    if !args_map.is_empty() {
        body["args"] = serde_json::to_value(&args_map)?;
    }
    if let Some(priority) = submission.priority {
        body["priority"] = serde_json::json!(priority);
    }
    for (key, value) in &submission.extra_params {
        body[key] = serde_json::json!(value);
    }

    let mut req = client
        .http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body)?);

    if submission.reduced_priority {
        req = req.header("X-Accept-Reduced-Priority", "1");
    }

    let resp = req.send().await?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        return Err(IaError::RateLimited { retry_after });
    }
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body_text = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let parsed: serde_json::Value = serde_json::from_str(&body_text)?;
    if parsed.get("success").and_then(|s| s.as_bool()) != Some(true) {
        let msg = parsed
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(IaError::TaskSubmitFailed { message: msg });
    }

    let value = parsed
        .get("value")
        .ok_or_else(|| IaError::TaskSubmitFailed {
            message: "response missing value field".into(),
        })?;
    let response: TaskSubmitResponse = serde_json::from_value(value.clone())?;
    Ok(response)
}

/// Rerun a failed task (PUT).
///
/// Sends `{"op": "rerun", "task_id": N}` to the Tasks API.
/// Returns the identifier associated with the rerun task on success,
/// or `IaError::TaskRerunFailed` if the API reports `success: false`.
pub async fn rerun_task(client: &IaClient, task_id: u64) -> Result<String> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let body = serde_json::json!({
        "op": "rerun",
        "task_id": task_id,
    });

    let resp = client
        .http()
        .put(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .send()
        .await?;

    let status = resp.status();
    if status.as_u16() == 429 {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        return Err(IaError::RateLimited { retry_after });
    }
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body_text = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let parsed: serde_json::Value = serde_json::from_str(&body_text)?;
    if parsed.get("success").and_then(|s| s.as_bool()) != Some(true) {
        let msg = parsed
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(IaError::TaskRerunFailed {
            task_id,
            message: msg,
        });
    }

    // API returns {"success": true, "value": {"task_id": "identifier"}}
    let identifier = parsed
        .get("value")
        .and_then(|v| v.get(task_id.to_string()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            tracing::warn!(task_id, "rerun response missing identifier in value field");
            task_id.to_string()
        });

    Ok(identifier)
}

/// Fetch a task's execution log.
///
/// For production (archive.org host), hits `catalogd.archive.org` directly.
/// For other hosts (tests), uses the client's configured host.
/// Returns the raw log text.
pub async fn get_task_log(client: &IaClient, task_id: u64) -> Result<String> {
    let (access, secret) = client.require_auth()?;

    let url = if client.host().contains("archive.org") {
        format!(
            "{}://catalogd.archive.org/services/tasks.php",
            client.protocol()
        )
    } else {
        client.url("/services/tasks.php")
    };

    let resp = client
        .http()
        .get(&url)
        .query(&[("task_log", &task_id.to_string())])
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(IaError::TaskNotFound { task_id });
    }
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let text = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    Ok(text)
}

/// Check rate limits for a task command.
///
/// Queries the Tasks API with `rate_limits=1&cmd=<cmd>` and returns
/// the parsed [`RateLimitInfo`]. Handles both standard JSON envelope
/// and JSONL response formats.
pub async fn get_rate_limit(client: &IaClient, cmd: &str) -> Result<RateLimitInfo> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    let resp = client
        .http()
        .get(&url)
        .query(&[("rate_limits", "1"), ("cmd", cmd)])
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body = resp.text().await.map_err(reqwest_middleware::Error::from)?;

    // Try standard envelope first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
        if parsed.get("success").and_then(|s| s.as_bool()) == Some(true) {
            if let Some(value) = parsed.get("value") {
                return Ok(serde_json::from_value(value.clone())?);
            }
        }
    }

    // Fall back to JSONL parsing
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(info) = serde_json::from_str::<RateLimitInfo>(line) {
            return Ok(info);
        }
    }

    Err(IaError::Http {
        status: 200,
        message: "failed to parse rate limit response".into(),
    })
}

/// Poll a task until it completes (leaves the catalog).
///
/// Uses exponential backoff from `initial_interval`, capped at 60s.
/// Returns when the task is no longer in the active catalog (completed
/// or errored). No total timeout — caller can cancel via Ctrl+C.
pub async fn wait_for_task(
    client: &IaClient,
    task_id: u64,
    initial_interval: Duration,
) -> Result<Option<TaskEntry>> {
    let max_interval = Duration::from_secs(60);
    let mut interval = initial_interval;

    loop {
        let query = TasksQuery {
            task_id: Some(task_id),
            catalog: Some(true),
            ..Default::default()
        };

        let value = get_tasks(client, &query).await?;
        let still_active = value.catalog.iter().any(|e| e.task_id == task_id);

        if !still_active {
            return Ok(None);
        }

        // Log current state for debugging
        if let Some(entry) = value.catalog.iter().find(|e| e.task_id == task_id) {
            tracing::debug!(
                "task {} still active (color={}), polling in {}s",
                task_id,
                entry.color,
                interval.as_secs()
            );
        }

        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(max_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        config
    }

    #[tokio::test]
    async fn test_get_tasks_by_identifier() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("identifier", "my-test-item"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 3,
                        "running": 1,
                        "error": 0,
                        "paused": 0
                    },
                    "catalog": [
                        {
                            "task_id": 123456789,
                            "identifier": "my-test-item",
                            "cmd": "derive.php",
                            "submitter": "user@example.com",
                            "submittime": "2026-03-09 12:00:00",
                            "server": "ia600100.us.archive.org",
                            "color": "green",
                            "priority": 10
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            identifier: Some("my-test-item".to_string()),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();

        // Verify summary
        assert_eq!(result.summary.queued, 3);
        assert_eq!(result.summary.running, 1);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);

        // Verify catalog entry
        assert_eq!(result.catalog.len(), 1);
        let entry = &result.catalog[0];
        assert_eq!(entry.task_id, 123456789);
        assert_eq!(entry.identifier, "my-test-item");
        assert_eq!(entry.cmd, "derive.php");
        assert_eq!(entry.submitter, "user@example.com");
        assert_eq!(entry.submittime, "2026-03-09 12:00:00");
        assert_eq!(entry.server, "ia600100.us.archive.org");
        assert_eq!(entry.color, "green");
        assert_eq!(entry.priority, 10);
    }

    #[tokio::test]
    async fn test_get_tasks_empty_catalog() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 0,
                        "running": 0,
                        "error": 0,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await.unwrap();

        assert_eq!(result.summary.queued, 0);
        assert_eq!(result.summary.running, 0);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);
        assert!(result.catalog.is_empty());
    }

    #[tokio::test]
    async fn test_get_tasks_with_cmd_and_limit() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("cmd", "derive.php"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 10,
                        "running": 2
                    },
                    "catalog": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            cmd: Some("derive.php".to_string()),
            limit: Some(5),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();

        // Summary fields with serde(default) should handle missing fields
        assert_eq!(result.summary.queued, 10);
        assert_eq!(result.summary.running, 2);
        assert_eq!(result.summary.error, 0);
        assert_eq!(result.summary.paused, 0);
        assert!(result.catalog.is_empty());
    }

    #[tokio::test]
    async fn test_get_tasks_http_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status, message } => {
                assert_eq!(status, 403);
                assert_eq!(message, "Forbidden");
            }
            other => panic!("expected Http error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_get_tasks_requires_auth() {
        let mut config = crate::config::IaConfig::default();
        config.general.host = "localhost".to_string();
        config.general.secure = false;
        // No s3_access / s3_secret set
        let client = IaClient::from_config(config).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
    }

    #[tokio::test]
    async fn test_get_tasks_with_submitter_and_args() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("submitter", "user@example.com"))
            .and(query_param("args", "*s3-put*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {
                        "queued": 5,
                        "running": 2,
                        "error": 1,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            submitter: Some("user@example.com".to_string()),
            args: Some("*s3-put*".to_string()),
            ..Default::default()
        };

        let result = get_tasks(&client, &query).await.unwrap();
        assert_eq!(result.summary.queued, 5);
        assert_eq!(result.summary.running, 2);
        assert_eq!(result.summary.error, 1);
    }

    #[tokio::test]
    async fn test_list_tasks_streaming_jsonl() {
        let mock_server = MockServer::start().await;
        let body = [
            r#"{"category":"summary","queued":2,"running":1,"error":0,"paused":0}"#,
            r#"{"category":"catalog","task_id":111,"identifier":"item-a","cmd":"derive.php","submitter":"user@example.com","submittime":"2026-03-10 12:00:00","server":"ia600100.us.archive.org","color":"green","priority":0}"#,
            r#"{"category":"catalog","task_id":222,"identifier":"item-b","cmd":"derive.php","submitter":"user@example.com","submittime":"2026-03-10 12:01:00","server":"ia600101.us.archive.org","color":"blue","priority":5}"#,
        ].join("\n");

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("limit", "0"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();
        let (summary, entries) = list_tasks(&client, &query).await.unwrap();

        assert_eq!(summary.queued, 2);
        assert_eq!(summary.running, 1);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].task_id, 111);
        assert_eq!(entries[0].identifier, "item-a");
        assert_eq!(entries[0].category.as_deref(), Some("catalog"));
        assert_eq!(entries[1].task_id, 222);
        assert_eq!(entries[1].color, "blue");
    }

    #[tokio::test]
    async fn test_list_tasks_with_history() {
        let mock_server = MockServer::start().await;
        let body = [
            r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
            r#"{"category":"history","task_id":333,"identifier":"my-item","cmd":"derive.php","submitter":"user@example.com","submittime":"2026-03-09 10:00:00","server":"ia600100.us.archive.org","color":"done","priority":0,"finished":85575266}"#,
        ].join("\n");

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("identifier", "my-item"))
            .and(query_param("catalog", "1"))
            .and(query_param("history", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            identifier: Some("my-item".into()),
            catalog: Some(true),
            history: Some(true),
            ..Default::default()
        };
        let (_, entries) = list_tasks(&client, &query).await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category.as_deref(), Some("history"));
        assert_eq!(entries[0].finished, Some(85575266));
    }

    #[tokio::test]
    async fn test_list_tasks_empty() {
        let mock_server = MockServer::start().await;
        let body = r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();
        let (summary, entries) = list_tasks(&client, &query).await.unwrap();

        assert_eq!(summary.queued, 0);
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_list_tasks_malformed_line_skipped() {
        let mock_server = MockServer::start().await;
        let body = [
            r#"{"category":"summary","queued":1,"running":0,"error":0,"paused":0}"#,
            r#"this is not valid json"#,
            r#"{"category":"catalog","task_id":444,"identifier":"item-c","cmd":"fixer.php","color":"green"}"#,
        ].join("\n");

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();
        let (summary, entries) = list_tasks(&client, &query).await.unwrap();

        assert_eq!(summary.queued, 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_id, 444);
    }

    #[tokio::test]
    async fn test_list_tasks_passes_all_query_params() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("identifier", "test-id"))
            .and(query_param("cmd", "derive.php"))
            .and(query_param("submitter", "user@example.com"))
            .and(query_param("server", "ia600100"))
            .and(query_param("priority", "5"))
            .and(query_param("color", "green"))
            .and(query_param("args", "*s3*"))
            .and(query_param("catalog", "1"))
            .and(query_param("history", "0"))
            .and(query_param("summary", "1"))
            .and(query_param("custom_param", "custom_value"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(
                        r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
                    )
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            identifier: Some("test-id".into()),
            cmd: Some("derive.php".into()),
            submitter: Some("user@example.com".into()),
            server: Some("ia600100".into()),
            priority: Some(5),
            color: Some("green".into()),
            task_id: None,
            submittime_after: None,
            submittime_before: None,
            args: Some("*s3*".into()),
            catalog: Some(true),
            history: Some(false),
            summary: Some(true),
            limit: None,
            extra_params: vec![("custom_param".into(), "custom_value".into())],
        };
        let (summary, _) = list_tasks(&client, &query).await.unwrap();
        assert_eq!(summary.queued, 0);
    }

    #[tokio::test]
    async fn test_list_tasks_http_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();
        let result = list_tasks(&client, &query).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status, .. } => assert_eq!(status, 403),
            other => panic!("expected Http error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_get_tasks_success_false() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "value": {
                    "summary": {
                        "queued": 0,
                        "running": 0,
                        "error": 0,
                        "paused": 0
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery::default();

        let result = get_tasks(&client, &query).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status, message } => {
                assert_eq!(status, 200);
                assert!(message.contains("success: false"));
            }
            other => panic!("expected Http error, got: {other}"),
        }
    }

    // -- submit_task tests --

    #[tokio::test]
    async fn test_submit_task_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "task_id": 9876543,
                    "log": "https://catalogd.archive.org/log/9876543"
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 9876543);
        assert!(result.log.contains("catalogd"));
    }

    #[tokio::test]
    async fn test_submit_task_with_comment_and_args() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "task_id": 111, "log": "https://catalogd.archive.org/log/111" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let mut task_args = HashMap::new();
        task_args.insert("remove_derived".into(), "*.jpg".into());
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: Some(task_args),
            comment: Some("re-derive without JPEGs".into()),
            priority: Some(5),
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 111);
    }

    #[tokio::test]
    async fn test_submit_task_403() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(403).set_body_string("not authorized"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::Http { status: 403, .. }
        ));
    }

    #[tokio::test]
    async fn test_submit_task_success_false() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "error": "item is darked"
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::TaskSubmitFailed { .. }
        ));
    }

    #[tokio::test]
    async fn test_submit_task_requires_auth() {
        let mut config = crate::config::IaConfig::default();
        config.general.host = "localhost".to_string();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await;
        assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
    }

    #[tokio::test]
    async fn test_submit_task_429_returns_rate_limited() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("rate limited")
                    .insert_header("Retry-After", "30"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::RateLimited { retry_after } => {
                assert_eq!(retry_after, 30, "should extract Retry-After header value");
            }
            other => panic!("expected RateLimited, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_submit_task_429_no_retry_after_header() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await;
        match result.unwrap_err() {
            IaError::RateLimited { retry_after } => {
                assert_eq!(retry_after, 0, "should default to 0 when no header");
            }
            other => panic!("expected RateLimited, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_rerun_task_429_returns_rate_limited() {
        let mock_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/services/tasks.php"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("rate limited")
                    .insert_header("Retry-After", "15"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = rerun_task(&client, 123456).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::RateLimited { retry_after } => {
                assert_eq!(retry_after, 15);
            }
            other => panic!("expected RateLimited, got: {other}"),
        }
    }

    // -- rerun_task tests --

    #[tokio::test]
    async fn test_rerun_task_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/services/tasks.php"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "123456": "my-item" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = rerun_task(&client, 123456).await.unwrap();
        assert_eq!(result, "my-item");
    }

    #[tokio::test]
    async fn test_rerun_task_failure() {
        let mock_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/services/tasks.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "error": "task is not in error state"
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = rerun_task(&client, 123456).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::TaskRerunFailed { .. }
        ));
    }

    // -- get_task_log tests --

    #[tokio::test]
    async fn test_get_task_log_success() {
        let mock_server = MockServer::start().await;
        let log_text = "Task started at: UTC: 2026-03-10 12:00:00\nProcessing...\nDone.";
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_log", "1234567"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(log_text)
                    .insert_header("content-type", "text/plain;charset=UTF-8"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get_task_log(&client, 1234567).await.unwrap();
        assert!(result.contains("Task started at"));
        assert!(result.contains("Done."));
    }

    #[tokio::test]
    async fn test_get_task_log_not_found() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_log", "9999999"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get_task_log(&client, 9999999).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::TaskNotFound { task_id: 9999999 }
        ));
    }

    // -- get_rate_limit tests --

    #[tokio::test]
    async fn test_get_rate_limit_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("rate_limits", "1"))
            .and(query_param("cmd", "derive.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "cmd": "derive.php",
                    "task_limits": 500,
                    "tasks_inflight": 120,
                    "tasks_blocked_by_offline": 3
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get_rate_limit(&client, "derive.php").await.unwrap();
        assert_eq!(result.cmd, "derive.php");
        assert_eq!(result.task_limits, 500);
        assert_eq!(result.tasks_inflight, 120);
        assert_eq!(result.tasks_blocked_by_offline, 3);
    }

    #[tokio::test]
    async fn test_get_rate_limit_different_cmd() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("cmd", "make_dark.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "cmd": "make_dark.php",
                    "task_limits": 100,
                    "tasks_inflight": 0,
                    "tasks_blocked_by_offline": 0
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get_rate_limit(&client, "make_dark.php").await.unwrap();
        assert_eq!(result.cmd, "make_dark.php");
        assert_eq!(result.task_limits, 100);
    }

    // -- wait_for_task tests --

    #[tokio::test]
    async fn test_wait_for_task_completes() {
        let mock_server = MockServer::start().await;

        // Task is no longer in catalog — completed
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_id", "555"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {"queued": 0, "running": 0, "error": 0, "paused": 0},
                    "catalog": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        // When catalog is empty for the task_id, it means the task finished
        let result = wait_for_task(&client, 555, Duration::from_millis(10)).await;
        // Task completed (no longer in catalog)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_tasks_with_task_id() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_id", "12345"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(
                        r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
                    )
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            task_id: Some(12345),
            ..Default::default()
        };
        let (summary, _) = list_tasks(&client, &query).await.unwrap();
        assert_eq!(summary.queued, 0);
    }

    #[tokio::test]
    async fn test_list_tasks_with_date_range() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("submittime>=", "2026-03-01"))
            .and(query_param("submittime<=", "2026-03-10"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(
                        r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
                    )
                    .insert_header("content-type", "application/json-l"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let query = TasksQuery {
            submittime_after: Some("2026-03-01".into()),
            submittime_before: Some("2026-03-10".into()),
            ..Default::default()
        };
        let (summary, _) = list_tasks(&client, &query).await.unwrap();
        assert_eq!(summary.queued, 0);
    }

    #[tokio::test]
    async fn test_wait_for_task_polls_until_done() {
        let mock_server = MockServer::start().await;

        // First poll: task is running
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_id", "888"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {"queued": 0, "running": 1, "error": 0, "paused": 0},
                    "catalog": [{"task_id": 888, "identifier": "my-item", "cmd": "derive.php", "color": "blue"}]
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second poll: task is done
        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_id", "888"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": {
                    "summary": {"queued": 0, "running": 0, "error": 0, "paused": 0},
                    "catalog": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = wait_for_task(&client, 888, Duration::from_millis(10)).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_wait_for_task_propagates_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/tasks.php"))
            .and(query_param("task_id", "999"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = wait_for_task(&client, 999, Duration::from_millis(10)).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            IaError::Http { status: 403, .. } => {}
            other => panic!("expected Http 403 error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_submit_task_reduced_priority_header() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .and(header("X-Accept-Reduced-Priority", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "task_id": 777, "log": "https://catalogd.archive.org/log/777" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: true,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 777);
    }

    #[tokio::test]
    async fn test_submit_task_auto_appends_php() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .and(body_string_contains("\"cmd\":\"derive.php\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "task_id": 123, "log": "https://catalogd.archive.org/log/123" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive".into(), // no .php — should auto-append
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 123);
    }

    #[tokio::test]
    async fn test_submit_task_comment_merged_into_args() {
        let mock_server = MockServer::start().await;
        // Comment should appear inside the "args" object as "comment" key
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .and(body_string_contains("test reason"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "task_id": 456, "log": "https://catalogd.archive.org/log/456" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "make_dark.php".into(),
            args: None,
            comment: Some("test reason".into()),
            priority: None,
            reduced_priority: false,
            extra_params: vec![],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 456);
    }

    #[tokio::test]
    async fn test_submit_task_extra_params_in_body() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/tasks.php"))
            .and(body_string_contains("\"custom_field\":\"custom_value\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "value": { "task_id": 789, "log": "https://catalogd.archive.org/log/789" }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let submission = TaskSubmission {
            identifier: "my-item".into(),
            cmd: "derive.php".into(),
            args: None,
            comment: None,
            priority: None,
            reduced_priority: false,
            extra_params: vec![("custom_field".into(), "custom_value".into())],
        };

        let result = submit_task(&client, &submission).await.unwrap();
        assert_eq!(result.task_id, 789);
    }

    #[tokio::test]
    async fn test_rerun_task_requires_auth() {
        let mut config = crate::config::IaConfig::default();
        config.general.host = "localhost".to_string();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let result = rerun_task(&client, 123).await;
        assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
    }

    #[tokio::test]
    async fn test_get_rate_limit_requires_auth() {
        let mut config = crate::config::IaConfig::default();
        config.general.host = "localhost".to_string();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let result = get_rate_limit(&client, "derive.php").await;
        assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
    }
}
