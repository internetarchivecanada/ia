# PR #255 Tasks Fixes — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix bugs, add missing features, and add missing tests identified in review of PR #255 (`feat/tasks-command`).

**Architecture:** All changes are on the existing `fix/tasks-fixes` branch (branched from `feat/tasks-command`). Changes touch `ia-core/src/tasks.rs` (types + functions), `ia-core/src/error.rs` (no changes needed — `RateLimited` already exists), `ia-cli/src/commands/tasks.rs` (CLI flags + validation + spinner), `ia-cli/src/tui/upload_app.rs` (optimize query), and `ia-cli/tests/tasks.rs` (new tests). Design doc updated.

**Tech Stack:** Rust, reqwest, wiremock (tests), clap (CLI), indicatif (spinner), comfy-table (table output)

**Worktree:** `~/github/jjjake/worktrees/tasks-fixes`

---

## File Structure

### Files to modify:
- `ia-core/src/tasks.rs` — Add `task_id`/`submittime_after`/`submittime_before` to `TasksQuery`, fix reduced priority header, simplify `wait_for_task`
- `ia-cli/src/commands/tasks.rs` — Add `--task-id`/`--since`/`--before` flags, spinner for `--wait`, validate `--args` format, reject `--wait` + batch
- `ia-cli/src/tui/upload_app.rs` — Optimize query to minimize data transfer
- `ia-cli/tests/tasks.rs` — Add ~16 missing tests
- `docs/plans/2026-03-10-tasks-command-design.md` — Update with new flags and fixes

### Files NOT changed:
- `ia-core/src/error.rs` — `RateLimited` variant already exists, no changes needed

---

## Chunk 1: Core Bug Fixes and Type Changes

### Task 1: Fix reduced priority header

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Write test for correct reduced priority header**

Add to `mod tests` in `ia-core/src/tasks.rs`, after `test_submit_task_429_no_retry_after_header`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails (wrong header name)**

Run: `cargo test -p ia-core -- test_submit_task_reduced_priority_header`
Expected: FAIL — the mock expects `X-Accept-Reduced-Priority` but the code sends `x-archive-queue-derive`.

- [ ] **Step 3: Fix the header name**

In `ia-core/src/tasks.rs`, in `submit_task()`, change:

```rust
// Old:
req = req.header("x-archive-queue-derive", "1");
// New:
req = req.header("X-Accept-Reduced-Priority", "1");
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core -- test_submit_task_reduced_priority_header`
Expected: PASS

- [ ] **Step 5: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "fix(tasks): use correct X-Accept-Reduced-Priority header for reduced priority"
```

### Task 2: Add `task_id` field to `TasksQuery`

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Add `task_id` field to `TasksQuery`**

In `TasksQuery` struct, add after the `color` field:

```rust
pub task_id: Option<u64>,
```

- [ ] **Step 2: Wire `task_id` through `build_tasks_request()`**

In `build_tasks_request()`, add after the `color` block:

```rust
if let Some(task_id) = query.task_id {
    req = req.query(&[("task_id", &task_id.to_string())]);
}
```

- [ ] **Step 3: Update `wait_for_task` to use the field**

In `wait_for_task()`, replace the `TasksQuery` construction:

```rust
// Old:
let query = TasksQuery {
    extra_params: vec![("task_id".into(), task_id.to_string())],
    catalog: Some(true),
    history: Some(true),
    ..Default::default()
};
// New:
let query = TasksQuery {
    task_id: Some(task_id),
    catalog: Some(true),
    history: Some(true),
    ..Default::default()
};
```

Also replace the fallback `TasksQuery`:

```rust
// Old:
let fallback = TasksQuery {
    extra_params: vec![("task_id".into(), task_id.to_string())],
    ..Default::default()
};
// New:
let fallback = TasksQuery {
    task_id: Some(task_id),
    ..Default::default()
};
```

- [ ] **Step 4: Write test that verifies task_id query param is sent**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All pass.

- [ ] **Step 6: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): add task_id field to TasksQuery, wire through build_tasks_request and wait_for_task"
```

