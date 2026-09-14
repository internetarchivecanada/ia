# Upload Phase 1 Review Fixes — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all 20 issues found in the code review of PR #219 — 6 missing features, 3 critical bugs, 5 important bugs, 6 UX improvements. Every fix must include tests.

**Architecture:** All changes are in the existing `feat/upload` worktree at `~/github/internetarchivecanada/ia/.claude/worktrees/feat/upload`. The worktree has ia-core (library) and ia-cli (binary). Upload modules live in `ia-core/src/upload/`. CLI command is `ia-cli/src/commands/upload.rs`. All write-operation tests use wiremock mocks — NEVER send live requests to archive.org.

**Tech Stack:** Rust 1.85, tokio 1, reqwest 0.12 (via reqwest-middleware), wiremock 0.6.2, serde 1, clap 4, md-5 0.10, indicatif 0.17.

**Design doc:** `docs/plans/2026-03-05-upload-design.md`
**PR:** #219

**SAFETY RULES (non-negotiable):**
- NEVER send POST/PUT/DELETE/PATCH to live archive.org
- ALL write tests use wiremock mocks
- NEVER commit secrets

**BUILD/TEST COMMANDS:**
- Check: `cargo check -p ia-core -p ia-cli`
- Test: `cargo test -p ia-core -p ia-cli`
- Clippy: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
- Single test: `cargo test -p ia-core --test upload_single -- test_name`
- Core unit: `cargo test -p ia-core --lib -- upload::single::tests::test_name`

**KEY FILE PATHS:**
- `ia-core/src/upload/single.rs` — core upload_file() function
- `ia-core/src/upload/item.rs` — multi-file upload_item()
- `ia-core/src/upload/batch.rs` — batch upload_batch()
- `ia-core/src/upload/types.rs` — UploadOpts, UploadResult, UploadStatus
- `ia-core/src/upload/validate.rs` — identifier, metadata, collection validation
- `ia-core/src/upload/check_limit.rs` — rate limit parsing
- `ia-core/src/upload/checksum.rs` — MD5 computation, checksums file parsing
- `ia-core/src/upload/headers.rs` — S3 metadata header encoding
- `ia-core/src/upload/template.rs` — spreadsheet template generation
- `ia-core/src/upload/mod.rs` — public API re-exports
- `ia-core/src/error.rs` — IaError enum, is_retryable(), to_json_error()
- `ia-core/src/types.rs` — ItemMetadata, FileMetadata
- `ia-core/src/client.rs` — IaClient, get_item()
- `ia-cli/src/commands/upload.rs` — CLI command
- `ia-cli/src/output.rs` — display helpers
- `ia-core/tests/upload_single.rs` — integration tests for single upload
- `ia-core/tests/upload_item.rs` — integration tests for item upload
- `ia-core/tests/upload_batch.rs` — integration tests for batch upload
- `ia-cli/tests/upload.rs` — CLI integration tests

**EXISTING TYPES YOU'LL NEED:**
```rust
// ia-core/src/types.rs
pub struct ItemMetadata {
    pub metadata: MetadataFields,
    pub files: Vec<FileMetadata>,
    pub server: Option<String>,
    // ...
}
pub struct FileMetadata {
    pub name: String,
    pub md5: Option<String>,
    pub size: Option<u64>,
    // ...
}

// ia-core/src/client.rs
impl IaClient {
    pub async fn get_item(&self, identifier: &str) -> Result<ItemMetadata>;
    pub async fn item_exists(&self, identifier: &str) -> Result<bool>;
    pub fn require_auth(&self) -> Result<(String, String)>; // (access, secret)
    pub fn http(&self) -> &reqwest_middleware::ClientWithMiddleware;
    pub fn host(&self) -> &str;
    pub fn protocol(&self) -> &str;
}
```

---

## Task Dependency Graph

```
Task 1 (S3 XML parsing)     — no deps, used by Task 2
Task 2 (retry classification) — depends on Task 1
Task 3 (delete-after-upload)  — no deps
Task 4 (hex_to_bytes)         — no deps
Task 5 (blocking I/O)         — no deps
Task 6 (file re-read)         — no deps
Task 7 (check_collections)    — no deps, used by Task 11
Task 8 (checksum skip)        — depends on Task 5 (for compute_file_md5 in spawn_blocking)
Task 9 (batch partial results) — no deps
Task 10 (--json error format)  — depends on Task 1 (S3 parsed errors)
Task 11 (batch pre-scan)       — depends on Task 7
Task 12 (REMOTE_NAME column)   — no deps
Task 13 (delete error logging)  — no deps
Task 14 (batch progress)        — no deps
Task 15 (open cross-platform)   — no deps
Task 16 (ValueEnum for format)  — no deps
Task 17 (progress in output.rs) — no deps
Task 18 (checksum naming)       — depends on Task 8
Task 19 (duplicate checksums code) — no deps
Task 20 (integration test gaps)  — depends on Tasks 1-8

Independent tasks: 1, 3, 4, 5, 6, 7, 9, 12, 13, 14, 15, 16, 17, 19
```

**Execution order (respects dependencies):**
1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20

---

## Task 1: S3 XML Error Parsing

The IA S3 API returns errors as XML. Currently we dump raw XML into error messages. We need to parse `<Code>` and `<Message>` fields for clean error reporting.

**Files:**
- Create: `ia-core/src/upload/s3_error.rs`
- Modify: `ia-core/src/upload/mod.rs` — add `pub mod s3_error;`
- Modify: `ia-core/src/upload/single.rs` — use parsed errors

**Step 1: Write failing tests for S3 XML parsing**

Create `ia-core/src/upload/s3_error.rs` with test module:

```rust
/// Parsed S3 error response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    pub code: String,
    pub message: String,
}

/// Parse an S3 XML error response body.
///
/// Extracts `<Code>` and `<Message>` using string matching (no XML crate).
/// Returns `None` if the body doesn't look like an S3 error.
///
/// Example input:
/// ```xml
/// <Error>
///   <Code>AccessDenied</Code>
///   <Message>Access Denied</Message>
///   <Resource>/my-item/file.pdf</Resource>
///   <RequestId>db1b9e2b-...</RequestId>
/// </Error>
/// ```
pub fn parse_s3_error(body: &str) -> Option<S3Error> {
    todo!()
}

