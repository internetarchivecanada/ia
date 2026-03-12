# `ia tasks` Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a full-featured `ia tasks` CLI command with list, submit, log, rerun, and rate-limit subcommands.

**Architecture:** Expand `ia-core/src/tasks.rs` with new functions (list_tasks, submit_task, rerun_task, get_task_log, get_rate_limit, wait_for_task) and types, then add `ia-cli/src/commands/tasks.rs` with clap subcommand routing. Batch submit reuses the existing `BatchInput` + `collect_identifiers_from_batch` pattern from `metadata.rs`.

**Tech Stack:** reqwest (streaming JSONL), wiremock (tests), clap (CLI), comfy-table (table output), console (colors), indicatif (spinner for --wait)

**Spec:** `docs/plans/2026-03-10-tasks-command-design.md`

**Worktree:** `/Users/jake/github/jjjake/worktrees/tasks-command`

---

## File Structure

### Files to create:
- `ia-cli/src/commands/tasks.rs` — CLI command module (subcommand routing, output formatting, batch logic)

### Files to modify:
- `ia-core/src/tasks.rs` — Expand types + add list_tasks, submit_task, rerun_task, get_task_log, get_rate_limit, wait_for_task
- `ia-core/src/error.rs` — Add TaskSubmitFailed, TaskRerunFailed, TaskNotFound variants + is_retryable + to_json_error
- `ia-cli/src/commands/mod.rs` — Add `pub mod tasks;`
- `ia-cli/src/main.rs` — Register Tasks command variant + dispatch
- `ia-cli/src/tui/upload_app.rs` — Migrate from old get_tasks to new list_tasks
- `docs/usage.md` — Add tasks section
- `README.md` — Add tasks to command list

### Test locations:
- `ia-core/src/tasks.rs` — Unit tests in `#[cfg(test)] mod tests` (existing pattern)
- `ia-cli/tests/tasks.rs` — CLI integration tests (new file)

---

## Chunk 1: Core Types, Error Variants, and list_tasks

### Task 1: Add error variants for tasks operations

**Files:**
- Modify: `ia-core/src/error.rs`

- [ ] **Step 1: Add three new IaError variants**

Add after `SchemaFieldNotFound` (line 128), before the transparent variants:

```rust
#[error("task submission failed: {message}")]
TaskSubmitFailed { message: String },

#[error("task rerun failed for task {task_id}: {message}")]
TaskRerunFailed { task_id: u64, message: String },

#[error("task log not found for task {task_id}")]
TaskNotFound { task_id: u64 },
```

- [ ] **Step 2: Add is_retryable arms**

In `is_retryable()`, add before the `// Permanent` section:

```rust
// Task errors — permanent (API-level rejections)
IaError::TaskSubmitFailed { .. } => false,
IaError::TaskRerunFailed { .. } => false,
IaError::TaskNotFound { .. } => false,
```

- [ ] **Step 3: Add to_json_error arms**

In `to_json_error()`, add before the Network match:

```rust
IaError::TaskSubmitFailed { .. } => "task_submit_failed",
IaError::TaskRerunFailed { task_id, .. } => {
    extra.insert("task_id".into(), (*task_id).into());
    "task_rerun_failed"
}
IaError::TaskNotFound { task_id } => {
    extra.insert("task_id".into(), (*task_id).into());
    "task_not_found"
}
```

- [ ] **Step 4: Write tests for new variants**

```rust
#[test]
fn task_submit_failed_is_not_retryable() {
    let err = IaError::TaskSubmitFailed { message: "denied".into() };
    assert!(!err.is_retryable());
}

#[test]
fn task_rerun_failed_is_not_retryable() {
    let err = IaError::TaskRerunFailed { task_id: 123, message: "not error state".into() };
    assert!(!err.is_retryable());
}

#[test]
fn task_not_found_is_not_retryable() {
    let err = IaError::TaskNotFound { task_id: 123 };
    assert!(!err.is_retryable());
}

#[test]
fn json_task_submit_failed() {
    let err = IaError::TaskSubmitFailed { message: "denied".into() };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "task_submit_failed");
}

#[test]
fn json_task_rerun_failed() {
    let err = IaError::TaskRerunFailed { task_id: 123, message: "not error state".into() };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "task_rerun_failed");
    assert_eq!(v["error"]["task_id"], 123);
}

#[test]
fn json_task_not_found() {
    let err = IaError::TaskNotFound { task_id: 123 };
    let v = parse_json_error(&err);
    assert_eq!(v["error"]["code"], "task_not_found");
    assert_eq!(v["error"]["task_id"], 123);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ia-core -- error`
