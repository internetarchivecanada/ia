use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};

use crate::error::{IaError, Result};
use crate::fs_util::expand_files;
use crate::identifier::validate_identifier;
use crate::upload::multipart::list_uploads_with_ctx;
use crate::upload::retry::S3RetryCtx;
use crate::upload::single::upload_file;
use crate::upload::types::{
    UploadOpts, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};
use crate::upload::validate::{validate_file, validate_required_metadata};
use crate::upload::validate_key;
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

    // 4. Expand directories and collect files. Each argument is expanded on
    //    its own so that `keep_directories` can name a file relative to the
    //    argument it came from (#96). The list is sorted by file, as before.
    let files_owned = files.to_vec();
    let mut pairs = tokio::task::spawn_blocking(move || -> Result<Vec<(PathBuf, PathBuf)>> {
        let mut pairs = Vec::new();
        for arg in &files_owned {
            for file in expand_files(std::slice::from_ref(arg))? {
                pairs.push((file, arg.clone()));
            }
        }
        Ok(pairs)
    })
    .await
    .map_err(|e| IaError::Io(std::io::Error::other(format!("spawn_blocking: {e}"))))??;
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let (expanded, sources): (Vec<PathBuf>, Vec<PathBuf>) = pairs.into_iter().unzip();

    // 5. Check for empty
    if expanded.is_empty() {
        return Err(IaError::EmptyUpload);
    }

    // 6. Validate each file (exists, not symlink)
    for file in &expanded {
        validate_file(file)?;
    }

    // 7. Compute remote keys
    let keys = compute_keys(&expanded, &sources, &opts)?;

    // 7b. One listing of the item's unfinished multipart uploads, so that a
    //     file which switched to multipart in an earlier run (#21), or was
    //     uploaded with `multipart` and left parts on IA, resumes from them
    //     instead of being sent again as a single PUT. Not with `multipart`
    //     (every file lists for itself) and not on a dry run (no request).
    if !opts.multipart && !opts.dry_run {
        record_unfinished_uploads(client, identifier, &opts).await;
    }

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

/// Ask IA once for the item's unfinished multipart uploads and record their
/// names on the run's [`MultipartFallback`] handle, with the run's retry
/// budget and, over all attempts, the response wait (120 s). Each attempt's
/// answer is bounded by `send_with_retry` (#60); the overall bound also
/// covers reading the answer's body, which that does not, and a run must
/// not hang before its first file on a server that accepts and goes silent.
/// A brand-new item has nothing to list. Any other failure does not fail
/// the item: the run warns and goes on as a run without the listing would,
/// sending every file as a single PUT, which is the behavior the listing
/// improves on, not one it replaces.
async fn record_unfinished_uploads(client: &IaClient, identifier: &str, opts: &UploadOpts) {
    let ctx = S3RetryCtx {
        identifier,
        key: "",
        retries: opts.retries,
        backoff: opts.backoff(),
        bytes_sent: 0,
        total_bytes: 0,
        progress: None,
    };
    tracing::info!(
        identifier,
        "checking {identifier} for unfinished multipart uploads"
    );
    let wait = crate::stall::response_wait();
    let listed = tokio::time::timeout(wait, list_uploads_with_ctx(client, &ctx)).await;
    let why = match listed {
        Ok(Ok(uploads)) => {
            if !uploads.is_empty() {
                tracing::info!(
                    identifier,
                    unfinished = uploads.len(),
                    "IA holds unfinished multipart uploads for this item; those files resume as multipart"
                );
            }
            opts.multipart_fallback
                .record_unfinished(identifier, uploads.into_iter().map(|u| u.key));
            return;
        }
        // The listing's own error already names the item.
        Ok(Err(IaError::UploadFailed { message, .. })) => message,
        Ok(Err(e)) => e.to_string(),
        Err(_elapsed) => format!("no answer within {} s", wait.as_secs()),
    };
    tracing::warn!(
        identifier,
        "could not list the unfinished multipart uploads of {identifier} ({why}); a file that \
         left parts on IA in an earlier run is sent again as a single PUT"
    );
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
/// `sources[i]` is the command-line argument `files[i]` came from: the file
/// itself, or the directory it was found under.
///
/// Rules:
/// - If `remote_name` is set and there is exactly one file: use that name
/// - If `keep_directories`: see [`keep_directories_key`]
/// - Default: use just the filename (basename)
/// - If `remote_dir` is set: prepend `{remote_dir}/` to each key, with
///   leading and trailing `/` trimmed from `remote_dir`
///
/// Every key is checked with `validate_key`, so a key that does not name a
/// file inside the item is refused here, before any request (#96).
fn compute_keys(files: &[PathBuf], sources: &[PathBuf], opts: &UploadOpts) -> Result<Vec<String>> {
    let mut keys = Vec::with_capacity(files.len());

    for (file, source) in files.iter().zip(sources) {
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
            keep_directories_key(file, source)
        } else {
            // Default: basename only
            file.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string_lossy().into_owned())
        };

        // Prepend remote_dir if set. Surrounding `/` are trimmed so
        // `scans/` and `/scans/` give `scans/a.bin`, not `scans//a.bin`
        // (#100); a value that is only slashes adds no prefix.
        let key = match opts.remote_dir.as_deref().map(|d| d.trim_matches('/')) {
            Some(dir) if !dir.is_empty() => format!("{dir}/{key}"),
            Some(_) | None => key,
        };

        validate_key(&key)?;
        keys.push(key);
    }

    Ok(keys)
}