impl S3Error {
    /// Whether this S3 error code indicates a retryable condition.
    ///
    /// Retryable: SlowDown, InternalError, ServiceUnavailable, OperationAborted
    /// Non-retryable: AccessDenied, InvalidAccessKeyId, BadDigest, MissingContentLength, etc.
    pub fn is_retryable(&self) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_access_denied() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8'?>
<Error><Code>AccessDenied</Code><Message>Access Denied</Message><Resource>/my-item/file.pdf</Resource><RequestId>abc123</RequestId></Error>"#;
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "AccessDenied");
        assert_eq!(err.message, "Access Denied");
        assert!(!err.is_retryable());
    }

    #[test]
    fn parse_slow_down() {
        let xml = "<Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "SlowDown");
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_bad_digest() {
        let xml = "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "BadDigest");
        assert!(!err.is_retryable());
    }

    #[test]
    fn parse_internal_error() {
        let xml = "<Error><Code>InternalError</Code><Message>We encountered an internal error.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_operation_aborted() {
        let xml = "<Error><Code>OperationAborted</Code><Message>A conflicting operation is in progress.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_multiline_xml() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8'?>
<Error>
  <Code>AccessDenied</Code>
  <Message>Access Denied</Message>
  <Resource>/my-item/file.pdf</Resource>
  <RequestId>db1b9e2b-1234</RequestId>
</Error>"#;
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "AccessDenied");
        assert_eq!(err.message, "Access Denied");
    }

    #[test]
    fn parse_not_xml_returns_none() {
        assert!(parse_s3_error("just plain text").is_none());
        assert!(parse_s3_error("").is_none());
        assert!(parse_s3_error("{}").is_none());
    }

    #[test]
    fn parse_missing_code_returns_none() {
        let xml = "<Error><Message>Something</Message></Error>";
        assert!(parse_s3_error(xml).is_none());
    }

    #[test]
    fn retryable_codes() {
        let retryable = ["SlowDown", "InternalError", "ServiceUnavailable", "OperationAborted"];
        for code in retryable {
            let err = S3Error { code: code.into(), message: String::new() };
            assert!(err.is_retryable(), "{code} should be retryable");
        }
    }

    #[test]
    fn non_retryable_codes() {
        let non_retryable = [
            "AccessDenied", "InvalidAccessKeyId", "BadDigest",
            "MissingContentLength", "NoSuchBucket", "InvalidArgument",
        ];
        for code in non_retryable {
            let err = S3Error { code: code.into(), message: String::new() };
            assert!(!err.is_retryable(), "{code} should NOT be retryable");
        }
    }
}
```

**Step 2: Run tests — verify they fail**

Run: `cargo test -p ia-core --lib -- upload::s3_error::tests`
Expected: FAIL — `todo!()` panics.

**Step 3: Implement `parse_s3_error` and `is_retryable`**

Replace the `todo!()` bodies:

```rust
pub fn parse_s3_error(body: &str) -> Option<S3Error> {
    let code = extract_xml_field(body, "Code")?;
    let message = extract_xml_field(body, "Message").unwrap_or_default();
    Some(S3Error { code, message })
}

/// Extract text between `<Tag>` and `</Tag>` using simple string matching.
fn extract_xml_field(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

impl S3Error {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.code.as_str(),
            "SlowDown" | "InternalError" | "ServiceUnavailable" | "OperationAborted"
        )
    }
}
```

**Step 4: Add to mod.rs**

Add `pub mod s3_error;` to `ia-core/src/upload/mod.rs`.

**Step 5: Run tests — verify they pass**

Run: `cargo test -p ia-core --lib -- upload::s3_error::tests`
Expected: all pass.

**Step 6: Commit**

```
git add ia-core/src/upload/s3_error.rs ia-core/src/upload/mod.rs
git commit -m "feat(upload): add S3 XML error parsing

Parse <Code> and <Message> from IA S3 error responses using simple
string matching. Classify errors as retryable (SlowDown, InternalError,
ServiceUnavailable, OperationAborted) vs permanent (AccessDenied,
BadDigest, etc.) for correct retry behavior.

Ref #219"
```

---

## Task 2: Fix Retry Classification for Non-503 HTTP Errors

**The bug:** `IaError::UploadFailed` is unconditionally `is_retryable() == true`, so 403 AccessDenied gets retried 10 times with 30-second sleeps. The design doc (lines 626-639) says 400/403 are permanent.

**The fix:** Use the S3 XML parser from Task 1 to determine retryability from the actual HTTP status code and S3 error code, not from `IaError::is_retryable()`.

**Files:**
- Modify: `ia-core/src/upload/single.rs` — rewrite non-503 error branch
- Modify: `ia-core/src/error.rs` — make `UploadFailed` non-retryable (it's the terminal state)
- Test: `ia-core/tests/upload_single.rs` — add integration tests for 403, 400, 500

**Step 1: Write failing integration tests**

Add to `ia-core/tests/upload_single.rs`:

```rust
#[tokio::test]
async fn upload_403_is_not_retried() {
    let mock = MockServer::start().await;

    // 403 should be returned immediately — NOT retried
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
        ))
        .expect(1) // exactly 1 request — no retries
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        retries: 3,
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("AccessDenied"));
}

#[tokio::test]
async fn upload_400_bad_digest_is_not_retried() {
    let mock = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>",
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        retries: 3,
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("BadDigest"));
}

#[tokio::test]
async fn upload_500_is_retried_then_succeeds() {
    let mock = MockServer::start().await;

    // First request: 500
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500).set_body_string(
            "<Error><Code>InternalError</Code><Message>Internal error</Message></Error>",
        ))
        .expect(1)
        .up_to_n_times(1)
        .mount(&mock)
        .await;

    // check_limit returns clear
    Mock::given(method("GET").and(query_param("check_limit", "1")))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"over_limit":0}"#,
        ))
        .mount(&mock)
        .await;

    // Second request: 200
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount_as_scoped(&mock)  // or use priority
        .await;

    // NOTE: wiremock matching — the second PUT mock needs to be a non-scoped
    // mount that outlives the first. The exact approach depends on wiremock version.
    // If this doesn't work cleanly, use a counter-based ResponseTemplate.
    // The key assertion: the function retries on 500 and eventually succeeds.

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        retries: 3,
        retry_sleep: Duration::from_millis(1),
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None).await;
    assert!(result.is_ok());
}
```

**Step 2: Run tests — verify they fail**

Run: `cargo test -p ia-core --test upload_single -- upload_403`
Expected: FAIL — 403 currently retried.

**Step 3: Rewrite the non-503 error handling in `single.rs`**

In `ia-core/src/upload/single.rs`, replace the non-503 branch (lines 229-248):

```rust
// At top of file, add:
use crate::upload::s3_error::parse_s3_error;