Expected: All error tests pass including 6 new ones.

- [ ] **Step 6: Commit**

```
git add ia-core/src/error.rs
git commit -m "feat(tasks): add TaskSubmitFailed, TaskRerunFailed, TaskNotFound error variants"
```

### Task 2: Expand core types in tasks.rs

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Add Serialize import and HashMap**

At top of file, update imports:

```rust
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
```

- [ ] **Step 2: Replace TasksQuery with extended version**

Replace the existing `TasksQuery` struct (lines 7-14) with:

```rust
/// Query parameters for the Tasks API.
#[derive(Debug, Default, Clone)]
pub struct TasksQuery {
    pub identifier: Option<String>,
    pub cmd: Option<String>,
    pub limit: Option<u32>,
    pub submitter: Option<String>,
    pub args: Option<String>,
    pub server: Option<String>,
    pub priority: Option<i32>,
    pub color: Option<String>,
    pub catalog: Option<bool>,
    pub history: Option<bool>,
    pub summary: Option<bool>,
    pub extra_params: Vec<(String, String)>,
}
```

- [ ] **Step 3: Add Serialize to TasksSummary and extend TaskEntry**

Add `Serialize` derive to `TasksSummary`. Replace existing `TaskEntry` with:

```rust
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

/// A single task entry from the catalog or history.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskEntry {
    pub task_id: u64,
    pub identifier: String,
    pub cmd: String,
    #[serde(default)]
    pub submitter: String,
    #[serde(default)]
    pub submittime: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub finished: Option<u64>,
}
```

- [ ] **Step 4: Add new types for submit, response, and rate limit**

Add after `TaskEntry`:

```rust
/// Task submission request body.
#[derive(Debug, Clone)]
pub struct TaskSubmission {
    pub identifier: String,
    pub cmd: String,
    pub args: Option<HashMap<String, String>>,
    pub comment: Option<String>,
    pub priority: Option<i32>,
    pub reduced_priority: bool,
    pub extra_params: Vec<(String, String)>,
}

/// Response from a successful task submission.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskSubmitResponse {
    pub task_id: u64,
    pub log: String,
}

/// Rate limit information for a task command.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitInfo {
    pub cmd: String,
    pub task_limits: u32,
    pub tasks_inflight: u32,
    pub tasks_blocked_by_offline: u32,
}
```

- [ ] **Step 5: Run tests to verify existing tests still pass with new fields**

Run: `cargo test -p ia-core -- tasks`
Expected: All 7 existing tests pass. The new optional fields (`args`, `category`, `finished`) use `#[serde(default)]` so existing JSON without those fields still deserializes.

- [ ] **Step 6: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): expand TasksQuery, TaskEntry, add TaskSubmission/TaskSubmitResponse/RateLimitInfo types"
```

### Task 3: Implement list_tasks with streaming JSONL

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Write tests for list_tasks**

Add to the existing `mod tests` block. These test the streaming JSONL path.

```rust
#[tokio::test]
async fn test_list_tasks_streaming_jsonl() {
    let mock_server = MockServer::start().await;

    // Streaming JSONL: one line per object with category field
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
                .set_body_string(r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#)
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
        args: Some("*s3*".into()),
        catalog: Some(true),
        history: Some(false),
        summary: Some(true),
        limit: None, // list_tasks forces limit=0 for streaming
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- test_list_tasks`
Expected: FAIL — `list_tasks` function doesn't exist yet.

- [ ] **Step 3: Implement list_tasks**

Add helper function to build query params, then implement `list_tasks`:

```rust
/// Build query parameters from a TasksQuery, adding auth header.
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

/// List tasks using streaming JSONL (limit=0).
///
/// Parses the response line-by-line. Summary lines are merged into
/// `TasksSummary`. Catalog and history entries are collected into a flat
/// `Vec<TaskEntry>`. Malformed lines are skipped with a debug log.
pub async fn list_tasks(
    client: &IaClient,
    query: &TasksQuery,
) -> Result<(TasksSummary, Vec<TaskEntry>)> {
    let mut streaming_query = query.clone();
    // Force limit=0 for streaming unless caller explicitly set a limit
    // (used when --limit + --no-summary for server-side truncation)
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

    let body = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let mut summary = TasksSummary::default();
    let mut entries = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try to parse as a generic JSON value first to check category
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("skipping malformed JSONL line: {e}");
                continue;
            }
        };

        match value.get("category").and_then(|c| c.as_str()) {
            Some("summary") => {
                if let Ok(s) = serde_json::from_value::<TasksSummary>(value) {
                    summary = s;
                }
            }
            Some("catalog") | Some("history") => {
                match serde_json::from_value::<TaskEntry>(value) {
                    Ok(entry) => entries.push(entry),
                    Err(e) => tracing::debug!("skipping malformed task entry: {e}"),
                }
            }
            _ => {
                tracing::debug!("skipping line with unknown category");
            }
        }
    }

    Ok((summary, entries))
}
```

- [ ] **Step 4: Refactor existing get_tasks to use build_tasks_request**

Update the existing `get_tasks()` function to use the shared helper:

```rust
/// Fetch tasks from the archive.org Tasks API (read-only).
/// Used by the upload TUI dashboard. Prefer list_tasks() for new code.
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
```

- [ ] **Step 5: Run all tasks tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All existing tests + new list_tasks tests pass.

- [ ] **Step 6: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): implement list_tasks with streaming JSONL parsing"
```

