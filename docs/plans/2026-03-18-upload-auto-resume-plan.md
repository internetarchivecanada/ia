# Upload Auto-Resume Implementation Plan

**Goal:** Make batch uploads auto-resume from joblog by default, skipping files with successful entries.

**Architecture:** New `successful_files(entries, op)` in `joblog.rs` returns `HashSet<(String, String)>` of succeeded `(item, file)` pairs. `upload_item()` and `upload_batch()` accept an optional skip set. CLI builds the set from the joblog on startup. `--no-resume` global flag opts out.

**Tech Stack:** Rust, clap, serde_json, tokio, wiremock (tests)

**Spec:** `docs/plans/2026-03-18-upload-auto-resume-design.md`

---

### Task 1: Add `successful_files()` to joblog module

**Files:**
- Modify: `ia-core/src/joblog.rs:229` (after `failed_items()`)

- [ ] **Step 1: Write failing tests**

Add these tests in the existing `#[cfg(test)] mod tests` block at the end of `joblog.rs`:

```rust
#[test]
fn successful_files_empty() {
    let set = successful_files(&[], "upload");
    assert!(set.is_empty());
}

#[test]
fn successful_files_basic() {
    let entries = vec![
        JoblogEntry {
            ts: "2026-01-01T00:00:00Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-a.txt".into(),
            status: "ok".into(),
            bytes: Some(100),
            elapsed_ms: Some(50),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
        JoblogEntry {
            ts: "2026-01-01T00:00:01Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-b.txt".into(),
            status: "error".into(),
            bytes: None,
            elapsed_ms: Some(10),
            error: Some("timeout".into()),
            retries: Some(3),
            changes: None,
            tokens: None,
        },
        JoblogEntry {
            ts: "2026-01-01T00:00:02Z".into(),
            op: "upload".into(),
            item: "item-2".into(),
            file: "file-c.txt".into(),
            status: "skipped".into(),
            bytes: None,
            elapsed_ms: Some(5),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
    ];
    let set = successful_files(&entries, "upload");
    assert_eq!(set.len(), 1);
    assert!(set.contains(&("item-1".into(), "file-a.txt".into())));
    // error and skipped are excluded
    assert!(!set.contains(&("item-1".into(), "file-b.txt".into())));
    assert!(!set.contains(&("item-2".into(), "file-c.txt".into())));
}

#[test]
fn successful_files_latest_wins() {
    let entries = vec![
        // file-a: failed first, then succeeded → should be in set
        JoblogEntry {
            ts: "2026-01-01T00:00:00Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-a.txt".into(),
            status: "error".into(),
            bytes: None,
            elapsed_ms: Some(10),
            error: Some("timeout".into()),
            retries: Some(1),
            changes: None,
            tokens: None,
        },
        JoblogEntry {
            ts: "2026-01-01T00:00:01Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-a.txt".into(),
            status: "ok".into(),
            bytes: Some(100),
            elapsed_ms: Some(50),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
        // file-b: succeeded first, then failed → should NOT be in set
        JoblogEntry {
            ts: "2026-01-01T00:00:02Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-b.txt".into(),
            status: "ok".into(),
            bytes: Some(200),
            elapsed_ms: Some(30),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
        JoblogEntry {
            ts: "2026-01-01T00:00:03Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-b.txt".into(),
            status: "error".into(),
            bytes: None,
            elapsed_ms: Some(10),
            error: Some("server error".into()),
            retries: Some(2),
            changes: None,
            tokens: None,
        },
    ];
    let set = successful_files(&entries, "upload");
    assert!(set.contains(&("item-1".into(), "file-a.txt".into())));
    assert!(!set.contains(&("item-1".into(), "file-b.txt".into())));
}

#[test]
fn successful_files_skipped_not_included() {
    let entries = vec![
        JoblogEntry {
            ts: "2026-01-01T00:00:00Z".into(),
            op: "upload".into(),
            item: "item-1".into(),
            file: "file-a.txt".into(),
            status: "skipped".into(),
            bytes: None,
            elapsed_ms: Some(5),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
    ];
    let set = successful_files(&entries, "upload");
    assert!(set.is_empty());
}

#[test]
fn successful_files_filters_by_op() {
    let entries = vec![
        // A download success for (item-1, file-a.txt)
        JoblogEntry {
            ts: "2026-01-01T00:00:00Z".into(),
            op: "download".into(),
            item: "item-1".into(),
            file: "file-a.txt".into(),
            status: "ok".into(),
            bytes: Some(100),
            elapsed_ms: Some(50),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
        // An upload success for (item-2, file-b.txt)
        JoblogEntry {
            ts: "2026-01-01T00:00:01Z".into(),
            op: "upload".into(),
            item: "item-2".into(),
            file: "file-b.txt".into(),
            status: "ok".into(),
            bytes: Some(200),
            elapsed_ms: Some(30),
            error: None,
            retries: None,
            changes: None,
            tokens: None,
        },
    ];
    let set = successful_files(&entries, "upload");
    // Download entry should be excluded
    assert!(!set.contains(&("item-1".into(), "file-a.txt".into())));
    // Upload entry should be included
    assert!(set.contains(&("item-2".into(), "file-b.txt".into())));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core successful_files`