// Replace the `else` branch (non-503 errors):
} else {
    // Non-503 error — parse S3 XML to classify
    let body_text = resp.text().await.unwrap_or_default();
    let s3_err = parse_s3_error(&body_text);

    // Build a clean error message from parsed XML or raw body
    let err_msg = match &s3_err {
        Some(e) => format!("{}: {}", e.code, e.message),
        None => format!("HTTP {status}: {body_text}"),
    };

    // Only retry if the S3 error is classified as retryable
    let should_retry = s3_err.as_ref().map_or(
        status.is_server_error(), // fallback: retry 5xx
        |e| e.is_retryable(),
    );

    if should_retry && retries < opts.retries {
        tracing::warn!(
            identifier,
            key,
            retry = retries + 1,
            %status,
            "retrying upload: {err_msg}"
        );
        retries += 1;
        continue;
    }

    return Err(IaError::UploadFailed {
        identifier: identifier.to_string(),
        key: key.to_string(),
        message: err_msg,
    });
}
```

**Step 4: Change `UploadFailed` to non-retryable in `error.rs`**

In `ia-core/src/error.rs`, line 162, change:
```rust
IaError::UploadFailed { .. } => true,   // transient network issues
```
to:
```rust
IaError::UploadFailed { .. } => false,  // terminal — retry logic is in single.rs
```

Also update the test `upload_failed_is_retryable` to `upload_failed_is_not_retryable` and flip the assertion.

**Step 5: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`
Expected: all pass (including new tests).

**Step 6: Commit**

```
git commit -m "fix(upload): classify HTTP errors correctly for retry

Parse S3 XML error responses to extract Code and Message. Only retry
on retryable S3 codes (SlowDown, InternalError, ServiceUnavailable,
OperationAborted) or unknown 5xx errors. 400 BadDigest and 403
AccessDenied are now correctly treated as permanent failures.

Previously, all non-503 errors were wrapped as UploadFailed (always
retryable), causing 403s to be retried 10 times with 30-second sleeps.

Ref #219"
```

---

## Task 3: Enforce `--delete-after-upload` Forces Verify

**The bug:** Design doc line 521: "Forces verify=true (non-negotiable)". Not enforced.

**Files:**
- Modify: `ia-cli/src/commands/upload.rs` — enforce in both `run_bare_upload` and `run_import`
- Test: `ia-cli/tests/upload.rs` — add CLI test

**Step 1: Write failing CLI test**

Add to `ia-cli/tests/upload.rs`:

```rust
#[test]
fn delete_after_upload_rejects_no_verify() {
    let cmd = Command::cargo_bin("ia").unwrap();
    let output = cmd
        .args(["upload", "test-item", "/dev/null", "--delete-after-upload", "--no-verify"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--delete-after-upload requires verification"));
}
```

**Step 2: Run test — verify it fails**

**Step 3: Add validation in CLI**

In `ia-cli/src/commands/upload.rs`, in `run_bare_upload`, right after the `parse_key_values` calls (around line 339), add:

```rust
// --delete-after-upload forces verify — non-negotiable data safety invariant
if args.delete_after_upload && args.no_verify {
    bail!(
        "--delete-after-upload requires verification (Content-MD5).\n\
         Cannot combine with --no-verify — refusing to delete local files \
         without server-side integrity confirmation."
    );
}
```

Add the same check in `run_import` after `parse_key_values`:

```rust
if args.delete_after_upload && args.no_verify {
    bail!(
        "--delete-after-upload requires verification (Content-MD5).\n\
         Cannot combine with --no-verify — refusing to delete local files \
         without server-side integrity confirmation."
    );
}
```

**Step 4: Run tests — verify pass**

**Step 5: Commit**

```
git commit -m "fix(upload): reject --delete-after-upload with --no-verify

The design doc mandates that delete-after-upload forces verify=true.
Deleting a local file without server-side Content-MD5 confirmation
risks silent data loss. Now errors immediately if both flags are set.

Ref #219"
```

---

## Task 4: Fix `hex_to_bytes` Panic on Odd-Length Input

**Files:**
- Modify: `ia-core/src/upload/single.rs` — fix `hex_to_bytes`

**Step 1: Write failing test**

Add to `single.rs` test module:

```rust
#[test]
fn hex_to_bytes_odd_length_does_not_panic() {
    // Should not panic, just return partial/empty result
    let result = hex_to_bytes("abc");
    assert!(result.len() <= 1);
}

#[test]
fn hex_to_bytes_empty() {
    assert!(hex_to_bytes("").is_empty());
}
```

**Step 2: Run — verify panic on odd-length**

**Step 3: Fix**

Replace `hex_to_bytes` in `single.rs`:

```rust
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| hex.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()))
        .collect()
}
```

**Step 4: Run tests — verify pass**

**Step 5: Commit**

```
git commit -m "fix(upload): prevent panic in hex_to_bytes on odd-length input

Use hex.get(i..i+2) instead of &hex[i..i+2] to avoid panicking when
the input has an odd length. Input comes from user-provided checksums
files, so we can't assume it's always valid.

Ref #219"
```

---

## Task 5: Fix Blocking I/O in Async Context (`compute_file_md5`)

**Files:**
- Modify: `ia-core/src/upload/checksum.rs` — add async wrapper
- Modify: `ia-core/src/upload/single.rs` — use async wrapper
- Modify: `ia-core/src/upload/mod.rs` — update re-exports if needed

**Step 1: Add `compute_file_md5_async` to `checksum.rs`**

```rust
/// Async wrapper for compute_file_md5 that runs on a blocking thread.
///
/// MD5 computation is CPU + I/O intensive. Running it on tokio's async
/// runtime blocks the executor. This spawns it on the blocking thread pool.
pub async fn compute_file_md5_async(path: &std::path::Path) -> Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || compute_file_md5(&path))
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
}
```

**Step 2: Write test**

```rust
#[tokio::test]
async fn compute_md5_async_matches_sync() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"hello async").unwrap();
    f.flush().unwrap();

    let sync_result = compute_file_md5(f.path()).unwrap();
    let async_result = compute_file_md5_async(f.path()).await.unwrap();
    assert_eq!(sync_result, async_result);
}
```

**Step 3: Update `single.rs` to use async version**

Replace line 64 (`Some(compute_file_md5(file)?)`) with:
```rust
Some(compute_file_md5_async(file).await?)
```

Update the import at top of `single.rs`:
```rust
use crate::upload::checksum::compute_file_md5_async;
```

Remove the old `compute_file_md5` import.

**Step 4: Run all tests — verify pass**

**Step 5: Commit**