### Task 4: Implement submit_task, rerun_task, get_task_log, get_rate_limit

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Write tests for submit_task**

```rust
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
        args: None, comment: None, priority: None,
        reduced_priority: false, extra_params: vec![],
    };

    let result = submit_task(&client, &submission).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), IaError::Http { status: 403, .. }));
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
        args: None, comment: None, priority: None,
        reduced_priority: false, extra_params: vec![],
    };

    let result = submit_task(&client, &submission).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), IaError::TaskSubmitFailed { .. }));
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
        args: None, comment: None, priority: None,
        reduced_priority: false, extra_params: vec![],
    };

    let result = submit_task(&client, &submission).await;
    assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
}
```

- [ ] **Step 2: Write tests for rerun_task**

```rust
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
    let result = rerun_task(&client, 123456).await;
    assert!(result.is_ok());
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
    assert!(matches!(result.unwrap_err(), IaError::TaskRerunFailed { .. }));
}
```

- [ ] **Step 3: Write tests for get_task_log**

```rust
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
    assert!(matches!(result.unwrap_err(), IaError::TaskNotFound { task_id: 9999999 }));
}
```

- [ ] **Step 4: Write tests for get_rate_limit**

```rust
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
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test -p ia-core -- "test_submit_task\|test_rerun_task\|test_get_task_log\|test_get_rate_limit"`
Expected: FAIL — functions don't exist yet.

- [ ] **Step 6: Implement submit_task**

```rust
/// Submit a task to the Tasks API (POST).
///
/// Comment is merged into `args.comment` before sending.
/// Auto-appends `.php` to cmd if not present.
pub async fn submit_task(
    client: &IaClient,
    submission: &TaskSubmission,
) -> Result<TaskSubmitResponse> {
    let (access, secret) = client.require_auth()?;
    let url = client.url("/services/tasks.php");

    // Build JSON body
    let cmd = if submission.cmd.ends_with(".php") {
        submission.cmd.clone()
    } else {
        format!("{}.php", submission.cmd)
    };

    let mut args_map: HashMap<String, String> =
        submission.args.clone().unwrap_or_default();
    if let Some(ref comment) = submission.comment {
        args_map.insert("comment".into(), comment.clone());
    }

    let mut body = serde_json::json!({
        "identifier": submission.identifier,
        "cmd": cmd,
    });
    if !args_map.is_empty() {
        body["args"] = serde_json::to_value(&args_map)
            .unwrap_or_default();
    }
    if let Some(priority) = submission.priority {
        body["priority"] = serde_json::json!(priority);
    }
    // Merge extra_params into body
    for (key, value) in &submission.extra_params {
        body[key] = serde_json::json!(value);
    }

    let mut req = client
        .http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body).unwrap_or_default());

    if submission.reduced_priority {
        req = req.header("x-archive-queue-derive", "1");
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body_text = resp.text().await.map_err(reqwest_middleware::Error::from)?;

    // Parse response — check for success: false
    let parsed: serde_json::Value = serde_json::from_str(&body_text)?;
    if parsed.get("success").and_then(|s| s.as_bool()) != Some(true) {
        let msg = parsed.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(IaError::TaskSubmitFailed { message: msg });
    }

    let value = parsed.get("value")
        .ok_or_else(|| IaError::TaskSubmitFailed {
            message: "response missing value field".into(),
        })?;
    let response: TaskSubmitResponse = serde_json::from_value(value.clone())?;
    Ok(response)
}
```