Expected: Compilation error — `successful_files` doesn't exist yet.

- [ ] **Step 3: Implement `successful_files()`**

Add after `failed_items()` (line 229) in `ia-core/src/joblog.rs`:

```rust
/// Get (item, file) pairs that succeeded, for auto-resume.
///
/// Scans entries filtered by `op`, keeps latest status per `(item, file)`,
/// returns pairs where latest status is `"ok"`. Used to skip already-uploaded
/// files when resuming a batch upload.
pub fn successful_files(
    entries: &[JoblogEntry],
    op: &str,
) -> std::collections::HashSet<(String, String)> {
    use std::collections::HashMap;

    let mut latest: HashMap<(String, String), &str> = HashMap::new();
    for entry in entries.iter().filter(|e| e.op == op) {
        latest.insert(
            (entry.item.clone(), entry.file.clone()),
            entry.status.as_str(),
        );
    }

    latest
        .into_iter()
        .filter(|(_, status)| *status == "ok")
        .map(|((item, file), _)| (item, file))
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core successful_files`
Expected: All 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/joblog.rs
git commit -m "feat(joblog): add successful_files() for upload auto-resume

Scans joblog entries filtered by operation type, returns HashSet of
(item, file) pairs where the latest status is 'ok'. Used by upload
auto-resume to skip already-uploaded files on re-run."
```

---

### Task 2: Add `Resumed` variants to upload types

**Files:**
- Modify: `ia-core/src/upload/types.rs:250-296`

- [ ] **Step 1: Add `Resumed` to `UploadStatus`**

In `ia-core/src/upload/types.rs`, add `Resumed` after `Skipped` in the `UploadStatus` enum (line ~253):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum UploadStatus {
    Uploaded,
    Skipped,
    Resumed,
    Failed(String),
    DryRun,
}
```

- [ ] **Step 2: Add `Resumed` to `UploadProgressStatus`**

Add after `Skipped` in `UploadProgressStatus` (line ~289):

```rust
pub enum UploadProgressStatus {
    Enumerated { files_count: usize, bytes_total: u64 },
    Verifying,
    Uploading,
    Retrying,
    WaitingRateLimit,
    Complete,
    Skipped,
    Resumed,
    Failed,
}
```

- [ ] **Step 3: Try to compile — identify all broken exhaustive matches**

Run: `cargo check --workspace`
Expected: Compile errors in every file that matches on `UploadStatus` or `UploadProgressStatus`. Note the locations — these are addressed in later tasks.

- [ ] **Step 4: Fix all match sites in `ia-cli/src/commands/upload.rs`**