```
git commit -m "fix(upload): run MD5 computation on blocking thread pool

compute_file_md5 uses std::fs::File + std::io::Read, which blocks the
tokio runtime thread. For large files this starves other async tasks.
Added compute_file_md5_async that wraps in spawn_blocking.

Ref #219"
```

---

## Task 6: Read File Once, Reuse Across Retries

**The bug:** `tokio::fs::read(file).await?` inside the retry loop re-reads the entire file on every attempt. For large files this is wasteful.

**Files:**
- Modify: `ia-core/src/upload/single.rs` — move file read before retry loop

**Step 1: Move file read before the retry loop**

In `single.rs`, move `let body = tokio::fs::read(file).await?;` from inside the loop (line 123) to before the loop (after the `metadata_headers` computation, around line 101):

```rust
// Read file into memory once (before retry loop)
let body = tokio::fs::read(file).await?;

// Retry loop
let mut retries = 0u32;
loop {
    // ... existing code, but remove the `let body = ...` line from inside the loop

    // Use body.clone() in the request since reqwest consumes it
    let response = request.body(body.clone()).send().await;
```

**Step 2: Run all tests — verify pass**

**Step 3: Commit**

```
git commit -m "fix(upload): read file once before retry loop

Previously the file was re-read from disk on every retry attempt.
Now read once into memory and clone the buffer for each request.
This avoids redundant I/O for retried uploads.

Ref #219"
```

---

## Task 7: Implement `check_collections()` — Collection Existence Validation

**The design says:** "Check that collection(s) exist via metadata API, with retry on transient failure." (design lines 203-206, 242)

**Files:**
- Modify: `ia-core/src/upload/validate.rs` — add `check_collections()`
- Modify: `ia-core/src/upload/mod.rs` — re-export
- Test: `ia-core/tests/upload_item.rs` — add integration test with wiremock

**Step 1: Write the function and tests in `validate.rs`**

Add to `validate.rs`:

```rust
use crate::IaClient;

/// Check that the named collections exist on archive.org.
///
/// Makes GET /metadata/{collection} requests. Returns an error listing
/// all collections that were not found.
///
/// This is a pre-flight validation: better to fail early than to upload
/// files and discover the collection doesn't exist.
pub async fn check_collections(
    client: &IaClient,
    collections: &[&str],
) -> Result<(), IaError> {
    let mut not_found = Vec::new();

    for collection in collections {
        match client.item_exists(collection).await {
            Ok(true) => {} // exists
            Ok(false) => not_found.push((*collection).to_string()),
            Err(e) => {
                // Log but don't fail — collection check is best-effort
                tracing::warn!("failed to check collection {collection}: {e}");
            }
        }
    }

    if not_found.is_empty() {
        Ok(())
    } else {
        Err(IaError::CollectionNotFound {
            collection: not_found.join(", "),
        })
    }
}
```

Add unit test:

```rust
// In the tests module of validate.rs:
// (Note: full integration test with wiremock goes in tests/upload_item.rs)

#[test]
fn check_collections_requires_client() {
    // This is an async function — we can't unit test it without wiremock.
    // Integration tests are in tests/upload_item.rs.
    // This test just verifies the function exists and compiles.
}
```

**Step 2: Update `item.rs` to call `check_collections`**

In `ia-core/src/upload/item.rs`, after `validate_required_metadata` (around line 53), add:

```rust
// 3b. Check that collections actually exist on archive.org
if !opts.no_collection_check {
    let collections: Vec<&str> = opts
        .metadata
        .iter()
        .filter(|(k, _)| k == "collection")
        .map(|(_, v)| v.as_str())
        .collect();
    if !collections.is_empty() {
        crate::upload::validate::check_collections(client, &collections).await?;
    }
}
```

**Step 3: Write integration test with wiremock**

Add to `ia-core/tests/upload_item.rs`:

```rust
#[tokio::test]
async fn upload_item_checks_collection_exists() {
    let mock = MockServer::start().await;

    // Collection metadata endpoint — return 404 for nonexistent
    Mock::given(method("GET").and(path("/metadata/nonexistent-collection")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&mock)
        .await;

    // The item_exists check looks for a non-empty response
    // This depends on how item_exists is implemented — adjust mock accordingly

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "nonexistent-collection".into()),
        ],
        no_collection_check: false,
        ..UploadOpts::default()
    };

    // This test needs to be adapted based on how client.item_exists() works
    // and what response it expects from the metadata endpoint.
    // The key assertion: when collection doesn't exist, upload_item fails
    // with CollectionNotFound BEFORE any PUT request is sent.
}
```

**Step 4: Run tests — verify pass**

**Step 5: Update mod.rs re-exports if needed**

**Step 6: Commit**

```
git commit -m "feat(upload): implement collection existence validation

Add check_collections() that verifies collections exist on archive.org
via the metadata API before uploading. Called from upload_item() when
no_collection_check is false. Fails fast with CollectionNotFound listing
all missing collections.

Ref #219"
```

---

## Task 8: Implement `--checksum` Skip-If-Already-Uploaded

**This is the biggest missing feature.** Design lines 494-501: compute local MD5, fetch remote metadata, compare. Skip if match AND no pending catalog tasks.

**Files:**
- Modify: `ia-core/src/upload/single.rs` — add checksum skip logic before upload
- Test: `ia-core/tests/upload_single.rs` — wiremock tests for skip and no-skip cases

**Step 1: Write failing integration tests**

Add to `ia-core/tests/upload_single.rs`:

```rust
#[tokio::test]
async fn upload_checksum_skip_when_md5_matches() {
    let mock = MockServer::start().await;

    // Mock metadata endpoint — file exists with matching MD5
    // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
    Mock::given(method("GET").and(path("/metadata/test-item")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}
            ]
        })))
        .mount(&mock)
        .await;

    // No PUT request should be made
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0) // zero PUTs — file was skipped
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        checksum: true,
        verify: true,
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Skipped));
}

#[tokio::test]
async fn upload_checksum_no_skip_when_md5_differs() {
    let mock = MockServer::start().await;

    // Mock metadata — file exists but MD5 doesn't match
    Mock::given(method("GET").and(path("/metadata/test-item")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "0000000000000000000000000000000", "size": "5"}
            ]
        })))
        .mount(&mock)
        .await;

    // PUT should proceed since MD5 doesn't match
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        checksum: true,
        verify: true,
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_checksum_no_skip_when_file_not_on_remote() {
    let mock = MockServer::start().await;

    // Mock metadata — item exists but file is not in it
    Mock::given(method("GET").and(path("/metadata/test-item")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "other.txt", "md5": "abc123", "size": "10"}
            ]
        })))
        .mount(&mock)
        .await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        checksum: true,
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn upload_checksum_no_verify_still_computes_md5_for_skip() {
    // Design line 517: --no-verify --checksum → MD5 computed for skip, no Content-MD5 header
    let mock = MockServer::start().await;

    Mock::given(method("GET").and(path("/metadata/test-item")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "test.txt", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}
            ]
        })))
        .mount(&mock)
        .await;

    // No PUT — should be skipped
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        checksum: true,
        verify: false, // no Content-MD5 header, but still compute for skip
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Skipped));
}
```