- [ ] **Step 7: Implement rerun_task**

```rust
/// Rerun a failed task (PUT).
///
/// Sends `{"op": "rerun", "task_id": N}`.
pub async fn rerun_task(client: &IaClient, task_id: u64) -> Result<()> {
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
        .body(serde_json::to_vec(&body).unwrap_or_default())
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body_text = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let parsed: serde_json::Value = serde_json::from_str(&body_text)?;
    if parsed.get("success").and_then(|s| s.as_bool()) != Some(true) {
        let msg = parsed.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(IaError::TaskRerunFailed {
            task_id,
            message: msg,
        });
    }

    Ok(())
}
```

- [ ] **Step 8: Implement get_task_log**

```rust
/// Fetch a task's execution log.
///
/// For production (archive.org host), hits catalogd.archive.org directly
/// to avoid the 301 redirect. For other hosts (tests), uses client's host.
/// Returns the raw log text.
pub async fn get_task_log(client: &IaClient, task_id: u64) -> Result<String> {
    let (access, secret) = client.require_auth()?;

    // Use catalogd for production, client's host for tests
    let url = if client.host().contains("archive.org") {
        format!("{}://catalogd.archive.org/services/tasks.php", client.protocol())
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
```

- [ ] **Step 9: Implement get_rate_limit**

```rust
/// Check rate limits for a task command.
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

    // Response may be JSONL (streaming) or standard JSON envelope
    // Try standard envelope first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
        if parsed.get("success").and_then(|s| s.as_bool()) == Some(true) {
            if let Some(value) = parsed.get("value") {
                return Ok(serde_json::from_value(value.clone())?);
            }
        }
    }

    // Fall back to JSONL parsing (each line is a JSON object)
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
```

- [ ] **Step 10: Run all tasks tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All tests pass (existing 7 + new ~16).

- [ ] **Step 11: Run clippy**

Run: `cargo clippy -p ia-core -- -D warnings`
Expected: No warnings.