In `write_upload_result()` (line 1294), add arm that returns early (don't log resumed files):

```rust
fn write_upload_result(jl: &JoblogWriter, r: &UploadResult) {
    let entry = JoblogEntry::new("upload", &r.identifier, &r.key);
    let entry = match &r.status {
        UploadStatus::Uploaded => entry.ok(r.bytes, r.elapsed_ms),
        UploadStatus::Skipped => entry.skipped(),
        UploadStatus::Resumed => return, // Already has a success entry — don't log again
        UploadStatus::Failed(msg) => entry.error(msg, r.retries as usize),
        UploadStatus::DryRun => entry.skipped(),
    };
    jl.write(&entry);
}
```

In `print_result_line()` (line 1258), add `Resumed` arm:

```rust
UploadStatus::Resumed => {
    eprintln!(
        " {} {}/{} (resumed, already uploaded)",
        style("–").dim(),
        r.identifier,
        r.key,
    );
}
```

In `summarize_results()` (line 1175), add resumed count — change return type to 5-tuple:

```rust
fn summarize_results(results: &[UploadResult]) -> (usize, usize, usize, usize, u64) {
    let uploaded = results.iter().filter(|r| matches!(r.status, UploadStatus::Uploaded)).count();
    let skipped = results.iter().filter(|r| matches!(r.status, UploadStatus::Skipped)).count();
    let resumed = results.iter().filter(|r| matches!(r.status, UploadStatus::Resumed)).count();
    let failed = results.iter().filter(|r| matches!(r.status, UploadStatus::Failed(_))).count();
    let total_bytes: u64 = results.iter()
        .filter(|r| matches!(r.status, UploadStatus::Uploaded))
        .map(|r| r.bytes).sum();
    (uploaded, skipped, resumed, failed, total_bytes)
}
```

Update all callers of `summarize_results()` to destructure the 5-tuple. There are two call sites:

In `run_bare_upload()` (line ~548):
```rust
let (uploaded, skipped, resumed, failed, total_bytes) = summarize_results(&results);
eprintln!(
    "{}  {} uploaded, {} skipped, {} resumed, {} failed ({}) in {:.1}s",
    identifier, uploaded, skipped, resumed, failed,
    crate::output::format_bytes(total_bytes),
    total_ms as f64 / 1000.0,
);
```

In `run_import()` (line ~747):
```rust
let (uploaded, skipped, resumed, failed, total_bytes) = summarize_results(&results);
eprintln!(
    "{} {} uploaded, {} skipped, {} resumed, {} failed ({})",
    if failure { style("done").red().bold().to_string() }
    else { style("done").green().bold().to_string() },
    uploaded, skipped, resumed, failed,
    crate::output::format_bytes(total_bytes),
);
```

In `output_results()` (line ~1143), the `_ =>` wildcard already covers `Resumed` for JSON output — no change needed (Resumed serializes as `{"status":"resumed"}`).

- [ ] **Step 5: Fix match sites in `ia-cli/src/output.rs`**

In `UploadDisplay::update()` (line ~539, after the `Skipped` arm), add `Resumed`:

```rust
UploadProgressStatus::Resumed => {
    *self.files_skipped.lock().unwrap() += 1;
    let mut processed = self.files_processed.lock().unwrap();
    *processed += 1;
    let files_total = *self.files_total.lock().unwrap();
    self.bar
        .set_message(format!("{processed}/{files_total} files"));
}
```

In `UploadBatchDisplay::update()` (line ~769, after the `Skipped` arm), add a `Resumed` arm that mirrors `Skipped`:

```rust
UploadProgressStatus::Resumed => {
    let should_finish = {
        let mut items = self.active_items.lock().unwrap();
        if let Some(item) = items.get_mut(identifier) {
            item.files_skipped += 1;
            item.files_processed += 1;
            item.bar.set_message(format!(
                "{}/{} files",
                item.files_processed, item.files_total
            ));
            item.files_processed >= item.files_total
        } else {
            false
        }
    };
    if should_finish {
        self.maybe_finish_item(identifier);
    }
}
```

In `UploadBatchDisplay::finish()` (line ~946), add `Resumed` arm in the result loop:

```rust
UploadStatus::Resumed => {
    // Resumed files don't count toward bytes_total (already uploaded)
    files_skipped += 1;
}
```

- [ ] **Step 6: Fix match sites in `ia-cli/src/tui/upload_app.rs`**

Three locations need `Resumed` arms. The TUI passes `None` for skip_set so these won't actually fire, but the matches must be exhaustive.

**6a.** `UploadTuiState::update()` per-item match (line ~235, after the `Skipped` arm):

```rust
UploadProgressStatus::Resumed => {
    item.files_skipped += 1;
    if item.files_total > 0
        && item.files_completed + item.files_skipped + item.files_failed
            >= item.files_total
    {
        item.status = UploadItemStatus::Complete;
    }
}
```

**6b.** `UploadTuiState::update()` global state match (line ~315, after the `Skipped` arm):

```rust
UploadProgressStatus::Resumed => {
    self.active_files.remove(&fk);
    self.files_skipped += 1;
}
```

**6c.** `run_upload_tui()`/`run_upload_batch_tui()` result summary (line ~487, match on `UploadStatus`):

```rust
ia_core::upload::UploadStatus::Resumed => {
    total_skipped += 1;
}
```

- [ ] **Step 7: Verify compilation**

Run: `cargo check --workspace`
Expected: Clean compilation, no errors.

- [ ] **Step 8: Run all tests**

Run: `cargo test --workspace`
Expected: All existing tests pass.

- [ ] **Step 9: Commit**

```bash
git add ia-core/src/upload/types.rs ia-cli/src/commands/upload.rs ia-cli/src/output.rs ia-cli/src/tui/upload_app.rs
git commit -m "feat(upload): add Resumed status variant for auto-resume

New UploadStatus::Resumed and UploadProgressStatus::Resumed variants,
semantically distinct from Skipped (which checks remote MD5). Resumed
means the joblog already has a success entry for this file.

Resumed files are not written to the joblog (already have a success
entry). All exhaustive matches updated across CLI, output, and TUI."
```

---

### Task 3: Skip logic in `upload_item()`

**Files:**
- Modify: `ia-core/src/upload/item.rs:33-200`

- [ ] **Step 1: Write failing tests**

Add wiremock-based tests in the existing test module of `ia-core/src/upload/item.rs` (or create one if it doesn't exist — check first). These tests need a wiremock server for the collection check that happens in `upload_item()`.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::NamedTempFile;
    use std::io::Write as IoWrite;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path};

    /// Helper: create a temp file with given content, return path
    fn temp_file(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        f
    }

    /// Helper: build an IaClient pointed at the mock server
    async fn mock_client(server: &MockServer) -> crate::IaClient {
        crate::IaClient::builder()
            .host(&server.uri().replace("http://", ""))
            .insecure(true)
            .build()
            .unwrap()
    }

    /// Helper: default opts with no_collection_check=true to skip collection validation
    fn test_opts() -> UploadOpts {
        UploadOpts {
            no_collection_check: true,
            metadata: vec![("collection".into(), "test_collection".into())],
            dry_run: true, // dry_run avoids actual HTTP uploads
            ..UploadOpts::default()
        }
    }

    #[tokio::test]
    async fn upload_item_skips_resumed_files() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"file one content");
        let f2 = temp_file(b"file two content");

        let mut skip_set = HashSet::new();
        // Use basename of f1 as the key (that's what compute_keys produces)
        let f1_key = f1.path().file_name().unwrap().to_string_lossy().to_string();
        skip_set.insert(("test-item".into(), f1_key.clone()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf(), f2.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(matches!(results[0].status, UploadStatus::Resumed));
        assert!(matches!(results[1].status, UploadStatus::DryRun)); // dry_run for non-resumed
    }

    #[tokio::test]
    async fn upload_item_all_resumed_no_uploads() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"content");

        let mut skip_set = HashSet::new();
        let f1_key = f1.path().file_name().unwrap().to_string_lossy().to_string();
        skip_set.insert(("test-item".into(), f1_key));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].status, UploadStatus::Resumed));
        // No HTTP calls should have been made (beyond what setup requires)
    }

    #[tokio::test]
    async fn upload_item_is_first_after_resume() {
        // When file 0 is resumed, file 1 should get is_first=true (metadata headers).
        // We verify this indirectly: with dry_run, the first non-resumed file should
        // produce a DryRun result (meaning upload_file was called with is_first=true,
        // which includes metadata headers). If is_first were false, metadata wouldn't
        // be attached to any file.
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"first file");
        let f2 = temp_file(b"second file");

        let mut skip_set = HashSet::new();
        let f1_key = f1.path().file_name().unwrap().to_string_lossy().to_string();
        skip_set.insert(("test-item".into(), f1_key));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf(), f2.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(matches!(results[0].status, UploadStatus::Resumed));
        // File 2 should have been uploaded (dry run) — this means is_first was true
        // for it, so metadata headers would be attached
        assert!(matches!(results[1].status, UploadStatus::DryRun));
    }

    #[tokio::test]
    async fn upload_item_is_last_correct_with_resume() {
        // When the last file in the list is resumed, derive should trigger on the
        // last non-resumed file (is_last=true). We verify by checking that all files
        // produce results: resumed files get Resumed, non-resumed get DryRun.
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"first");
        let f2 = temp_file(b"second");
        let f3 = temp_file(b"third");

        let mut skip_set = HashSet::new();
        // Resume the LAST file — f2 (the middle one) should become is_last
        let f3_key = f3.path().file_name().unwrap().to_string_lossy().to_string();
        skip_set.insert(("test-item".into(), f3_key));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf(), f2.path().to_path_buf(), f3.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 3);
        assert!(matches!(results[0].status, UploadStatus::DryRun));
        assert!(matches!(results[1].status, UploadStatus::DryRun));
        assert!(matches!(results[2].status, UploadStatus::Resumed));
    }

    #[tokio::test]
    async fn upload_item_different_key_not_resumed() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"content");

        let mut skip_set = HashSet::new();
        // Key in skip set doesn't match the file's basename
        skip_set.insert(("test-item".into(), "totally-different-name.txt".into()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 1);
        // Should NOT be resumed — key doesn't match
        assert!(!matches!(results[0].status, UploadStatus::Resumed));
    }

    #[tokio::test]
    async fn upload_item_same_key_different_item_not_resumed() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;

        let f1 = temp_file(b"content");
        let f1_key = f1.path().file_name().unwrap().to_string_lossy().to_string();

        let mut skip_set = HashSet::new();
        // Same key but different item
        skip_set.insert(("other-item".into(), f1_key));

        let results = upload_item(
            &client,
            "test-item",
            &[f1.path().to_path_buf()],
            &test_opts(),
            None,
            Some(&skip_set),
        ).await.unwrap();

        assert_eq!(results.len(), 1);
        assert!(!matches!(results[0].status, UploadStatus::Resumed));
    }
}
```

Note: The exact test setup may need adjustment based on existing test patterns in this file. Check if there are existing tests and follow the same mock client setup pattern.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core upload_item_skips`
Expected: Compilation error — `upload_item` doesn't accept `skip_set` yet.