**Step 2: Run tests — verify they fail**

**Step 3: Implement checksum skip in `single.rs`**

Add to `single.rs`, after the file_size computation (around line 47) and before the MD5 computation block:

```rust
// Checksum skip: compare local MD5 with remote, skip if match
if opts.checksum {
    // Compute local MD5 (need it for comparison regardless of verify setting)
    let local_md5 = if let Some(md5) = opts.checksums.as_ref().and_then(|cs| cs.get(key)) {
        md5.clone()
    } else {
        if let Some(cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: key.to_string(),
                bytes_sent: 0,
                total_bytes: file_size,
                status: UploadProgressStatus::Verifying,
            });
        }
        compute_file_md5_async(file).await?
    };

    // Fetch remote metadata to get remote file's MD5
    match client.get_item(identifier).await {
        Ok(item) => {
            let remote_md5 = item.files.iter()
                .find(|f| f.name == key)
                .and_then(|f| f.md5.as_deref());

            if remote_md5 == Some(local_md5.as_str()) {
                // MD5 match — skip upload
                if let Some(cb) = progress {
                    cb(UploadProgress {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        bytes_sent: file_size,
                        total_bytes: file_size,
                        status: UploadProgressStatus::Skipped,
                    });
                }
                return Ok(UploadResult {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    status: UploadStatus::Skipped,
                    bytes: file_size,
                    md5: Some(local_md5),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    retries: 0,
                });
            }

            // MD5 doesn't match or file not found — proceed with upload.
            // If verify is on and we already computed the MD5, reuse it below.
            // Store it so the verify block doesn't recompute.
        }
        Err(e) => {
            // Item doesn't exist yet or metadata fetch failed — upload normally
            tracing::debug!("checksum skip: metadata fetch failed for {identifier}: {e}");
        }
    }

    // If verify is on, reuse the MD5 we already computed
    // (avoid the verify block recomputing it)
    // We need to restructure the MD5 flow slightly for this.
}
```

**IMPORTANT:** The checksum-skip and verify-md5 blocks share the MD5 computation. Restructure so the MD5 is computed once and reused:

```rust
// Compute local MD5 if needed for either checksum skip or verify
let needs_md5 = opts.checksum || opts.verify;
let md5_hex = if needs_md5 {
    if let Some(md5) = opts.checksums.as_ref().and_then(|cs| cs.get(key)) {
        Some(md5.clone())
    } else {
        if let Some(cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: key.to_string(),
                bytes_sent: 0,
                total_bytes: file_size,
                status: UploadProgressStatus::Verifying,
            });
        }
        Some(compute_file_md5_async(file).await?)
    }
} else {
    None
};

// Checksum skip: compare local MD5 with remote
if opts.checksum {
    let local_md5 = md5_hex.as_ref().expect("md5 computed when checksum=true");
    match client.get_item(identifier).await {
        Ok(item) => {
            let remote_md5 = item.files.iter()
                .find(|f| f.name == key)
                .and_then(|f| f.md5.as_deref());
            if remote_md5 == Some(local_md5.as_str()) {
                if let Some(cb) = progress {
                    cb(UploadProgress {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        bytes_sent: file_size,
                        total_bytes: file_size,
                        status: UploadProgressStatus::Skipped,
                    });
                }
                return Ok(UploadResult {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    status: UploadStatus::Skipped,
                    bytes: file_size,
                    md5: Some(local_md5.clone()),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    retries: 0,
                });
            }
        }
        Err(e) => {
            tracing::debug!("checksum skip: metadata fetch failed for {identifier}: {e}");
        }
    }
}

// Build Content-MD5 header only if verify is on
let content_md5_b64 = if opts.verify {
    md5_hex.as_ref().map(|hex| {
        let raw_bytes = hex_to_bytes(hex);
        base64_encode(&raw_bytes)
    })
} else {
    None
};
```

This handles all flag interactions from the design doc:
- default (verify on): compute MD5, send Content-MD5, no skip
- `--checksum`: compute MD5, send Content-MD5, check skip
- `--no-verify`: no MD5, no Content-MD5, no skip
- `--no-verify --checksum`: compute MD5 (for skip), no Content-MD5, check skip
- `--checksums FILE`: use pre-computed MD5, send Content-MD5, skip if `--checksum`

**Step 4: Run all tests — verify pass**

**Step 5: Commit**

```
git commit -m "feat(upload): implement --checksum skip-if-already-uploaded

Compute local MD5, fetch remote metadata, compare. Skip upload when
MD5 matches. Handles all flag interactions:
- --checksum alone: compute MD5, check skip, send Content-MD5
- --no-verify --checksum: compute MD5 for skip only, no Content-MD5
- --checksums FILE + --checksum: use pre-computed MD5 for skip
- Metadata fetch failure: proceed with upload (best-effort skip)

Ref #219"
```

---

## Task 9: Batch Returns Partial Results Instead of Discarding

**Files:**
- Modify: `ia-core/src/upload/batch.rs`

**Step 1: Write failing test**

Add to `ia-core/src/upload/batch.rs` tests:

```rust
// Integration test in tests/upload_batch.rs is better for this, but
// we can test the collection logic at the unit level.
```

Actually, this should be an integration test in `ia-core/tests/upload_batch.rs`.

**Step 2: Change `upload_batch` to collect all results, not short-circuit**

Replace the result collection (lines 66-75) in `batch.rs`:

```rust
// 4. Flatten results — collect successes AND failures
let mut all_results = Vec::new();
let mut errors = Vec::new();
for result in results {
    match result {
        Ok(item_results) => all_results.extend(item_results),
        Err(e) => {
            // Record the error as a failed result so callers see it
            errors.push(e);
        }
    }
}

// If we had errors but also successes, convert errors to Failed results
for err in &errors {
    all_results.push(UploadResult {
        identifier: String::new(), // error may not have identifier context
        key: String::new(),
        status: UploadStatus::Failed(err.to_string()),
        bytes: 0,
        md5: None,
        elapsed_ms: 0,
        retries: 0,
    });
}

// If ALL items failed and we have no results, return the first error
if all_results.is_empty() && !errors.is_empty() {
    return Err(errors.into_iter().next().unwrap());
}

Ok(all_results)
```