- [ ] **Step 12: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): implement submit_task, rerun_task, get_task_log, get_rate_limit"
```

### Task 5: Implement wait_for_task

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Add Duration import**

```rust
use std::time::Duration;
```

- [ ] **Step 2: Write tests for wait_for_task**

```rust
#[tokio::test]
async fn test_wait_for_task_completes() {
    let mock_server = MockServer::start().await;
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = call_count.clone();

    // First call: task is running. Second call: task is done.
    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "555"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
```

- [ ] **Step 3: Implement wait_for_task**

```rust
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
        // Query for this specific task in the catalog
        let query = TasksQuery {
            extra_params: vec![("task_id".into(), task_id.to_string())],
            catalog: Some(true),
            history: Some(true),
            ..Default::default()
        };

        let (_, entries) = list_tasks(client, &query).await?;

        // Check if the task is in history (completed)
        if let Some(entry) = entries.iter().find(|e| {
            e.task_id == task_id
                && e.category.as_deref() == Some("history")
        }) {
            return Ok(Some(entry.clone()));
        }

        // Check if the task is still in catalog (still active)
        let still_active = entries.iter().any(|e| {
            e.task_id == task_id
                && e.category.as_deref() == Some("catalog")
        });

        if !still_active && entries.is_empty() {
            // Task not found in either catalog or history via streaming
            // Fall back to non-streaming query
            let fallback = TasksQuery {
                extra_params: vec![("task_id".into(), task_id.to_string())],
                ..Default::default()
            };
            let result = get_tasks(client, &fallback).await;
            match result {
                Ok(value) => {
                    if value.catalog.is_empty() {
                        // Task is done (no longer active)
                        return Ok(None);
                    }
                    // Still active, keep waiting
                }
                Err(_) => return Ok(None),
            }
        } else if !still_active {
            return Ok(None);
        }

        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(max_interval);
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ia-core -- test_wait_for_task`
Expected: PASS

- [ ] **Step 5: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): implement wait_for_task with exponential backoff"
```

---

## Chunk 2: CLI Command Module

### Task 6: Create commands/tasks.rs with list subcommand

**Files:**
- Create: `ia-cli/src/commands/tasks.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

- [ ] **Step 1: Add `pub mod tasks;` to commands/mod.rs**

Add `pub mod tasks;` in alphabetical order (after `status`, before `#[cfg(feature = "self-update")]`).

- [ ] **Step 2: Register Tasks command in main.rs**

In the `Commands` enum (after `Status`), add:

```rust
/// Manage archive.org catalog tasks
#[command(visible_alias = "ta")]
Tasks(commands::tasks::TasksArgs),
```

In the dispatch match (after `Commands::Status`), add:

```rust
Commands::Tasks(args) => {
    commands::tasks::run(
        &client,
        args,
        cli.quiet,
        cli.jobs,
        cli.joblog,
        cli.retry_failed,
    )
    .await?
}
```

- [ ] **Step 3: Create commands/tasks.rs with the list (bare) subcommand**

Create `ia-cli/src/commands/tasks.rs` with the full structure. Start with just the list functionality to keep it testable:

```rust
use std::io::IsTerminal;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use comfy_table::{presets, Cell, Color as TableColor, Table};
use console::style;

use ia_core::tasks::{self, TaskEntry, TasksSummary, TasksQuery};
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
         \n  <bold>$ ia tasks submit my-item derive</bold>\
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
        Some(TasksCommand::Rerun(rerun_args)) => run_rerun(client, rerun_args, quiet).await,
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
        && !extra_params.iter().any(|(k, _)| k == "task_id")
    {
        bail!("--completed-only requires an identifier or task_id parameter");
    }

    // Build query with smart defaults matching Python behavior
    let has_identifier = args.identifier.is_some();
    let query = TasksQuery {
        identifier: args.identifier.clone(),
        cmd: args.cmd.clone(),
        submitter: if has_identifier {
            args.submitter.clone()
        } else {
            // Default to current user when no identifier given
            args.submitter.clone().or_else(|| {
                // Get submitter from whoami if not explicitly set
                let rt_client = client.clone();
                // Use blocking approach since we're already in async context
                None // Will be set below
            })
        },
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
    let query = if !has_identifier && query.submitter.is_none() {
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
        print_json_listing(&summary, &entries, &args);
    } else {
        print_table_listing(&summary, &entries, quiet, args.no_summary);
    }

    Ok(())
}

fn print_json_listing(
    summary: &TasksSummary,
    entries: &[TaskEntry],
    args: &TasksArgs,
) {
    if !args.no_summary {
        let summary_json = serde_json::json!({
            "summary": summary,
        });
        println!("{}", serde_json::to_string(&summary_json).unwrap_or_default());
    }
    for entry in entries {
        println!("{}", serde_json::to_string(entry).unwrap_or_default());
    }
}

fn print_table_listing(
    summary: &TasksSummary,
    entries: &[TaskEntry],
    quiet: u8,
    no_summary: bool,
) {
    // Print summary header if non-zero and not suppressed
    let has_counts = summary.queued > 0
        || summary.running > 0
        || summary.error > 0
        || summary.paused > 0;

    if has_counts && !no_summary && quiet < 2 {
        let mut parts = Vec::new();
        if summary.queued > 0 {
            parts.push(format!("Queued: {}", style(summary.queued).yellow()));
        }
        if summary.running > 0 {
            parts.push(format!("Running: {}", style(summary.running).cyan()));
        }
        if summary.error > 0 {
            parts.push(format!("Error: {}", style(summary.error).red()));
        }
        if summary.paused > 0 {
            parts.push(format!("Paused: {}", style(summary.paused).dim()));
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

// ─── Stubs for other subcommands (implemented in later tasks) ────────────────

async fn run_submit(
    _client: &IaClient,
    _args: SubmitArgs,
    _quiet: u8,
    _jobs: usize,
    _joblog: Option<std::path::PathBuf>,
    _retry_failed: bool,
) -> Result<()> {
    bail!("submit not yet implemented")
}

async fn run_log(_client: &IaClient, _args: LogArgs) -> Result<()> {
    bail!("log not yet implemented")
}

async fn run_rerun(_client: &IaClient, _args: RerunArgs, _quiet: u8) -> Result<()> {
    bail!("rerun not yet implemented")
}

async fn run_rate_limit(_client: &IaClient, _args: RateLimitArgs) -> Result<()> {
    bail!("rate-limit not yet implemented")
}
```

- [ ] **Step 4: Run `cargo check -p ia-cli`**

Expected: Compiles without errors.

- [ ] **Step 5: Write CLI integration test for bare listing**

Create `ia-cli/tests/tasks.rs`:

```rust
use assert_cmd::Command;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ia_cmd() -> Command {
    Command::cargo_bin("ia").unwrap()
}

#[tokio::test]
async fn test_tasks_list_json() {
    let mock_server = MockServer::start().await;

    // Mock whoami (POST /services/xauthn/?op=info)
    Mock::given(method("POST"))
        .and(path("/services/xauthn/"))
        .and(query_param("op", "info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "values": {
                "screenname": "testuser",
                "email": "test@example.com",
                "itemname": "@testuser"
            }
        })))
        .mount(&mock_server)
        .await;

    let body = [
        r#"{"category":"summary","queued":1,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":111,"identifier":"test-item","cmd":"derive.php","submitter":"test@example.com","submittime":"2026-03-10 12:00:00","server":"ia600100.us.archive.org","color":"green","priority":0}"#,
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

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":111"));
    assert!(stdout.contains("derive.php"));
}

#[tokio::test]
async fn test_tasks_alias() {
    // Just check that `ia ta` parses correctly (will fail connecting but
    // proves the alias is registered)
    let output = ia_cmd()
        .args(["ta", "--help"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("catalog tasks"));
}

#[tokio::test]
async fn test_tasks_active_completed_mutual_exclusion() {
    let output = ia_cmd()
        .args(["tasks", "--active-only", "--completed-only"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mutually exclusive"));
}

#[tokio::test]
async fn test_tasks_completed_only_requires_identifier() {
    let output = ia_cmd()
        .args(["tasks", "--completed-only"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires an identifier"));
}
```

- [ ] **Step 6: Run CLI integration tests**

Run: `cargo test -p ia-cli -- tasks`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```
git add ia-cli/src/commands/tasks.rs ia-cli/src/commands/mod.rs ia-cli/src/main.rs ia-cli/tests/tasks.rs
git commit -m "feat(tasks): add ia tasks CLI command with list subcommand and table output"
```

### Task 7: Implement log, rate-limit, submit, and rerun subcommands

**Files:**
- Modify: `ia-cli/src/commands/tasks.rs`

- [ ] **Step 1: Implement run_log**

Replace the stub:

```rust
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
```

- [ ] **Step 2: Implement run_rate_limit**

Replace the stub:

```rust
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
```

- [ ] **Step 3: Implement run_submit (single mode)**

Replace the stub. Full implementation with batch and --wait support:

```rust
use std::collections::HashMap;
use std::time::Duration;

use ia_core::auth;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::tasks::{self, TaskEntry, TasksQuery, TasksSummary, TaskSubmission};

async fn run_submit(
    client: &IaClient,
    args: SubmitArgs,
    quiet: u8,
    jobs: usize,
    joblog: Option<std::path::PathBuf>,
    retry_failed: bool,
) -> Result<()> {
    // Parse task args
    let task_args: HashMap<String, String> = args
        .task_args
        .iter()
        .filter_map(|a| {
            let (k, v) = a.split_once('=').or_else(|| a.split_once(':'))?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    let extra_params: Vec<(String, String)> = args
        .parameter
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=').or_else(|| p.split_once(':'))?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    // Collect identifiers
    let identifiers = collect_submit_identifiers(&args, client).await?;

    if identifiers.is_empty() {
        bail!("no identifiers provided");
    }

    // Single mode
    if identifiers.len() == 1 {
        let submission = TaskSubmission {
            identifier: identifiers[0].clone(),
            cmd: args.cmd.clone(),
            args: if task_args.is_empty() { None } else { Some(task_args) },
            comment: args.comment.clone(),
            priority: if args.priority == 0 { None } else { Some(args.priority) },
            reduced_priority: args.reduced_priority,
            extra_params,
        };

        let result = submit_with_retry(client, &submission, args.max_retries, quiet).await?;

        if args.json {
            println!("{}", serde_json::to_string(&serde_json::json!({
                "success": true,
                "value": result,
            }))?);
        } else if quiet == 0 {
            eprintln!("Task submitted: {}", style(result.task_id).cyan());
            eprintln!("Log: {}", style(&result.log).dim());
        }

        // --wait
        if args.wait {
            let interval = Duration::from_secs(args.wait_interval);
            if quiet == 0 && !args.json {
                eprintln!("Waiting for task {}...", result.task_id);
            }
            let completed = tasks::wait_for_task(client, result.task_id, interval).await?;
            if let Some(entry) = &completed {
                if args.json {
                    println!("{}", serde_json::to_string(entry)?);
                } else if quiet == 0 {
                    let status_label = match entry.color.as_str() {
                        "done" => "completed",
                        "red" => "error",
                        other => other,
                    };
                    eprintln!("Task {} {}", result.task_id, status_label);
                }
                if entry.color == "red" {
                    std::process::exit(1);
                }
            } else if quiet == 0 && !args.json {
                eprintln!("Task {} completed", result.task_id);
            }
        }

        return Ok(());
    }

    // Batch mode
    let mut joblog_writer = if let Some(ref path) = joblog {
        Some(JoblogWriter::open(path)?)
    } else {
        None
    };

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for id in &identifiers {
        let permit = sem.clone().acquire_owned().await?;
        let submission = TaskSubmission {
            identifier: id.clone(),
            cmd: args.cmd.clone(),
            args: if task_args.is_empty() { None } else { Some(task_args.clone()) },
            comment: args.comment.clone(),
            priority: if args.priority == 0 { None } else { Some(args.priority) },
            reduced_priority: args.reduced_priority,
            extra_params: extra_params.clone(),
        };

        match submit_with_retry(client, &submission, args.max_retries, quiet).await {
            Ok(result) => {
                succeeded += 1;
                if args.json {
                    println!("{}", serde_json::to_string(&serde_json::json!({
                        "identifier": id,
                        "task_id": result.task_id,
                        "success": true,
                    }))?);
                } else if quiet == 0 {
                    eprintln!("{} {} (task {})",
                        style("✓").green(),
                        id,
                        result.task_id
                    );
                }
                if let Some(ref mut writer) = joblog_writer {
                    writer.append(&JoblogEntry::success(id))?;
                }
            }
            Err(e) => {
                failed += 1;
                if args.json {
                    let json_err = ia_core::error::IaError::TaskSubmitFailed {
                        message: e.to_string(),
                    }
                    .to_json_error();
                    eprintln!("{}", serde_json::to_string(&json_err)?);
                } else {
                    eprintln!("{} {}: {}", style("✗").red(), id, e);
                }
                if let Some(ref mut writer) = joblog_writer {
                    writer.append(&JoblogEntry::failure(id, &e.to_string()))?;
                }
            }
        }
        drop(permit);
    }

    // Batch summary
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

    Ok(())
}

async fn collect_submit_identifiers(
    args: &SubmitArgs,
    client: &IaClient,
) -> Result<Vec<String>> {
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
        use futures::StreamExt;
        let opts = ia_core::search::SearchOpts::default();
        let mut stream = ia_core::search::scrape(client, query, &opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    }

    Ok(ids)
}

/// Submit a task with auto-retry on 429.
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
            Err(ia_core::error::IaError::Http { status: 429, message }) => {
                retries += 1;
                if retries > max_retries {
                    bail!("max retries ({max_retries}) exceeded for rate limiting");
                }

                // Try to parse Retry-After from message or use backoff
                let wait = backoff;
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
```

- [ ] **Step 4: Implement run_rerun**

Replace the stub:

```rust
async fn run_rerun(client: &IaClient, args: RerunArgs, quiet: u8) -> Result<()> {
    let task_ids = collect_rerun_task_ids(&args, client).await?;

    if task_ids.is_empty() {
        bail!("no task IDs provided");
    }

    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for task_id in &task_ids {
        match tasks::rerun_task(client, *task_id).await {
            Ok(()) => {
                succeeded += 1;
                if args.json {
                    println!("{}", serde_json::json!({
                        "task_id": task_id,
                        "success": true,
                    }));
                } else if quiet == 0 {
                    eprintln!("{} Rerun task {task_id}", style("✓").green());
                }
            }
            Err(e) => {
                failed += 1;
                if args.json {
                    println!("{}", serde_json::json!({
                        "task_id": task_id,
                        "success": false,
                        "error": e.to_string(),
                    }));
                } else {
                    eprintln!("{} Task {task_id}: {e}", style("✗").red());
                }
            }
        }
    }

    if task_ids.len() > 1 && !args.json && quiet < 2 {
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

/// Collect task IDs from positional args, stdin, or query filters.
async fn collect_rerun_task_ids(
    args: &RerunArgs,
    client: &IaClient,
) -> Result<Vec<u64>> {
    let mut ids = Vec::new();

    // Parse positional task IDs
    for arg in &args.task_ids {
        if arg == "-" {
            // Read from stdin
            use std::io::BufRead;
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                // Try parsing as raw integer
                if let Ok(id) = trimmed.parse::<u64>() {
                    ids.push(id);
                    continue;
                }
                // Try parsing as JSONL with task_id field
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if let Some(id) = value.get("task_id").and_then(|v| v.as_u64()) {
                        ids.push(id);
                        continue;
                    }
                }
                // Skip unrecognized lines
                tracing::debug!("skipping unrecognized stdin line: {trimmed}");
            }
        } else {
            ids.push(arg.parse::<u64>().context(format!("invalid task ID: {arg}"))?);
        }
    }

    // Query filter mode — list matching tasks then collect IDs
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
```

- [ ] **Step 5: Add required imports at top of file**

Ensure all needed imports are present:

```rust
use std::collections::HashMap;
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use comfy_table::{presets, Cell, Color as TableColor, Table};
use console::style;
use futures::StreamExt;

use ia_core::error::write_json_error;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::tasks::{self, TaskEntry, TasksQuery, TasksSummary, TaskSubmission};
use ia_core::IaClient;
```

- [ ] **Step 6: Run `cargo check -p ia-cli`**

Expected: Compiles. Fix any issues.

- [ ] **Step 7: Run `cargo clippy -p ia-core -p ia-cli -- -D warnings`**

Expected: No clippy warnings.

- [ ] **Step 8: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

- [ ] **Step 9: Write additional CLI integration tests**

Add to `ia-cli/tests/tasks.rs`:

```rust
#[tokio::test]
async fn test_tasks_rate_limit_json() {
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
                "tasks_blocked_by_offline": 0
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "rate-limit", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_limits\":500"));
}

#[tokio::test]
async fn test_tasks_log_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_log", "1234567"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("Task started at: 2026-03-10\nDone.")
                .insert_header("content-type", "text/plain;charset=UTF-8"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "tasks", "log", "1234567", "--json"])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":1234567"));
    assert!(stdout.contains("Task started at"));
}

#[tokio::test]
async fn test_tasks_submit_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "value": {
                "task_id": 9876543,
                "log": "https://catalogd.archive.org/log/9876543"
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure", "-H", &host,
            "tasks", "submit", "derive", "my-item", "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9876543"));
}
```

- [ ] **Step 10: Run all tests again**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All pass.

- [ ] **Step 11: Commit**

```
git add ia-cli/src/commands/tasks.rs ia-cli/tests/tasks.rs
git commit -m "feat(tasks): implement submit, log, rerun, rate-limit subcommands with batch + retry"
```

---

## Chunk 3: TUI Migration, Docs, and Final Polish

### Task 8: Migrate upload TUI to use list_tasks

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`

- [ ] **Step 1: Update import**

Replace `use ia_core::tasks::{get_tasks, TasksQuery};` with `use ia_core::tasks::{list_tasks, TasksQuery};`

- [ ] **Step 2: Update the tasks polling call**

Find the `get_tasks()` call in the polling loop and replace with `list_tasks()`:

```rust
// Before:
let result = get_tasks(&client, &query).await;
// After:
let result = list_tasks(&client, &query).await;
```

Adjust how the result is destructured — `list_tasks` returns `(TasksSummary, Vec<TaskEntry>)` instead of `TasksValue`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All pass. TUI tests (if any) still work.

- [ ] **Step 4: Commit**

```
git add ia-cli/src/tui/upload_app.rs
git commit -m "refactor(tui): migrate upload dashboard from get_tasks to list_tasks"
```

### Task 9: Update documentation

**Files:**
- Modify: `docs/usage.md`
- Modify: `README.md`

- [ ] **Step 1: Add tasks section to usage.md**

Find where other commands are documented and add a `## Tasks` section covering all subcommands with examples.

- [ ] **Step 2: Update README.md command list**

Add `tasks` to the commands list in README.md.

- [ ] **Step 3: Commit**

```
git add docs/usage.md README.md
git commit -m "docs: add ia tasks to usage.md and README.md"
```

### Task 10: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: No formatting issues.

- [ ] **Step 4: Run doc check**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p ia-core -p ia-cli`
Expected: No doc warnings.

- [ ] **Step 5: Push and verify CI**

```
git push
```

Check that CI passes on the PR.