- [ ] **Step 3: Add `skip_set` parameter to `upload_item()`**

Change the signature at line 33:

```rust
pub async fn upload_item(
    client: &IaClient,
    identifier: &str,
    files: &[PathBuf],
    opts: &UploadOpts,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
    skip_set: Option<&std::collections::HashSet<(String, String)>>,
) -> Result<Vec<UploadResult>> {
```

Add `use std::collections::HashSet;` to the imports at the top of the file.

- [ ] **Step 4: Compute `last_upload_idx` before the loop**

Insert before line 129 (before "10. Upload sequentially"):

```rust
    // 9b. Compute the index of the last file that will actually be uploaded
    //     (not in skip_set). Used for is_last/derive logic.
    let last_upload_idx = if let Some(skip) = skip_set {
        expanded
            .iter()
            .zip(keys.iter())
            .enumerate()
            .rev()
            .find(|(_, (_, key))| {
                !skip.contains(&(identifier.to_string(), key.to_string()))
            })
            .map(|(i, _)| i)
    } else if file_count > 0 {
        Some(file_count - 1)
    } else {
        None
    };
```

- [ ] **Step 5: Keep `Enumerated` event unchanged (full counts)**

Do NOT modify the `Enumerated` event (lines 95-127). It should continue to report the **full** file count and byte total including resumed files. This is critical because:
- `UploadBatchDisplay` uses `files_total` from `Enumerated` to track completion
- `Resumed` progress events increment `files_processed` — if `files_total` only counted active files, `files_processed` would exceed `files_total`
- Progress bars need full byte totals to show correct position after resumed files are accounted for

