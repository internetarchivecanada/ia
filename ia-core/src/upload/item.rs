use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};

use crate::error::{IaError, Result};
use crate::fs_util::expand_files;
use crate::identifier::validate_identifier;
use crate::upload::single::upload_file;
use crate::upload::types::{
    UploadOpts, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};
use crate::upload::validate::{validate_file, validate_required_metadata};
use crate::IaClient;

/// Callback invoked after each file result is produced (for streaming joblog writes).
type OnResultCallback = Arc<dyn Fn(&UploadResult) + Send + Sync>;

/// Upload multiple files to a single IA item.
///
/// Handles:
/// - Identifier and metadata validation
/// - Directory expansion (recursively walk directories)
/// - File-to-key mapping (basename, keep-directories, remote-dir)
/// - Size hint computation
/// - test_item: inject collection:test_collection
/// - First file gets metadata headers + auto-make-bucket
/// - Last file gets queue-derive
/// - Sequential upload by default; concurrent middle files when `file_concurrency > 1`
///
/// When `file_concurrency > 1` and there are more than 2 files, the first and last
/// files upload sequentially (for metadata/derive headers) while middle files upload
/// concurrently via `buffer_unordered`. In this mode, results for middle files may
/// be returned in completion order rather than file order.
///
/// # Errors
///
/// Returns `IaError::InvalidIdentifier` if the identifier is malformed.
/// Returns `IaError::MissingRequiredMetadata` if required metadata fields are absent.
/// Returns `IaError::EmptyUpload` if no files remain after expansion and filtering.
/// Returns `IaError::SymlinkSkipped` if a file is a symlink (during validation).
/// Propagates any errors from individual `upload_file()` calls.
#[allow(clippy::too_many_arguments)]
pub async fn upload_item(
    client: &IaClient,
    identifier: &str,
    files: &[PathBuf],
    opts: &UploadOpts,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
    skip_set: Option<&HashSet<(String, String)>>,
    on_result: Option<OnResultCallback>,
    file_concurrency: usize,
    pause_flag: Option<Arc<AtomicBool>>,
) -> Result<Vec<UploadResult>> {
    // 1. Validate identifier
    validate_identifier(identifier)?;

    // 2. Handle test_item: clone opts and inject collection:test_collection
    let opts = if opts.test_item {
        let mut opts = opts.clone();
        // Remove any existing collection entry, then add test_collection
        opts.metadata.retain(|(k, _)| k != "collection");
        opts.metadata
            .push(("collection".into(), "test_collection".into()));
        opts
    } else {
        opts.clone()
    };

    // 3. Validate required metadata (if metadata is provided)
    if !opts.metadata.is_empty() && !opts.no_collection_check {
        validate_required_metadata(&opts.metadata)?;
    }

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

    // 4. Expand directories and collect files
    let files_owned = files.to_vec();
    let expanded = tokio::task::spawn_blocking(move || expand_files(&files_owned))
        .await
        .map_err(|e| IaError::Io(std::io::Error::other(format!("spawn_blocking: {e}"))))??;

    // 5. Check for empty
    if expanded.is_empty() {
        return Err(IaError::EmptyUpload);
    }

    // 6. Validate each file (exists, not symlink)
    for file in &expanded {
        validate_file(file)?;
    }

    // 7. Compute remote keys
    let keys = compute_keys(&expanded, &opts)?;

    // 8. Compute total bytes (always needed for progress display).
    //    Do a single stat pass here; conditionally set size_hint based on no_size_hint.
    let file_count = expanded.len();
    let total_bytes: u64 = {
        let paths = expanded.clone();
        tokio::task::spawn_blocking(move || -> u64 {
            paths
                .iter()
                .filter_map(|f| std::fs::metadata(f).ok())
                .map(|m| m.len())
                .sum()
        })
        .await
        // JoinError only fires on panic or runtime shutdown — propagate rather
        // than silently defaulting to 0 so callers notice catastrophic failures.
        .map_err(|e| IaError::Io(std::io::Error::other(format!("spawn_blocking: {e}"))))?
    };
    let size_hint = if opts.no_size_hint {
        None
    } else {
        Some(total_bytes)
    };

    // 9. Emit Enumerated event so consumers know the file list and total size.
    if let Some(ref cb) = progress {
        cb(UploadProgress {
            identifier: identifier.to_string(),
            key: String::new(),
            bytes_sent: 0,
            total_bytes: 0,
            status: UploadProgressStatus::Enumerated {
                files_count: file_count,
                bytes_total: total_bytes,
            },
        });
    }

    // 10. Pre-compute owned identifier for skip-set lookups (avoids repeated
    //     allocations inside the file loop).
    let id_owned = identifier.to_string();

    // 11. Compute the index of the last file that will actually be uploaded
    //     (i.e. not in the skip set). This determines which file triggers derive.
    let last_upload_idx = if let Some(skip) = skip_set {
        expanded
            .iter()
            .zip(keys.iter())
            .enumerate()
            .rev()
            .find(|(_, (_, key))| !skip.contains(&(id_owned.clone(), key.to_string())))
            .map(|(i, _)| i)
    } else if file_count > 0 {
        Some(file_count - 1)
    } else {
        None
    };

    // 12. Upload files
    let start = Instant::now();

    // Concurrent path: when file_concurrency > 1 and more than 2 files,
    // upload the first file sequentially (metadata + bucket creation),
    // middle files concurrently, and last file sequentially (queue-derive).
    if file_concurrency > 1 && file_count > 2 {
        let mut results = Vec::with_capacity(file_count);

        // Phase 1: Upload first file sequentially
        let (first_result, first_uploaded) = upload_one_file(
            client,
            identifier,
            &expanded[0],
            &keys[0],
            &opts,
            &id_owned,
            skip_set,
            0,
            last_upload_idx,
            false, // no previous file succeeded
            size_hint,
            progress.clone(),
            on_result.clone(),
            start,
        )
        .await?;
        let first_uploaded =
            first_uploaded || matches!(first_result.status, UploadStatus::Uploaded);
        results.push(first_result);

        // Phase 2: Upload middle files concurrently
        let middle_range = 1..(file_count - 1);
        let owned_skip = skip_set.cloned();

        let middle_results: Vec<Result<(UploadResult, bool)>> = stream::iter(middle_range)
            .map(|i| {
                let client = client.clone();
                let identifier = identifier.to_string();
                let file = expanded[i].clone();
                let key = keys[i].clone();
                let opts = opts.clone();
                let id_owned = id_owned.clone();
                let skip_ref = owned_skip.clone();
                let progress = progress.clone();
                let on_result = on_result.clone();
                let pause = pause_flag.clone();

                async move {
                    // Wait while paused before starting this file
                    if let Some(ref flag) = pause {
                        while flag.load(Ordering::Relaxed) {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                    upload_one_file(
                        &client,
                        &identifier,
                        &file,
                        &key,
                        &opts,
                        &id_owned,
                        skip_ref.as_ref(),
                        i,
                        last_upload_idx,
                        true, // first file already handled
                        None, // size_hint only on first file
                        progress,
                        on_result,
                        start,
                    )
                    .await
                }
            })
            .buffer_unordered(file_concurrency)
            .collect()
            .await;

        // Check for fatal errors and collect middle results
        for r in middle_results {
            let (result, _) = r?;
            results.push(result);
        }

        // Wait while paused before starting last file
        if let Some(ref flag) = pause_flag {
            while flag.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        // Phase 3: Upload last file sequentially
        let last_idx = file_count - 1;
        let (last_result, _) = upload_one_file(
            client,
            identifier,
            &expanded[last_idx],
            &keys[last_idx],
            &opts,
            &id_owned,
            skip_set,
            last_idx,
            last_upload_idx,
            first_uploaded,
            None, // size_hint only on first file
            progress.clone(),
            on_result.clone(),
            start,
        )
        .await?;
        results.push(last_result);

        return Ok(results);
    }

    // Sequential path (file_concurrency <= 1, or <= 2 files)
    let mut results = Vec::with_capacity(file_count);
    let mut first_file_succeeded = false;

    for (i, (file, key)) in expanded.iter().zip(keys.iter()).enumerate() {
        // Wait while paused before starting next file
        if let Some(ref flag) = pause_flag {
            while flag.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        // Resume: skip files already successfully uploaded in a previous run
        if let Some(skip) = skip_set {
            if skip.contains(&(id_owned.clone(), key.clone())) {
                let file_size = tokio::fs::metadata(file).await.map_err(|e| {
                    IaError::Io(std::io::Error::other(format!(
                        "failed to stat resumed file {}: {e}",
                        file.display()
                    )))
                })?;
                let file_size = file_size.len();
                if let Some(ref cb) = progress {
                    cb(UploadProgress {
                        identifier: id_owned.clone(),
                        key: key.clone(),
                        bytes_sent: file_size,
                        total_bytes: file_size,
                        status: UploadProgressStatus::Resumed,
                    });
                }
                let result = UploadResult {
                    identifier: id_owned.clone(),
                    key: key.clone(),
                    status: UploadStatus::Resumed,
                    bytes: file_size,
                    md5: None,
                    elapsed_ms: 0,
                    retries: 0,
                };
                if let Some(ref cb) = on_result {
                    cb(&result);
                }
                results.push(result);
                continue;
            }
        }

        let is_first = i == 0 || !first_file_succeeded;
        let is_last = Some(i) == last_upload_idx;

        // Size hint only on the first file that actually uploads
        let hint = if !first_file_succeeded {
            size_hint
        } else {
            None
        };

        match upload_file(
            client,
            identifier,
            file,
            key,
            &opts,
            is_first,
            is_last,
            hint,
            progress.clone(),
        )
        .await
        {
            Ok(result) => {
                if matches!(result.status, UploadStatus::Uploaded) {
                    first_file_succeeded = true;
                }
                if let Some(ref cb) = on_result {
                    cb(&result);
                }
                results.push(result);
            }
            Err(
                e @ (IaError::Auth(_)
                | IaError::Config(_)
                | IaError::SpamDetected { .. }
                | IaError::CheckLimitFailed { .. }),
            ) => {
                // Fatal errors — bail immediately, no point continuing
                return Err(e);
            }
            Err(e) => {
                // Non-fatal: record failure and continue with remaining files
                let err_msg = e.to_string();
                tracing::warn!(identifier, key, "file upload failed, continuing: {err_msg}");

                if let Some(ref cb) = progress {
                    cb(UploadProgress {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        bytes_sent: 0,
                        total_bytes: 0,
                        status: UploadProgressStatus::Failed(err_msg.clone()),
                    });
                }

                let file_size = tokio::fs::metadata(file)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                let result = UploadResult {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    status: UploadStatus::Failed(err_msg),
                    bytes: file_size,
                    md5: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    retries: 0,
                };
                if let Some(ref cb) = on_result {
                    cb(&result);
                }
                results.push(result);
            }
        }
    }

    Ok(results)
}

/// Upload a single file within an item, handling skip-set checks, error
/// classification, and progress/result callbacks.
///
/// Returns `Ok((result, first_file_succeeded))` for uploaded/resumed/skipped/failed
/// files, and `Err` only for fatal errors (Auth, Config, SpamDetected, CheckLimitFailed).
#[allow(clippy::too_many_arguments)]
async fn upload_one_file(
    client: &IaClient,
    identifier: &str,
    file: &Path,
    key: &str,
    opts: &UploadOpts,
    id_owned: &str,
    skip_set: Option<&HashSet<(String, String)>>,
    index: usize,
    last_upload_idx: Option<usize>,
    first_file_succeeded: bool,
    size_hint: Option<u64>,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
    on_result: Option<OnResultCallback>,
    start: Instant,
) -> Result<(UploadResult, bool)> {
    // Resume: skip files already successfully uploaded in a previous run
    if let Some(skip) = skip_set {
        if skip.contains(&(id_owned.to_string(), key.to_string())) {
            let file_size = tokio::fs::metadata(file).await.map_err(|e| {
                IaError::Io(std::io::Error::other(format!(
                    "failed to stat resumed file {}: {e}",
                    file.display()
                )))
            })?;
            let file_size = file_size.len();
            if let Some(ref cb) = progress {
                cb(UploadProgress {
                    identifier: id_owned.to_string(),
                    key: key.to_string(),
                    bytes_sent: file_size,
                    total_bytes: file_size,
                    status: UploadProgressStatus::Resumed,
                });
            }
            let result = UploadResult {
                identifier: id_owned.to_string(),
                key: key.to_string(),
                status: UploadStatus::Resumed,
                bytes: file_size,
                md5: None,
                elapsed_ms: 0,
                retries: 0,
            };
            if let Some(ref cb) = on_result {
                cb(&result);
            }
            return Ok((result, first_file_succeeded));
        }
    }

    let is_first = index == 0 || !first_file_succeeded;
    let is_last = Some(index) == last_upload_idx;

    // Size hint only on the first file that actually uploads
    let hint = if !first_file_succeeded {
        size_hint
    } else {
        None
    };

    match upload_file(
        client,
        identifier,
        file,
        key,
        opts,
        is_first,
        is_last,
        hint,
        progress.clone(),
    )
    .await
    {
        Ok(result) => {
            let uploaded = matches!(result.status, UploadStatus::Uploaded);
            if let Some(ref cb) = on_result {
                cb(&result);
            }
            Ok((result, first_file_succeeded || uploaded))
        }
        Err(
            e @ (IaError::Auth(_)
            | IaError::Config(_)
            | IaError::SpamDetected { .. }
            | IaError::CheckLimitFailed { .. }),
        ) => {
            // Fatal errors — bail immediately
            Err(e)
        }
        Err(e) => {
            // Non-fatal: record failure and continue
            let err_msg = e.to_string();
            tracing::warn!(identifier, key, "file upload failed, continuing: {err_msg}");

            if let Some(ref cb) = progress {
                cb(UploadProgress {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    bytes_sent: 0,
                    total_bytes: 0,
                    status: UploadProgressStatus::Failed(err_msg.clone()),
                });
            }

            let file_size = tokio::fs::metadata(file)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let result = UploadResult {
                identifier: identifier.to_string(),
                key: key.to_string(),
                status: UploadStatus::Failed(err_msg),
                bytes: file_size,
                md5: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                retries: 0,
            };
            if let Some(ref cb) = on_result {
                cb(&result);
            }
            Ok((result, first_file_succeeded))
        }
    }
}

/// Compute remote S3 keys for each file.
///
/// Rules:
/// - If `remote_name` is set and there is exactly one file: use that name
/// - If `keep_directories`: use relative path from file's parent structure
/// - Default: use just the filename (basename)
/// - If `remote_dir` is set: prepend `{remote_dir}/` to each key
fn compute_keys(files: &[PathBuf], opts: &UploadOpts) -> Result<Vec<String>> {
    let mut keys = Vec::with_capacity(files.len());

    for file in files {
        let key = if let Some(ref remote_name) = opts.remote_name {
            if files.len() == 1 {
                remote_name.clone()
            } else {
                // remote_name only applies to single file; use basename for multi
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.to_string_lossy().into_owned())
            }
        } else if opts.keep_directories {
            // Use the full path as given (relative path preserved)
            file.to_string_lossy().into_owned()
        } else {
            // Default: basename only
            file.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string_lossy().into_owned())
        };

        // Prepend remote_dir if set
        let key = if let Some(ref dir) = opts.remote_dir {
            format!("{dir}/{key}")
        } else {
            key
        };

        keys.push(key);
    }

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    /// Create a default client suitable for dry-run tests (no HTTP needed).
    fn dry_run_client() -> crate::IaClient {
        crate::IaClient::from_config(crate::IaConfig::default()).unwrap()
    }

    /// Build opts for resume tests: dry_run + no_collection_check to avoid HTTP.
    fn resume_test_opts() -> UploadOpts {
        UploadOpts {
            dry_run: true,
            no_collection_check: true,
            ..Default::default()
        }
    }

    // -- compute_keys tests --

    #[test]
    fn keys_basename_default() {
        let files = vec![PathBuf::from("/tmp/dir/file.txt")];
        let opts = UploadOpts::default();
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["file.txt"]);
    }

    #[test]
    fn keys_with_remote_name_single() {
        let files = vec![PathBuf::from("/tmp/dir/file.txt")];
        let opts = UploadOpts {
            remote_name: Some("renamed.txt".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["renamed.txt"]);
    }

    #[test]
    fn keys_with_remote_name_multi_uses_basename() {
        let files = vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")];
        let opts = UploadOpts {
            remote_name: Some("renamed.txt".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn keys_with_remote_dir() {
        let files = vec![PathBuf::from("/tmp/file.txt")];
        let opts = UploadOpts {
            remote_dir: Some("scans".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["scans/file.txt"]);
    }

    #[test]
    fn keys_keep_directories() {
        let files = vec![PathBuf::from("relative/path/file.txt")];
        let opts = UploadOpts {
            keep_directories: true,
            ..Default::default()
        };
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["relative/path/file.txt"]);
    }

    #[test]
    fn keys_remote_dir_plus_keep_directories() {
        let files = vec![PathBuf::from("dir/file.txt")];
        let opts = UploadOpts {
            keep_directories: true,
            remote_dir: Some("prefix".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &opts).unwrap();
        assert_eq!(keys, vec!["prefix/dir/file.txt"]);
    }

    // -- upload_item resume (skip_set) tests --

    #[tokio::test]
    async fn upload_item_skips_resumed_files() {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        fs::write(&f1, "hello").unwrap();
        fs::write(&f2, "world").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // Mark a.txt as already uploaded
        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "a.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert!(
            matches!(results[0].status, UploadStatus::Resumed),
            "a.txt should be Resumed, got {:?}",
            results[0].status
        );
        assert_eq!(results[0].key, "a.txt");
        assert_eq!(results[0].bytes, 5); // "hello" = 5 bytes
        assert!(
            matches!(results[1].status, UploadStatus::DryRun),
            "b.txt should be DryRun, got {:?}",
            results[1].status
        );
    }

    #[tokio::test]
    async fn upload_item_all_resumed_no_uploads() {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "a.txt".to_string()));
        skip.insert(("test-item".to_string(), "b.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r.status, UploadStatus::Resumed)));
    }

    #[tokio::test]
    async fn upload_item_is_first_after_resume() {
        // When file 0 is resumed, file 1 should be treated as is_first=true
        // (gets metadata headers). With dry_run, we just verify no panic and correct results.
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        let f3 = dir.path().join("c.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();
        fs::write(&f3, "ccc").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // Skip only the first file
        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "a.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2, f3],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 3);
        assert!(matches!(results[0].status, UploadStatus::Resumed));
        // b.txt and c.txt should be DryRun (they actually get processed)
        assert!(matches!(results[1].status, UploadStatus::DryRun));
        assert!(matches!(results[2].status, UploadStatus::DryRun));
    }

    #[tokio::test]
    async fn upload_item_is_last_correct_with_resume() {
        // When the last file is in the skip set, derive should trigger on the
        // last non-resumed file. With dry_run we can't directly observe is_last,
        // but we verify the function completes correctly and produces the right statuses.
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        let f3 = dir.path().join("c.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();
        fs::write(&f3, "ccc").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // Skip only the last file
        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "c.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2, f3],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 3);
        assert!(matches!(results[0].status, UploadStatus::DryRun));
        assert!(matches!(results[1].status, UploadStatus::DryRun));
        assert!(matches!(results[2].status, UploadStatus::Resumed));
        assert_eq!(results[2].key, "c.txt");
    }

    #[tokio::test]
    async fn upload_item_different_key_not_resumed() {
        // skip_set has a different key — the file should upload normally
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "hello").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // Skip set has a different filename for the same item
        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "other.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(
            matches!(results[0].status, UploadStatus::DryRun),
            "should not be resumed when key doesn't match"
        );
    }

    #[tokio::test]
    async fn upload_item_same_key_different_item() {
        // skip_set has (other-item, a.txt) — should NOT resume for test-item
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "data").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        let mut skip = HashSet::new();
        skip.insert(("other-item".to_string(), "a.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f],
            &opts,
            None,
            Some(&skip),
            None,
            1,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(
            matches!(results[0].status, UploadStatus::DryRun),
            "should not be resumed when identifier doesn't match"
        );
    }

    // -- concurrent upload tests --

    #[tokio::test]
    async fn upload_item_concurrent_dry_run_all_files_reported() {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        let f3 = dir.path().join("c.txt");
        let f4 = dir.path().join("d.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();
        fs::write(&f3, "ccc").unwrap();
        fs::write(&f4, "ddd").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2, f3, f4],
            &opts,
            None,
            None,
            None,
            2, // concurrent
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 4);
        assert!(results
            .iter()
            .all(|r| matches!(r.status, UploadStatus::DryRun)));
    }

    #[tokio::test]
    async fn upload_item_concurrent_two_files_stays_sequential() {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // file_concurrency=4, but only 2 files → falls through to sequential
        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2],
            &opts,
            None,
            None,
            None,
            4,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r.status, UploadStatus::DryRun)));
    }

    #[tokio::test]
    async fn upload_item_concurrent_with_skip_set() {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        let f3 = dir.path().join("c.txt");
        let f4 = dir.path().join("d.txt");
        fs::write(&f1, "aaa").unwrap();
        fs::write(&f2, "bbb").unwrap();
        fs::write(&f3, "ccc").unwrap();
        fs::write(&f4, "ddd").unwrap();

        let client = dry_run_client();
        let opts = resume_test_opts();

        // Mark b.txt as already uploaded (a middle file)
        let mut skip = HashSet::new();
        skip.insert(("test-item".to_string(), "b.txt".to_string()));

        let results = upload_item(
            &client,
            "test-item",
            &[f1, f2, f3, f4],
            &opts,
            None,
            Some(&skip),
            None,
            2,
            None,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 4);
        // Find the b.txt result and verify it's resumed
        let b_result = results.iter().find(|r| r.key == "b.txt").unwrap();
        assert!(
            matches!(b_result.status, UploadStatus::Resumed),
            "b.txt should be Resumed, got {:?}",
            b_result.status
        );
    }
}