/// The `--keep-directories` key for `file`, found under the command-line
/// argument `source` (#96).
///
/// Both paths are normalized lexically: `.` is dropped and `name/..` pairs
/// cancel. Then:
/// - a relative argument that does not start with `..` keeps the file's path
///   as given (`./scans/a.pdf` → `scans/a.pdf`);
/// - an absolute argument, or one starting with `..`, keeps only its own last
///   name and what is below it (`../scans` and `/data/scans` both give
///   `scans/a.pdf`);
/// - an argument with no last name (`..`, `/`) keeps only what is below it.
///
/// Components are joined with `/`. The caller validates the result, so a
/// path that cannot be named inside the item is refused, not sent.
fn keep_directories_key(file: &Path, source: &Path) -> String {
    let file = normalize(file);
    let source = normalize(source);
    let below_cwd = source
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    let base = if below_cwd {
        Path::new("")
    } else if source.file_name().is_some() {
        source.parent().unwrap_or(Path::new(""))
    } else {
        source.as_path()
    };
    // `file` is `source` with names appended, so the prefix always strips;
    // if it ever did not, the full path is kept and validation refuses it.
    let rel = file.strip_prefix(base).unwrap_or(&file);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// `path` with `.` components dropped and each `name/..` pair removed,
/// without touching the filesystem. A `..` directly under the root is
/// dropped (`/..` is `/`); a leading `..` of a relative path is kept.
fn normalize(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::ParentDir | Component::CurDir) | None => out.push(c),
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(c),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    /// Client for dry-run tests. Dry-run still fetches item metadata for resume
    /// detection, so point it at a port that refuses connections rather than at
    /// archive.org; the code path tolerates the failure.
    fn dry_run_client() -> crate::IaClient {
        let mut config = crate::IaConfig::default();
        config.general.host = "127.0.0.1:1".to_string();
        crate::IaClient::from_config(config).unwrap()
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
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["file.txt"]);
    }

    #[test]
    fn keys_with_remote_name_single() {
        let files = vec![PathBuf::from("/tmp/dir/file.txt")];
        let opts = UploadOpts {
            remote_name: Some("renamed.txt".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["renamed.txt"]);
    }

    #[test]
    fn keys_with_remote_name_multi_uses_basename() {
        let files = vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")];
        let opts = UploadOpts {
            remote_name: Some("renamed.txt".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn keys_with_remote_dir() {
        let files = vec![PathBuf::from("/tmp/file.txt")];
        let opts = UploadOpts {
            remote_dir: Some("scans".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["scans/file.txt"]);
    }

    #[test]
    fn keys_with_remote_dir_trim_surrounding_slashes() {
        let files = vec![PathBuf::from("/tmp/file.txt")];
        for (dir, want) in [
            ("scans/", "scans/file.txt"),
            ("/scans/", "scans/file.txt"),
            ("//scans//", "scans/file.txt"),
            ("/scans/sub/", "scans/sub/file.txt"),
            ("/", "file.txt"),
        ] {
            let opts = UploadOpts {
                remote_dir: Some(dir.into()),
                ..Default::default()
            };
            let keys = compute_keys(&files, &files, &opts).unwrap();
            assert_eq!(keys, vec![want], "--remote-dir {dir}");
        }
    }

    #[test]
    fn keys_keep_directories() {
        let files = vec![PathBuf::from("relative/path/file.txt")];
        let opts = UploadOpts {
            keep_directories: true,
            ..Default::default()
        };
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["relative/path/file.txt"]);
    }

    /// (file, argument it came from, expected key) for `keep_directories` (#96).
    #[test]
    fn keep_directories_keys_stay_inside_the_item() {
        let cases = [
            // Relative, below the current directory: the path as given.
            ("scans/a.pdf", "scans", "scans/a.pdf"),
            ("./scans/sub/a.pdf", "./scans", "scans/sub/a.pdf"),
            ("./a.pdf", ".", "a.pdf"),
            ("scans/../other/a.pdf", "scans/../other", "other/a.pdf"),
            // Through `..` or absolute: the argument's last name down.
            ("../scans/a.pdf", "../scans", "scans/a.pdf"),
            (
                "../../x/scans/sub/a.pdf",
                "../../x/scans",
                "scans/sub/a.pdf",
            ),
            ("a/../../scans/a.pdf", "a/../../scans", "scans/a.pdf"),
            ("/data/scans/a.pdf", "/data/scans", "scans/a.pdf"),
            ("/data/scans/sub/a.pdf", "/data/scans/", "scans/sub/a.pdf"),
            ("/data/a.pdf", "/data/a.pdf", "a.pdf"),
            ("../a.pdf", "../a.pdf", "a.pdf"),
            // An argument with no last name: only what is below it.
            ("../x/a.pdf", "..", "x/a.pdf"),
            ("../../a.pdf", "../..", "a.pdf"),
            ("/a.pdf", "/", "a.pdf"),
        ];
        let opts = UploadOpts {
            keep_directories: true,
            ..Default::default()
        };
        for (file, source, want) in cases {
            let keys =
                compute_keys(&[PathBuf::from(file)], &[PathBuf::from(source)], &opts).unwrap();
            assert_eq!(keys, vec![want], "file {file}, argument {source}");
        }
    }

    #[test]
    fn keys_that_leave_the_item_are_refused() {
        let files = vec![PathBuf::from("a.bin")];
        for opts in [
            UploadOpts {
                remote_name: Some("../other/a.bin".into()),
                ..Default::default()
            },
            UploadOpts {
                remote_name: Some("/a.bin".into()),
                ..Default::default()
            },
            UploadOpts {
                remote_dir: Some("../other".into()),
                ..Default::default()
            },
            UploadOpts {
                remote_dir: Some("scans/./sub".into()),
                ..Default::default()
            },
        ] {
            let err = compute_keys(&files, &files, &opts).unwrap_err();
            assert!(matches!(err, IaError::InvalidKey { .. }), "{err:?}");
        }
    }

    #[test]
    fn keys_remote_dir_plus_keep_directories() {
        let files = vec![PathBuf::from("dir/file.txt")];
        let opts = UploadOpts {
            keep_directories: true,
            remote_dir: Some("prefix".into()),
            ..Default::default()
        };
        let keys = compute_keys(&files, &files, &opts).unwrap();
        assert_eq!(keys, vec!["prefix/dir/file.txt"]);
    }

    // -- upload_item resume (skip_set) tests --

    /// The listing has no body, so nothing in the stall rules bounds its
    /// answer (#60); a server that accepts the connection and goes silent
    /// must not hang the run before its first file. The listing is given
    /// the response wait in all, then the run warns and sends single PUTs.
    #[tokio::test]
    async fn a_listing_that_is_never_answered_is_abandoned_and_the_run_goes_on() {
        use crate::upload::stall_watch::test_support::{
            shrink_policy, stalling_then_forwarding_proxy,
        };
        use wiremock::matchers::{method, path, query_param_is_missing};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _policy = shrink_policy();
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/test-item/a.bin"))
            .and(query_param_is_missing("partNumber"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let target: std::net::SocketAddr = server
            .uri()
            .strip_prefix("http://")
            .unwrap()
            .parse()
            .unwrap();
        let proxy = stalling_then_forwarding_proxy(target).await;

        let mut config = crate::IaConfig::default();
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        config.general.host = proxy.to_string();
        config.general.secure = false;
        let client = crate::IaClient::from_config_no_retry(config).unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("a.bin");
        std::fs::write(&f, vec![0x42u8; 1024]).unwrap();
        let opts = UploadOpts {
            verify: false,
            checksum: false,
            no_collection_check: true,
            retries: 1,
            ..Default::default()
        };

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            upload_item(&client, "test-item", &[f], &opts, None, None, None, 1, None),
        )
        .await
        .expect("the silent listing must be abandoned within the shrunk response wait")
        .unwrap();

        assert!(
            matches!(results[0].status, UploadStatus::Uploaded),
            "{results:?}"
        );
        assert!(!opts.multipart_fallback.has_unfinished("test-item", "a.bin"));
        server.verify().await;
    }

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