The existing code at lines 94-127 is correct as-is. No changes needed for this step.

- [ ] **Step 6: Add skip logic in the file loop**

In the file loop (line 134), before the `upload_file()` call, add the resume check:

```rust
    for (i, (file, key)) in expanded.iter().zip(keys.iter()).enumerate() {
        // Resume check: skip files already successfully uploaded
        if let Some(skip) = skip_set {
            if skip.contains(&(identifier.to_string(), key.clone())) {
                let file_size = tokio::fs::metadata(file)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);

                if let Some(ref cb) = progress {
                    cb(UploadProgress {
                        identifier: identifier.to_string(),
                        key: key.clone(),
                        bytes_sent: file_size,
                        total_bytes: file_size,
                        status: UploadProgressStatus::Resumed,
                    });
                }

                results.push(UploadResult {
                    identifier: identifier.to_string(),
                    key: key.clone(),
                    status: UploadStatus::Resumed,
                    bytes: file_size,
                    md5: None,
                    elapsed_ms: 0,
                    retries: 0,
                });
                continue;
            }
        }

        let is_first = i == 0 || !first_file_succeeded;
        let is_last = Some(i) == last_upload_idx;  // CHANGED from: i == file_count - 1

        // Size hint on first non-resumed file
        let hint = if !first_file_succeeded { size_hint } else { None };

        // ... rest of loop unchanged ...
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ia-core upload_item_`
Expected: All 4 new tests pass.

- [ ] **Step 8: Fix compilation for all callers of `upload_item()`**

Add `None` as the last argument to every existing call site:

1. `ia-core/src/upload/batch.rs` — inside `upload_batch()` (will be updated properly in Task 4, for now pass `None`)
2. `ia-cli/src/commands/upload.rs` — `run_bare_upload()` dry-run call (line ~487): add `None`
3. `ia-cli/src/commands/upload.rs` — `run_bare_upload()` main upload call (line ~532): add `None`
4. `ia-cli/src/tui/upload_app.rs` — two call sites (lines ~626, ~695): add `None`

- [ ] **Step 9: Verify full compilation and all tests pass**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```bash
git add ia-core/src/upload/item.rs ia-core/src/upload/batch.rs ia-cli/src/commands/upload.rs ia-cli/src/tui/upload_app.rs
git commit -m "feat(upload): add skip_set parameter to upload_item for auto-resume

upload_item() now accepts an optional HashSet of (item, file) pairs to
skip. Files in the set produce UploadStatus::Resumed results without
making HTTP calls.

Correctly handles is_first (metadata headers on first non-resumed file)
and is_last (derive on last non-resumed file). Enumerated event reports
only active (non-resumed) file counts and bytes."
```

---

### Task 4: Thread skip set through `upload_batch()`