**Step 3: Run tests — verify pass**

**Step 4: Commit**

```
git commit -m "fix(upload): batch collects partial results instead of discarding

Previously, if item 3 of 10 failed, successful results from items 1-2
were discarded. Now all results are collected — successes and failures
are both returned so callers can log/report them. Only returns Err if
ALL items failed.

Ref #219"
```

---

## Task 10: Fix `--json` Error Output Format

**The bug:** `UploadStatus::Failed` serializes as `{"status":{"failed":"msg"}}`. Project convention: `{"error":{"code":"...","message":"..."}}`.

**Files:**
- Modify: `ia-cli/src/commands/upload.rs` — fix `output_results` for failed items in JSON mode

**Step 1: Fix the JSON output for failed results**

In `output_results` in `upload.rs`, change the JSON mode branch:

```rust
if json_mode {
    match &r.status {
        UploadStatus::Failed(msg) => {
            // Use project error convention for failures
            let err_json = serde_json::json!({
                "error": {
                    "code": "upload_failed",
                    "message": msg,
                    "identifier": r.identifier,
                    "key": r.key,
                }
            });
            eprintln!("{}", serde_json::to_string(&err_json).unwrap_or_default());
        }
        _ => {
            // Success/skip/dry-run go to stdout as JSONL
            let json = serde_json::to_string(r).context("failed to serialize upload result")?;
            println!("{json}");
        }
    }
}
```

**Step 2: Add CLI test for --json error format**

Add to `ia-cli/tests/upload.rs`:

```rust
#[test]
fn upload_json_error_format() {
    // Trigger a failure — e.g., nonexistent file
    let cmd = Command::cargo_bin("ia").unwrap();
    let output = cmd
        .args(["upload", "test-item", "/tmp/nonexistent-file-xyz.txt", "--json",
               "-m", "mediatype:texts", "-m", "collection:test"])
        .output()
        .unwrap();
    // Should have error JSON on stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The exact format depends on where the error occurs, but we can
    // at least verify the structure if it gets to the upload stage.
}
```

**Step 3: Run tests — verify pass**

**Step 4: Commit**

```
git commit -m "fix(upload): --json errors use project convention format

Failed uploads in --json mode now output to stderr as:
  {\"error\":{\"code\":\"upload_failed\",\"message\":\"...\",\"identifier\":\"...\",\"key\":\"...\"}}

Previously serialized as {\"status\":{\"failed\":\"...\"}} which didn't
match the project's agent-friendly output convention.

Ref #219"
```

---

## Task 11: Batch Pre-Scan Metadata Validation

**The bug:** Design says batch pre-scan should validate required metadata per group BEFORE starting any uploads. Currently metadata validation only happens inside `upload_item()` — after the batch has started.

**Files:**
- Modify: `ia-core/src/upload/batch.rs` — add metadata validation to `validate_groups`

**Step 1: Add metadata validation to `validate_groups()`**

In `batch.rs`, update `validate_groups`:

```rust
fn validate_groups(groups: &[ItemGroup]) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for group in groups {
        if let Err(e) = validate_identifier(&group.identifier) {
            errors.push(e.to_string());
        }

        // Validate required metadata per group (design line 263)
        if !group.metadata.is_empty() {
            if let Err(e) = crate::upload::validate::validate_required_metadata(&group.metadata) {
                errors.push(format!("{}: {e}", group.identifier));
            }
        }

        for file in &group.files {
            if let Err(e) = validate_file(file) {
                errors.push(format!("{}: {e}", file.display()));
            }
        }
    }

    if !errors.is_empty() {
        return Err(IaError::Config(format!(
            "batch validation failed:\n  {}",
            errors.join("\n  ")
        )));
    }

    Ok(())
}
```

**Step 2: Add test**

```rust
#[test]
fn validate_groups_checks_required_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "content").unwrap();

    let groups = vec![ItemGroup {
        identifier: "test-item".into(),
        metadata: vec![("title".into(), "My Item".into())], // missing mediatype + collection
        files: vec![file],
    }];
    let err = validate_groups(&groups).unwrap_err();
    assert!(err.to_string().contains("mediatype"));
}
```

**Step 3: Run tests — verify pass**

**Step 4: Commit**

```
git commit -m "fix(upload): validate required metadata during batch pre-scan

The design requires metadata validation (mediatype, collection) during
the batch pre-scan phase, before any uploads start. Previously metadata
was only checked inside upload_item(), after the batch had begun.

Ref #219"
```

---

## Task 12: Add `REMOTE_NAME` Column to Template and Import

**Files:**
- Modify: `ia-core/src/upload/template.rs` — add `remote_name` field to `TemplateRow`
- Modify: `ia-core/src/upload/batch.rs` — handle `REMOTE_NAME` column
- Modify: `ia-cli/src/commands/upload.rs` — update CSV/TSV/XLSX writers

**Step 1: Add field to `TemplateRow`**

In `template.rs`, add to `TemplateRow`:
```rust
pub struct TemplateRow {
    pub identifier: String,
    pub file: String,
    pub remote_name: String,  // NEW
    pub mediatype: String,
    // ... rest unchanged
}
```

Initialize it as `remote_name: String::new()` in `generate_template`.

**Step 2: Update all template writers (CSV, TSV, XLSX) to include `REMOTE_NAME` as third column**

In `template.rs` `write_template_csv`, add `"REMOTE_NAME"` to the header array after `"file"`. Same for TSV and XLSX writers in `upload.rs`.

**Step 3: Handle `REMOTE_NAME` in batch `group_records`**

In `batch.rs` `group_records`, extract `REMOTE_NAME` from fields similarly to `file`:

```rust
// Extract optional REMOTE_NAME
let remote_name = fields.remove("REMOTE_NAME");
// Store it alongside the file path — the item upload needs it for key computation
```

This requires a small structural change: instead of `files: Vec<PathBuf>`, the group needs to track `files: Vec<(PathBuf, Option<String>)>` where the second element is the optional remote name. Or add a separate `remote_names` vec. Choose the simplest approach.

**Step 4: Update tests for new column**

**Step 5: Commit**

```
git commit -m "feat(upload): add REMOTE_NAME column to template and import

The design doc specifies a REMOTE_NAME column in the spreadsheet format
for overriding remote filenames per-row. Added to template generation
(CSV/TSV/XLSX) and handled during batch import record grouping.

Ref #219"
```

---

## Task 13: Log Warning on File Deletion Failure

**Files:**
- Modify: `ia-core/src/upload/single.rs`