### Task 3: Add `submittime_after` and `submittime_before` to `TasksQuery`

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Add fields to `TasksQuery`**

After the `task_id` field, add:

```rust
/// Filter by submittime >= value (parseable date/time string)
pub submittime_after: Option<String>,
/// Filter by submittime <= value (parseable date/time string)
pub submittime_before: Option<String>,
```

- [ ] **Step 2: Wire through `build_tasks_request()`**

Add after the `task_id` block:

```rust
if let Some(ref after) = query.submittime_after {
    req = req.query(&[("submittime>=", after.as_str())]);
}
if let Some(ref before) = query.submittime_before {
    req = req.query(&[("submittime<=", before.as_str())]);
}
```

- [ ] **Step 3: Write test**

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All pass.

- [ ] **Step 5: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "feat(tasks): add submittime date range filters (--since/--before) to TasksQuery"
```

### Task 4: Simplify `wait_for_task` to use `get_tasks()` and propagate errors

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Replace `wait_for_task` implementation**

Replace the entire `wait_for_task` function body with:

```rust
/// Poll a task until it completes (leaves the catalog).
///
/// Uses exponential backoff from `initial_interval`, capped at 60s.
/// Returns the task's final entry if found in history, or `None` if
/// it disappeared from both catalog and history.
/// No total timeout — caller can cancel via Ctrl+C.
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
            history: Some(true),
            ..Default::default()
        };

        let value = get_tasks(client, &query).await?;

        // Check if any catalog entry matches (still active)
        let still_active = value.catalog.iter().any(|e| e.task_id == task_id);

        if !still_active {
            // Task is done — not in catalog anymore.
            // Return None since get_tasks doesn't include history entries
            // (the caller can look up the final state if needed).
            return Ok(None);
        }

        // Still active — report current state and sleep
        let current = value.catalog.iter().find(|e| e.task_id == task_id);
        if let Some(entry) = current {
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
```

- [ ] **Step 2: Remove unused `bytes` and `futures` imports if no longer needed**

Check if `BytesMut` and `futures::StreamExt` are still used by `list_tasks`. They are — `list_tasks` still uses streaming. No import changes needed.

- [ ] **Step 3: Run existing wait_for_task test**

Run: `cargo test -p ia-core -- test_wait_for_task`
Expected: PASS — the existing test mocks a `get_tasks` JSON envelope response, which now matches the implementation.

- [ ] **Step 4: Add test for wait_for_task with active task that completes**

```rust
#[tokio::test]
async fn test_wait_for_task_polls_until_done() {
    let mock_server = MockServer::start().await;

    // First poll: task is running. Second poll: task is gone.
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
    assert!(result.unwrap().is_none()); // get_tasks doesn't return history
}
```

- [ ] **Step 5: Add test for wait_for_task propagating errors**

```rust
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
```

- [ ] **Step 6: Run all tasks tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All pass.

- [ ] **Step 7: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "fix(tasks): simplify wait_for_task to use get_tasks(), propagate errors instead of swallowing"
```

### Task 5: Add missing core tests (body verification, auto-append .php, auth, extra_params)

**Files:**
- Modify: `ia-core/src/tasks.rs`

- [ ] **Step 1: Add test for submit_task auto-append .php**

```rust
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
```

Note: Add `use wiremock::matchers::body_string_contains;` to the test imports.

- [ ] **Step 2: Add test for submit_task body with comment merged into args**

```rust
#[tokio::test]
async fn test_submit_task_body_has_comment_in_args() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_string_contains("\"comment\":\"test reason\""))
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
```

- [ ] **Step 3: Add test for submit_task with extra_params in body**

```rust
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
```

- [ ] **Step 4: Add test for rerun_task auth required**

```rust
#[tokio::test]
async fn test_rerun_task_requires_auth() {
    let mut config = crate::config::IaConfig::default();
    config.general.host = "localhost".to_string();
    config.general.secure = false;
    let client = IaClient::from_config(config).unwrap();

    let result = rerun_task(&client, 123).await;
    assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
}
```

- [ ] **Step 5: Add test for get_rate_limit auth required**

```rust
#[tokio::test]
async fn test_get_rate_limit_requires_auth() {
    let mut config = crate::config::IaConfig::default();
    config.general.host = "localhost".to_string();
    config.general.secure = false;
    let client = IaClient::from_config(config).unwrap();

    let result = get_rate_limit(&client, "derive.php").await;
    assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
}
```

- [ ] **Step 6: Run all core tasks tests**

Run: `cargo test -p ia-core -- tasks`
Expected: All pass.

- [ ] **Step 7: Commit**

```
git add ia-core/src/tasks.rs
git commit -m "test(tasks): add missing core tests (body verification, auto-append, auth, extra_params)"
```

---

## Chunk 2: CLI Fixes and New Flags

### Task 6: Add `--task-id`, `--since`, `--before` CLI flags

**Files:**
- Modify: `ia-cli/src/commands/tasks.rs`

- [ ] **Step 1: Add new flags to `TasksArgs`**

In `TasksArgs`, add after the `color` field:

```rust
/// Filter by task ID
#[arg(long)]
pub task_id: Option<u64>,

/// Show tasks submitted after this date/time
#[arg(long)]
pub since: Option<String>,

/// Show tasks submitted before this date/time
#[arg(long)]
pub before: Option<String>,
```

- [ ] **Step 2: Wire into the query in `run_list()`**

In `run_list()`, update the `TasksQuery` construction to include the new fields:

```rust
let query = TasksQuery {
    identifier: args.identifier.clone(),
    cmd: args.cmd.clone(),
    task_id: args.task_id,
    submitter: args.submitter.clone(),
    server: args.server.clone(),
    priority: args.priority,
    color: args.color.clone(),
    args: args.args.clone(),
    submittime_after: args.since.clone(),
    submittime_before: args.before.clone(),
    // ... rest unchanged
```

- [ ] **Step 3: Update `--completed-only` validation to check `task_id` field**

Replace:
```rust
if args.completed_only
    && args.identifier.is_none()
    && !extra_params.iter().any(|(k, _)| k == "task_id")
{
```

With:
```rust
if args.completed_only
    && args.identifier.is_none()
    && args.task_id.is_none()
    && !extra_params.iter().any(|(k, _)| k == "task_id")
{
```

- [ ] **Step 4: Run `cargo check -p ia-cli`**

Expected: Compiles without errors.

- [ ] **Step 5: Add CLI integration test for --task-id**

Add to `ia-cli/tests/tasks.rs`:

```rust
#[tokio::test]
async fn test_tasks_list_with_task_id_flag() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":1,"error":0,"paused":0}"#,
        r#"{"category":"catalog","task_id":12345,"identifier":"test-item","cmd":"derive.php","color":"blue"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "12345"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure", "-H", &host, "tasks", "--task-id", "12345", "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":12345"));
}
```

- [ ] **Step 6: Add CLI integration test for --since/--before**

```rust
#[tokio::test]
async fn test_tasks_list_with_date_range_flags() {
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

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "test-item",
            "--since",
            "2026-03-01",
            "--before",
            "2026-03-10",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
}
```

- [ ] **Step 7: Run CLI tests**

Run: `cargo test -p ia-cli -- tasks`
Expected: All pass.

- [ ] **Step 8: Commit**

```
git add ia-cli/src/commands/tasks.rs ia-cli/tests/tasks.rs
git commit -m "feat(tasks): add --task-id, --since, --before CLI flags for listing"
```

### Task 7: Validate `--args` format and reject `--wait` + batch

**Files:**
- Modify: `ia-cli/src/commands/tasks.rs`

- [ ] **Step 1: Add validation for `--args` format in `run_submit()`**

At the top of `run_submit()`, before parsing task_args, add:

```rust
// Validate --args format
for a in &args.task_args {
    if !a.contains('=') && !a.contains(':') {
        bail!(
            "invalid --args value '{}': expected KEY=VALUE format (e.g. --args remove_derived=\"*.jpg\")",
            a
        );
    }
}

// Validate --parameter format
for p in &args.parameter {
    if !p.contains('=') && !p.contains(':') {
        bail!(
            "invalid --parameter value '{}': expected KEY=VALUE format (e.g. -p history=1)",
            p
        );
    }
}
```

- [ ] **Step 2: Add validation for `--wait` + batch mode**

After collecting identifiers but before the single/batch branch, add:

```rust
if args.wait && identifiers.len() > 1 {
    bail!(
        "--wait is not supported in batch mode (got {} identifiers). \
         Submit individually with --wait, or use scripting to poll each task.",
        identifiers.len()
    );
}
```

- [ ] **Step 3: Add CLI test for malformed --args**

Add to `ia-cli/tests/tasks.rs`:

```rust
#[tokio::test]
async fn test_tasks_submit_malformed_args() {
    let output = ia_cmd()
        .args([
            "tasks",
            "submit",
            "derive",
            "my-item",
            "--args",
            "remove_derived",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("KEY=VALUE"),
        "should explain expected format, got: {stderr}"
    );
}
```

- [ ] **Step 4: Add CLI test for --wait + batch**

```rust
#[tokio::test]
async fn test_tasks_submit_wait_with_batch_errors() {
    let mock_server = MockServer::start().await;

    // We need the mock server for the test to not fail on connection
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 1, "log": "https://catalogd.archive.org/log/1" }
        })))
        .mount(&mock_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let itemlist_path = dir.path().join("items.txt");
    std::fs::write(&itemlist_path, "item-one\nitem-two\n").unwrap();

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "derive",
            "--itemlist",
            itemlist_path.to_str().unwrap(),
            "--wait",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not supported in batch mode"),
        "should reject --wait in batch mode, got: {stderr}"
    );
}
```

- [ ] **Step 5: Add CLI test for submit with --args and --comment**

```rust
#[tokio::test]
async fn test_tasks_submit_with_args_and_comment() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 4444, "log": "https://catalogd.archive.org/log/4444" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "submit",
            "derive",
            "my-item",
            "--args",
            "remove_derived=*.jpg",
            "--comment",
            "re-derive without JPEGs",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4444"));
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ia-cli -- tasks`
Expected: All pass.

- [ ] **Step 7: Commit**

```
git add ia-cli/src/commands/tasks.rs ia-cli/tests/tasks.rs
git commit -m "fix(tasks): validate --args KEY=VALUE format, reject --wait in batch mode"
```

### Task 8: Add spinner with run state during `--wait`

**Files:**
- Modify: `ia-cli/src/commands/tasks.rs`

- [ ] **Step 1: Add indicatif imports**

At the top of `ia-cli/src/commands/tasks.rs`, add:

```rust
use indicatif::{ProgressBar, ProgressStyle};
```

- [ ] **Step 2: Replace the `--wait` section in `run_submit()` with spinner-based polling**

Replace the `if args.wait { ... }` block (around lines 592-617) with:

```rust
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
        let query = ia_core::tasks::TasksQuery {
            task_id: Some(result.task_id),
            catalog: Some(true),
            ..Default::default()
        };

        let value = ia_core::tasks::get_tasks(client, &query).await?;
        let still_active = value.catalog.iter().any(|e| e.task_id == result.task_id);

        if !still_active {
            // Task is done
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
                let entry = value
                    .catalog
                    .iter()
                    .find(|e| e.task_id == result.task_id);
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
```

- [ ] **Step 3: Remove the import of `tasks::wait_for_task` if it's no longer used by the CLI**

Check if `wait_for_task` is used anywhere else in the CLI. If not, no import to remove — it's accessed as `tasks::wait_for_task` which is still a valid public function in ia-core.

- [ ] **Step 4: Run `cargo check -p ia-cli`**

Expected: Compiles without errors.

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All pass. The existing `test_tasks_submit_wait` test should still pass since it uses the JSON output path.

- [ ] **Step 6: Commit**

```
git add ia-cli/src/commands/tasks.rs
git commit -m "feat(tasks): add spinner with run state during --wait polling"
```

---

## Chunk 3: TUI, Docs, Remaining CLI Tests, and Final Polish

### Task 9: Optimize upload TUI query

**Files:**
- Modify: `ia-cli/src/tui/upload_app.rs`

- [ ] **Step 1: Add summary-only params to TUI query**

In the `tasks_handle` spawn block, update the `TasksQuery`:

```rust
&ia_core::tasks::TasksQuery {
    args: Some("*s3-put*".to_string()),
    submitter: submitter.clone(),
    catalog: Some(true),
    history: Some(false),
    summary: Some(true),
    limit: Some(1), // minimize data — we only need summary counts
    ..Default::default()
}
```

Note: We keep using `list_tasks` since the TUI only needs summary counts. Setting `limit=1` with `summary=true` and `history=false` minimizes the response size. `limit=1` means the server sends at most 1 catalog entry (plus the summary), rather than streaming all entries.

Wait — `list_tasks` forces `limit=0` when `limit.is_none()`, but when `limit=Some(1)`, it passes `1` to the server directly. That's the non-streaming JSON path (limit > 0 = standard JSON). Actually, looking at the code more carefully, `list_tasks` forces `limit=0` only when `streaming_query.limit.is_none()`. With `limit=Some(1)`, it will send `limit=1`, but then the response is standard JSON (not JSONL). The `list_tasks` streaming parser expects JSONL. This could fail.

Better approach: switch the TUI back to `get_tasks()` since it returns the standard JSON envelope with `TasksValue` which has the summary we need.

- [ ] **Step 1 (revised): Switch TUI back to `get_tasks()` for summary-only queries**

Replace the `list_tasks` call:

```rust
// Before:
if let Ok((summary, _catalog)) = ia_core::tasks::list_tasks(
    &tasks_client,
    &ia_core::tasks::TasksQuery {
        args: Some("*s3-put*".to_string()),
        submitter: submitter.clone(),
        ..Default::default()
    },
)
.await
{
    if let Ok(mut s) = tasks_state.lock() {
        s.tasks_queued = summary.queued;
        s.tasks_running = summary.running;
        s.tasks_error = summary.error;
    }
}

// After:
if let Ok(value) = ia_core::tasks::get_tasks(
    &tasks_client,
    &ia_core::tasks::TasksQuery {
        args: Some("*s3-put*".to_string()),
        submitter: submitter.clone(),
        catalog: Some(true),
        history: Some(false),
        ..Default::default()
    },
)
.await
{
    if let Ok(mut s) = tasks_state.lock() {
        s.tasks_queued = value.summary.queued;
        s.tasks_running = value.summary.running;
        s.tasks_error = value.summary.error;
    }
}
```

- [ ] **Step 2: Run `cargo check -p ia-cli`**

Expected: Compiles.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ia-cli`
Expected: All pass.

- [ ] **Step 4: Commit**

```
git add ia-cli/src/tui/upload_app.rs
git commit -m "perf(tui): use get_tasks() for upload TUI — only needs summary counts, not streaming"
```

### Task 10: Add remaining CLI integration tests

**Files:**
- Modify: `ia-cli/tests/tasks.rs`

- [ ] **Step 1: Add test for rerun with --identifier filter**

```rust
#[tokio::test]
async fn test_tasks_rerun_with_identifier_filter() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":0,"error":1,"paused":0}"#,
        r#"{"category":"catalog","task_id":777,"identifier":"target-item","cmd":"derive.php","color":"red"}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("identifier", "target-item"))
        .and(query_param("color", "red"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "777": "target-item" }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rerun",
            "--identifier",
            "target-item",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"task_id\":777"));
    assert!(stdout.contains("\"success\":true"));
}
```

- [ ] **Step 2: Add test for rate-limit with custom command**

```rust
#[tokio::test]
async fn test_tasks_rate_limit_custom_cmd() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("rate_limits", "1"))
        .and(query_param("cmd", "make_dark.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": {
                "cmd": "make_dark.php",
                "task_limits": 100,
                "tasks_inflight": 5,
                "tasks_blocked_by_offline": 0
            }
        })))
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "rate-limit",
            "make_dark",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"cmd\":\"make_dark.php\""));
    assert!(stdout.contains("\"task_limits\":100"));
}
```

- [ ] **Step 3: Add test for --completed-only with --task-id (should succeed)**

```rust
#[tokio::test]
async fn test_tasks_completed_only_with_task_id() {
    let mock_server = MockServer::start().await;

    let body = [
        r#"{"category":"summary","queued":0,"running":0,"error":0,"paused":0}"#,
        r#"{"category":"history","task_id":99999,"identifier":"done-item","cmd":"derive.php","color":"done","finished":85575266}"#,
    ]
    .join("\n");

    Mock::given(method("GET"))
        .and(path("/services/tasks.php"))
        .and(query_param("task_id", "99999"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json-l"),
        )
        .mount(&mock_server)
        .await;

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "tasks",
            "--completed-only",
            "--task-id",
            "99999",
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "--completed-only with --task-id should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p ia-cli -- tasks`
Expected: All pass.

- [ ] **Step 5: Commit**

```
git add ia-cli/tests/tasks.rs
git commit -m "test(tasks): add CLI tests for rerun --identifier, rate-limit custom cmd, completed-only + task-id"
```

### Task 11: Update design doc

**Files:**
- Modify: `docs/plans/2026-03-10-tasks-command-design.md`

- [ ] **Step 1: Update `TasksQuery` struct in design doc**

In the design doc's "New/Modified Types" section, add to the `TasksQuery` struct:

```rust
pub task_id: Option<u64>,
pub submittime_after: Option<String>,
pub submittime_before: Option<String>,
```

- [ ] **Step 2: Add `--task-id`, `--since`, `--before` to the listing flags table**

Add rows:

```
| `--task-id` | `u64` | Filter by specific task ID |
| `--since` | `String` | Show tasks submitted after this date/time |
| `--before` | `String` | Show tasks submitted before this date/time |
```

- [ ] **Step 3: Update the `reduced_priority` header reference**

Change `x-archive-queue-derive` to `X-Accept-Reduced-Priority` in the submit_task doc comment.

- [ ] **Step 4: Note `--wait` batch mode restriction**

Add to the submit section: "`--wait` is not supported in batch mode — an error is returned with a helpful message."

- [ ] **Step 5: Commit**

```
git add docs/plans/2026-03-10-tasks-command-design.md
git commit -m "docs(tasks): update design doc with --task-id, --since, --before, header fix, --wait batch restriction"
```

### Task 12: Update usage.md with new flags

**Files:**
- Modify: `docs/usage.md`

- [ ] **Step 1: Add `--task-id`, `--since`, `--before` to listing flags table**

In the listing flags table, add:

```
| `--task-id <ID>` | Filter by specific task ID |
| `--since <DATE>` | Show tasks submitted after this date/time |
| `--before <DATE>` | Show tasks submitted before this date/time |
```

- [ ] **Step 2: Add examples using the new flags**

```sh
# Show tasks submitted in the last hour
ia tasks --since "1 hour ago"

# Show a specific task's status
ia tasks --task-id 101247325
```

- [ ] **Step 3: Commit**

```
git add docs/usage.md
git commit -m "docs: add --task-id, --since, --before to usage.md"
```

### Task 13: Final verification

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

- [ ] **Step 5: Push to PR branch**

```
git push origin fix/tasks-fixes:feat/tasks-command
```

Wait for CI to pass, then mark PR ready for final review.