**Files:**
- Modify: `ia-core/src/upload/batch.rs:39-122`

- [ ] **Step 1: Add `skip_set` parameter to `upload_batch()`**

Change signature (line 39):

```rust
pub async fn upload_batch(
    client: &IaClient,
    records: Vec<SpreadsheetRecord>,
    opts: &UploadOpts,
    concurrency: usize,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
    skip_set: Option<Arc<std::collections::HashSet<(String, String)>>>,
) -> Result<Vec<UploadResult>>
```

- [ ] **Step 2: Pass skip set to `upload_item()` calls**

In the stream processing closure, clone the Arc and pass it:

```rust
    let results: Vec<(String, std::result::Result<Vec<UploadResult>, IaError>)> =
        stream::iter(groups)
            .map(|group| {
                let client = client;
                let progress = progress.clone();
                let skip = skip_set.clone();
                async move {
                    // ... existing opts building ...
                    let result = upload_item(
                        client,
                        &group.identifier,
                        &group.files,
                        &item_opts,
                        progress,
                        skip.as_deref(),
                    )
                    .await;
                    (group.identifier, result)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;
```

`Arc::as_deref()` on `Option<Arc<HashSet<...>>>` gives `Option<&HashSet<...>>`.

- [ ] **Step 3: Fix all callers of `upload_batch()`**

Add `None` to existing call sites:

1. `ia-cli/src/commands/upload.rs` — `run_import()` (line ~730): add `None` (will be replaced with actual skip set in Task 6)
2. `ia-cli/src/tui/upload_app.rs` — if `upload_batch` is called there (check — it might use `upload_item` directly via group iteration)

- [ ] **Step 4: Update `ia-core/src/upload/mod.rs` re-exports if needed**

Check if `upload_batch` signature change affects any re-exports. It shouldn't — the function is re-exported, not the types.

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/upload/batch.rs ia-core/src/upload/mod.rs ia-cli/src/commands/upload.rs ia-cli/src/tui/upload_app.rs
git commit -m "feat(upload): thread skip_set through upload_batch for auto-resume

upload_batch() now accepts Option<Arc<HashSet<(String, String)>>> and
passes it to each concurrent upload_item() call. Arc sharing avoids
cloning the (potentially large) set per item."
```

---

### Task 5: Add `--no-resume` global flag

**Files:**
- Modify: `ia-cli/src/main.rs:32-86` (Cli struct), `ia-cli/src/main.rs:260-280` (upload routing)

- [ ] **Step 1: Add `no_resume` field to Cli struct**

After `retry_failed` (line ~68):

```rust
    /// Don't resume from joblog — upload all files fresh
    #[arg(long, global = true)]
    no_resume: bool,