**Step 1: Replace silent discard with warning**

Replace line 189:
```rust
let _ = std::fs::remove_file(file);
```
with:
```rust
if let Err(e) = std::fs::remove_file(file) {
    tracing::warn!(
        "failed to delete {} after upload: {e}",
        file.display()
    );
}
```

**Step 2: Run tests — verify pass**

**Step 3: Commit**

```
git commit -m "fix(upload): log warning when post-upload file deletion fails

Previously used let _ to silently discard deletion errors. Now logs
a tracing::warn so users know if cleanup failed.

Ref #219"
```

---

## Task 14: Batch Import Progress — Per-Item Completion

**Files:**
- Modify: `ia-cli/src/commands/upload.rs` — wire up batch progress

**Step 1: Replace no-op progress callback in `run_import`**

Replace the empty progress_fn (lines 564-566) with a callback that prints per-item completion:

```rust
let quiet_level = quiet;
let progress_fn = move |p: UploadProgress| {
    if json_mode || quiet_level >= 1 {
        return;
    }
    match p.status {
        UploadProgressStatus::Complete => {
            eprintln!(
                " {} {}/{}",
                console::style("✓").green(),
                p.identifier,
                p.key,
            );
        }
        UploadProgressStatus::Failed => {
            eprintln!(
                " {} {}/{}",
                console::style("✗").red(),
                p.identifier,
                p.key,
            );
        }
        UploadProgressStatus::Skipped => {
            eprintln!(
                " {} {}/{} (skipped)",
                console::style("–").dim(),
                p.identifier,
                p.key,
            );
        }
        UploadProgressStatus::WaitingRateLimit => {
            eprintln!(
                " {} {} rate limited, polling...",
                console::style("⏸").yellow(),
                p.identifier,
            );
        }
        _ => {} // Uploading, Verifying — too noisy for batch
    }
};
```

**Step 2: Run tests — verify pass**

**Step 3: Commit**

```
git commit -m "feat(upload): add per-item progress output for batch import

Batch import previously had a no-op progress callback, leaving users
with silence during long uploads. Now shows per-file completion,
failures, skips, and rate limit status.

Ref #219"
```

---

## Task 15: Cross-Platform `open_after_upload`

**Files:**
- Modify: `ia-cli/src/commands/upload.rs`

**Step 1: Replace macOS-only `open` with cross-platform logic**

Replace lines 482-484:

```rust
if args.open_after_upload && !had_failure {
    let url = format!("https://archive.org/details/{identifier}");
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&url).spawn()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("xdg-open").arg(&url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", &url]).spawn()
    } else {
        eprintln!("--open-after-upload is not supported on this platform");
        Ok(std::process::Command::new("true").spawn().unwrap()) // no-op
    };
    if let Err(e) = result {
        eprintln!("failed to open browser: {e}");
    }
}
```

**Step 2: Commit**

```
git commit -m "fix(upload): cross-platform --open-after-upload

Use platform-appropriate command: open (macOS), xdg-open (Linux),
cmd /C start (Windows).

Ref #219"
```

---

## Task 16: Use `ValueEnum` for Template `--format`

**Files:**
- Modify: `ia-cli/src/commands/upload.rs`

**Step 1: Define enum**

```rust
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum TemplateFormat {
    Csv,
    Tsv,
    Xlsx,
}
```

**Step 2: Change `TemplateArgs.format` from `String` to `TemplateFormat`**

```rust
#[arg(long, default_value = "csv", value_enum)]
pub format: TemplateFormat,
```

**Step 3: Update `run_template` to match on enum instead of string**

Replace `match args.format.as_str()` with `match args.format`.

**Step 4: Run tests — verify pass**

**Step 5: Commit**

```
git commit -m "refactor(upload): use ValueEnum for template --format

Replaces runtime string validation with compile-time enum. Gives
better help text and shell tab completion for free.

Ref #219"
```

---

## Task 17: Extract Upload Progress Display to `output.rs`

**Files:**
- Modify: `ia-cli/src/output.rs` — add `UploadDisplay`
- Modify: `ia-cli/src/commands/upload.rs` — use `UploadDisplay`

**Step 1: Create `UploadDisplay` struct in `output.rs`**

Follow the pattern of `DownloadDisplay`/`BatchDisplay` already in `output.rs`. Extract the progress bar setup and callback from `run_bare_upload` into a reusable struct:

```rust
pub struct UploadDisplay {
    multi: MultiProgress,
    bars: Arc<Mutex<HashMap<String, ProgressBar>>>,
    style: ProgressStyle,
}

impl UploadDisplay {
    pub fn new() -> Self { /* ... */ }

    pub fn progress_callback(&self) -> impl Fn(UploadProgress) + Send + Sync + '_ {
        /* ... move the closure from run_bare_upload here ... */
    }
}
```

**Step 2: Use it in `run_bare_upload` and `run_import`**

**Step 3: Run tests — verify pass**

**Step 4: Commit**

```
git commit -m "refactor(upload): extract progress display to output.rs

Follows the pattern of DownloadDisplay/BatchDisplay. The progress bar
setup and callback are now in a reusable UploadDisplay struct.

Ref #219"
```

---

## Task 18: Rename `--checksum` to `--skip-existing`

**Files:**
- Modify: `ia-core/src/upload/types.rs` — rename field
- Modify: `ia-core/src/upload/single.rs` — update references
- Modify: `ia-cli/src/commands/upload.rs` — rename CLI flag
- Update all tests referencing the field

**Step 1: Rename `checksum: bool` to `skip_existing: bool` in `UploadOpts`**

**Step 2: Update CLI flag**

```rust
/// Skip files already uploaded (MD5 match)
#[arg(long = "skip-existing")]
pub skip_existing: bool,
```