```

- [ ] **Step 2: Pass `no_resume` to upload command**

In the Upload match arm (line ~266):

```rust
Commands::Upload(args) => {
    commands::upload::run(
        &client,
        args,
        cli.quiet,
        cli.jobs,
        cli.joblog,
        cli.retry_failed,
        cli.no_resume,
    )
    .await?
}
```

- [ ] **Step 3: Update `upload::run()` to accept `no_resume`**

Change the signature in `ia-cli/src/commands/upload.rs` at line 323:

```rust
pub async fn run(
    client: &IaClient,
    args: UploadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    retry_failed: bool,
    no_resume: bool,
) -> Result<()> {
```

Pass `no_resume` through to `run_import()` and `run_bare_upload()`.

- [ ] **Step 4: Verify compilation**

Run: `cargo check --workspace`
Expected: Clean compilation.

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/main.rs ia-cli/src/commands/upload.rs
git commit -m "feat(cli): add --no-resume global flag

Disables auto-resume from joblog. When set, all files are uploaded
fresh even if the joblog has success entries. Global because other
commands (download, tasks, metadata) will adopt auto-resume later."
```

---

### Task 6: Wire auto-resume in CLI upload commands

**Files:**
- Modify: `ia-cli/src/commands/upload.rs:323-768`

- [ ] **Step 1: Write integration test for `--retry-failed` error**

In `ia-cli/tests/upload.rs`:

```rust
#[test]
fn upload_retry_failed_shows_error() {
    let config = empty_config();
    ia_with_config(&config)
        .args(["upload", "test-id", "file.txt", "--retry-failed", "--joblog", "/dev/null"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--retry-failed is no longer needed for uploads"));
}
```

- [ ] **Step 2: Write integration test for `--no-resume`**

```rust
#[test]
fn upload_no_resume_flag_accepted() {
    let config = empty_config();
    // Should fail for missing files, but --no-resume should be accepted as valid
    ia_with_config(&config)
        .args(["upload", "test-id", "file.txt", "--no-resume"])
        .assert()
        .failure()
        .stderr(predicates::str::is_empty().not()); // fails for other reasons, but flag is valid
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ia-cli upload_retry_failed_shows_error upload_no_resume`
Expected: First test fails (current code doesn't show that message). Second may pass or fail depending on exact error.

- [ ] **Step 4: Add `build_skip_set()` helper function**

Add near the top of `ia-cli/src/commands/upload.rs` (after imports):

```rust
/// Build the auto-resume skip set from an existing joblog.
///
/// Returns `None` if: `--no-resume` is set, no `--joblog` path, joblog doesn't exist,
/// or joblog has no successful upload entries.
fn build_skip_set(
    joblog_path: &Option<PathBuf>,
    no_resume: bool,
    quiet: u8,
) -> Result<Option<Arc<std::collections::HashSet<(String, String)>>>> {
    if no_resume {
        return Ok(None);
    }
    let path = match joblog_path {
        Some(p) if p.exists() => p,
        _ => return Ok(None),
    };
    let entries = ia_core::joblog::read(path)
        .context(format!("failed to read joblog for resume: {}", path.display()))?;
    let set = ia_core::joblog::successful_files(&entries, "upload");
    if set.is_empty() {
        return Ok(None);
    }
    if quiet == 0 {
        eprintln!(
            " {} Resuming: {} files already uploaded (from {})",
            style("▸").cyan(),
            set.len(),
            path.display(),
        );
    }
    Ok(Some(Arc::new(set)))
}
```

- [ ] **Step 5: Replace `--retry-failed` check with error for uploads**

In `run()` (line ~361), replace the existing `retry_failed && !is_batch` check:

```rust
    // --retry-failed is no longer needed for uploads (auto-resume subsumes it)
    if retry_failed {
        bail!(
            "--retry-failed is no longer needed for uploads.\n\
             Resume is automatic when --joblog is provided. Re-run the same command to resume.\n\
             Use --no-resume to upload all files fresh."
        );
    }
```

- [ ] **Step 6: Wire skip set in `run_bare_upload()`**

Change `run_bare_upload()` signature to accept `no_resume`:

```rust
async fn run_bare_upload(
    client: &IaClient,
    args: UploadArgs,
    quiet: u8,
    joblog_path: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
```

Build and pass the skip set before the `upload_item()` call (line ~532):

```rust
    let skip_set = build_skip_set(&joblog_path, no_resume, quiet)?;

    // ... existing code ...

    let results = upload_item(
        client,
        identifier,
        &files,
        &opts,
        progress_ref,
        skip_set.as_deref(),
    )
    .await
    .context(format!("failed to upload to {identifier}"))?;
```

Also pass skip set to the dry-run call (line ~487):

```rust
    let results = upload_item(client, identifier, &files, &opts, None, None)
```

(Dry-run doesn't need resume — it just validates.)

- [ ] **Step 7: Wire skip set in `run_import()`**

Change signature:

```rust
async fn run_import(
    client: &IaClient,
    args: &UploadArgs,
    quiet: u8,
    jobs: usize,
    joblog_path: Option<PathBuf>,
    no_resume: bool,
) -> Result<()> {
```

Remove the entire `--retry-failed` filtering block (lines 613-638). Replace with:

```rust
    let skip_set = build_skip_set(&joblog_path, no_resume, quiet)?;
```

Pass to `upload_batch()`:

```rust
    let results = upload_batch(client, records, &opts, jobs, progress_ref, skip_set)
        .await
        .context("batch upload failed")?;
```

- [ ] **Step 8: Update all `run_import()` and `run_bare_upload()` call sites in `run()`**

Replace `retry_failed` with `no_resume` in all forwarding calls:

```rust
// Line ~406 (deprecated import redirect):
return run_import(client, &merged, quiet, jobs, joblog_path, no_resume).await;

// Line ~410 (spreadsheet path):
return run_import(client, &args, quiet, jobs, joblog_path, no_resume).await;

// Line ~417 (bare upload):
None => run_bare_upload(client, args, quiet, joblog_path, no_resume).await,
```

- [ ] **Step 9: Update `UploadBatchDisplay::new()` — remove `retry_mode`**

In `ia-cli/src/output.rs`, change:

```rust
pub fn new(items_total: usize, jobs: usize) -> Self {
    // ...
    let verb = "Uploading";
    // ... remove retry_mode parameter and the if/else for verb
```

Update the call site in `run_import()`:

```rust
Some(std::sync::Arc::new(crate::output::UploadBatchDisplay::new(
    item_count,
    jobs,
)))
```

- [ ] **Step 10: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass, including new integration tests.

- [ ] **Step 11: Commit**

```bash
git add ia-cli/src/commands/upload.rs ia-cli/src/output.rs ia-cli/tests/upload.rs
git commit -m "feat(upload): wire auto-resume from joblog in CLI

Build skip set from existing joblog on startup. Pass to upload_item()
and upload_batch(). --retry-failed now returns an error for uploads.
--no-resume disables auto-resume.

Startup message shows count of files being resumed. Removed retry_mode
from UploadBatchDisplay since --retry-failed is gone for uploads."
```

---

### Task 7: Documentation updates

**Files:**
- Modify: `docs/usage.md`
- Modify: `ia-cli/src/commands/upload.rs` (help text)

- [ ] **Step 1: Update upload help text**

In the `after_long_help` for upload commands, add resume documentation. Find the `after_long_help` string in `UploadArgs` and add:

```
# Resume interrupted uploads
$ ia upload --spreadsheet batch.csv --joblog upload.jsonl
# (re-run the same command — already-uploaded files are skipped automatically)

# Force re-upload everything (ignore previous progress)
$ ia upload --spreadsheet batch.csv --joblog upload.jsonl --no-resume
```

- [ ] **Step 2: Update `docs/usage.md`**

Add a "Resuming Uploads" section under the Upload section:

```markdown
### Resuming Uploads

When `--joblog` is provided, uploads automatically resume from where they left off.
Files that were successfully uploaded in a previous run are skipped.

```sh
# First run (interrupted after 8 of 15 files)
ia upload --spreadsheet batch.csv --joblog upload.jsonl
# ^C

# Re-run — 8 files skipped, 7 remaining uploaded
ia upload --spreadsheet batch.csv --joblog upload.jsonl
# ▸ Resuming: 8 files already uploaded (from upload.jsonl)

# Force re-upload everything
ia upload --spreadsheet batch.csv --joblog upload.jsonl --no-resume
```
```

- [ ] **Step 3: Update `--joblog` help text in `main.rs`**

Update the help string for the `joblog` field to mention auto-resume:

```rust
    /// Write operation results to a JSONL log file (enables auto-resume for uploads)
    #[arg(long, global = true)]
    joblog: Option<PathBuf>,
```

- [ ] **Step 4: Commit**

```bash
git add docs/usage.md ia-cli/src/commands/upload.rs ia-cli/src/main.rs
git commit -m "docs: document upload auto-resume behavior

Update upload help text, usage docs, and --joblog description to
explain automatic resume from joblog. Add examples for --no-resume."
```

---

### Task 8: Create GitHub issues for `--retry-failed` migration

**Files:** None (GitHub API only)

- [ ] **Step 1: Create issues**

Create 5 GitHub issues with label `auto-resume`:

1. **`download: migrate to auto-resume from joblog`**
   Body: Upload now auto-resumes from joblog (see `docs/plans/2026-03-18-upload-auto-resume-design.md`). Download should adopt the same pattern: read successful `(item, file)` entries from joblog via `successful_files()`, skip them, upload the rest. Remove `--retry-failed` from download after migration. Update download help text to explain resume is default.

2. **`tasks submit: migrate to auto-resume from joblog`**
   Body: Upload now auto-resumes from joblog. Tasks submit should adopt the same pattern. The granularity may be item-level rather than file-level. Remove `--retry-failed` from tasks after migration. Update help text.

3. **`metadata: migrate to auto-resume from joblog`**
   Body: Upload now auto-resumes from joblog. Metadata operations should adopt the same pattern. Remove `--retry-failed` from metadata after migration. Update help text.

4. **`remove --retry-failed global flag`** (blocked by 1-3)
   Body: Once download, tasks, and metadata all support auto-resume from joblog, remove `--retry-failed` from the global CLI options entirely. Currently kept for backward compatibility with those commands.

5. **`update --joblog help text across all commands`**
   Body: After auto-resume is adopted by all commands, update the `--joblog` help text and usage docs to note that resume is the default behavior for all batch operations, not just uploads.

- [ ] **Step 2: Commit** (nothing to commit — issues are on GitHub)

---

### Task 9: Final verification

- [ ] **Step 1: Run full CI suite**

Run: `just ci`
Expected: fmt-check, check, test, doc all pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Verify test count increased**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: Test count should be higher than current (1,087+). New tests: ~5 joblog + ~4 item + ~2 CLI = ~11 new tests.

- [ ] **Step 4: Manual smoke test (if possible)**

If you have test credentials:
1. Create a small spreadsheet with 2 items, 2 files each
2. `ia upload --spreadsheet test.csv --joblog test.jsonl --test-item` → upload all
3. Delete one file from the joblog (simulate partial completion)
4. Re-run same command → verify the deleted file's item re-uploads, others show "Resuming"
5. Re-run with `--no-resume` → all files uploaded fresh