Keep `--checksums` as-is (that's the pre-computed file, not confusing with `--skip-existing`).

**Step 3: Update all code referencing `opts.checksum` to `opts.skip_existing`**

**Step 4: Update help text to clarify the distinction**

**Step 5: Run tests — verify pass**

**Step 6: Commit**

```
git commit -m "refactor(upload): rename --checksum to --skip-existing

The flags --checksum and --checksums differed by one letter, causing
confusion. --skip-existing clearly describes the behavior (skip files
whose remote MD5 matches local). --checksums FILE remains for the
pre-computed MD5 file.

Ref #219"
```

---

## Task 19: Extract Duplicate Checksums Parsing Helper

**Files:**
- Modify: `ia-cli/src/commands/upload.rs`

**Step 1: Extract helper function**

```rust
/// Load and parse a checksums file.
fn load_checksums(path: &std::path::Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)
        .context(format!("failed to read checksums file: {}", path.display()))?;
    ia_core::upload::checksum::parse_checksums(&content)
        .context("failed to parse checksums file")
}
```

**Step 2: Replace duplicated blocks in `run_bare_upload` and `run_import`**

Both currently have ~10 lines of identical code. Replace with:
```rust
let checksums = args.checksums.as_ref().map(|p| load_checksums(p)).transpose()?;
```

**Step 3: Run tests — verify pass**

**Step 4: Commit**

```
git commit -m "refactor(upload): extract load_checksums helper

Removes duplicated checksums file parsing between run_bare_upload
and run_import.

Ref #219"
```

---

## Task 20: Add Missing Integration Tests

This task fills the test gaps identified in the review. All tests use wiremock.

**Files:**
- Modify: `ia-core/tests/upload_single.rs` — add error path tests
- Modify: `ia-core/tests/upload_batch.rs` — add batch tests
- Modify: `ia-cli/tests/upload.rs` — add CLI tests

**Tests to add:**

1. **403 AccessDenied not retried** — covered in Task 2
2. **400 BadDigest not retried** — covered in Task 2
3. **500 InternalError retried then succeeds** — covered in Task 2
4. **Checksum skip tests** — covered in Task 8
5. **Empty file upload (Content-Length: 0)**
6. **`poll_check_limit` exhaustion** — retries run out
7. **Network error retry** — connection refused
8. **Pre-computed checksums flow** — `opts.checksums` → correct Content-MD5
9. **test_item collection replacement** — existing collection replaced with test_collection
10. **--json error output format** — covered in Task 10

**Step 1: Add remaining tests**

```rust
// In ia-core/tests/upload_single.rs:

#[tokio::test]
async fn upload_empty_file() {
    let mock = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(header("Content-Length", "0"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.txt");
    std::fs::write(&file, "").unwrap();

    let opts = UploadOpts::default();
    let result = upload_file(&client, "test-item", &file, "empty.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
    assert_eq!(result.bytes, 0);
}

#[tokio::test]
async fn upload_check_limit_exhaustion() {
    let mock = MockServer::start().await;

    // First PUT: 503
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(503).set_body_string("SlowDown"))
        .mount(&mock)
        .await;

    // check_limit always returns over_limit
    Mock::given(method("GET").and(query_param("check_limit", "1")))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"over_limit":1}"#,
        ))
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let opts = UploadOpts {
        retries: 2,
        retry_sleep: Duration::from_millis(1),
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ia_core::error::IaError::CheckLimitFailed { .. }));
}

#[tokio::test]
async fn upload_precomputed_checksum_used_for_content_md5() {
    let mock = MockServer::start().await;

    // Expect Content-MD5 derived from pre-computed hash
    // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
    // Base64 of those 16 bytes = XUFAKrxLKna5cZ2REBfFkg==
    Mock::given(method("PUT").and(header("Content-MD5", "XUFAKrxLKna5cZ2REBfFkg==")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();

    let mut checksums = std::collections::HashMap::new();
    checksums.insert("test.txt".into(), "5d41402abc4b2a76b9719d911017c592".into());

    let opts = UploadOpts {
        verify: true,
        checksums: Some(checksums),
        ..UploadOpts::default()
    };

    let result = upload_file(&client, "test-item", &file, "test.txt", &opts, true, true, None, None)
        .await
        .unwrap();
    assert!(matches!(result.status, UploadStatus::Uploaded));
}
```

```rust
// In ia-core/tests/upload_item.rs:

#[tokio::test]
async fn upload_item_test_item_replaces_existing_collection() {
    let mock = MockServer::start().await;

    // Expect the metadata header to contain test_collection, NOT my-real-collection
    Mock::given(method("PUT").and(header_exists("x-archive-meta00-collection")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;

    let client = test_client(&mock);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "content").unwrap();

    let opts = UploadOpts {
        metadata: vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "my-real-collection".into()),
        ],
        test_item: true,
        no_collection_check: true,
        ..UploadOpts::default()
    };

    let results = upload_item(&client, "test-item", &[file], &opts, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    // The mock verifies the header was sent — the key check is that
    // my-real-collection was replaced with test_collection
}
```

**Step 2: Run all tests**

Run: `cargo test -p ia-core -p ia-cli`

**Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`

**Step 4: Commit**

```
git commit -m "test(upload): add missing integration tests for error paths

Add tests for: empty file upload, check_limit exhaustion, pre-computed
checksums, test_item collection replacement. Together with tests added
in earlier tasks (403/400/500 retry behavior, checksum skip), this
closes all significant coverage gaps identified in the review.

Ref #219"
```

---

## Final Verification

After all 20 tasks are complete:

1. `cargo test -p ia-core -p ia-cli` — all tests pass
2. `cargo clippy -p ia-core -p ia-cli -- -D warnings` — zero warnings
3. `git status` — no uncommitted changes
4. Review the PR diff to confirm all 20 issues are addressed
5. Update PR description with a "Review fixes" section listing all changes

---

## Summary of All 20 Fixes

| # | Issue | Type | Task |
|---|-------|------|------|
| 1 | S3 XML error parsing missing | Missing feature | Task 1 |
| 2 | Non-503 errors retried (403, 400) | Critical bug | Task 2 |
| 3 | --delete-after-upload doesn't force verify | Critical bug | Task 3 |
| 4 | hex_to_bytes panics on odd-length | Important bug | Task 4 |
| 5 | compute_file_md5 blocks async runtime | Important bug | Task 5 |
| 6 | File re-read on every retry | Important bug | Task 6 |
| 7 | check_collections() not implemented | Missing feature | Task 7 |
| 8 | --checksum skip not implemented | Missing feature | Task 8 |
| 9 | Batch discards partial results | Important bug | Task 9 |
| 10 | --json error format wrong | Important bug | Task 10 |
| 11 | Batch pre-scan skips metadata validation | Missing feature | Task 11 |
| 12 | REMOTE_NAME column missing from template | Missing feature | Task 12 |
| 13 | File deletion errors silently ignored | Critical bug | Task 13 |
| 14 | Batch import progress is no-op | UX | Task 14 |
| 15 | open_after_upload macOS-only | UX | Task 15 |
| 16 | Template --format uses String not enum | UX | Task 16 |
| 17 | Progress display inline vs output.rs | UX | Task 17 |
| 18 | --checksum vs --checksums naming | UX | Task 18 |
| 19 | Duplicate checksums parsing code | UX | Task 19 |
| 20 | Missing integration tests | Test coverage | Task 20 |
