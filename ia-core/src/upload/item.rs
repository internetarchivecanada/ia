use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{IaError, Result};
use crate::upload::single::upload_file;
use crate::upload::types::{UploadOpts, UploadProgress, UploadProgressStatus, UploadResult};
use crate::upload::validate::{validate_file, validate_identifier, validate_required_metadata};
use crate::IaClient;

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
/// - Sequential upload (files uploaded one at a time to avoid catalog congestion)
///
/// # Errors
///
/// Returns `IaError::InvalidIdentifier` if the identifier is malformed.
/// Returns `IaError::MissingRequiredMetadata` if required metadata fields are absent.
/// Returns `IaError::EmptyUpload` if no files remain after expansion and filtering.
/// Returns `IaError::SymlinkSkipped` if a file is a symlink (during validation).
/// Propagates any errors from individual `upload_file()` calls.
pub async fn upload_item(
    client: &IaClient,
    identifier: &str,
    files: &[PathBuf],
    opts: &UploadOpts,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
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
        .map_err(|e| IaError::Io(std::io::Error::other(format!("spawn_blocking: {e}"))))?
    };
    let size_hint = if opts.no_size_hint { None } else { Some(total_bytes) };

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

    // 10. Upload sequentially
    let mut results = Vec::with_capacity(file_count);

    for (i, (file, key)) in expanded.iter().zip(keys.iter()).enumerate() {
        let is_first = i == 0;
        let is_last = i == file_count - 1;

        // Size hint only on first file
        let hint = if is_first { size_hint } else { None };

        let result = upload_file(
            client, identifier, file, key, &opts, is_first, is_last, hint, progress.clone(),
        )
        .await?;

        results.push(result);
    }

    Ok(results)
}

/// Expand a list of paths, recursively walking directories.
///
/// Skips symlinks and dotfiles/dotdirs (entries starting with `.`).
/// Regular files are passed through unchanged.
fn expand_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk_dir(path, &mut result)?;
        } else {
            result.push(path.clone());
        }
    }
    // Sort for deterministic ordering (important for first/last file logic)
    result.sort();
    Ok(result)
}

/// Recursively walk a directory, collecting regular files.
///
/// Skips symlinks and entries whose filename starts with `.`.
fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        // Skip dotfiles and dotdirs
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }

        // Skip symlinks
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            walk_dir(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
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
    use std::fs;
    use tempfile::TempDir;

    // -- expand_files tests --

    #[test]
    fn expand_single_file() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("test.txt");
        fs::write(&f, "content").unwrap();

        let result = expand_files(&[f.clone()]).unwrap();
        assert_eq!(result, vec![f]);
    }

    #[test]
    fn expand_directory_skips_dotfiles() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("visible.txt"), "yes").unwrap();
        fs::write(dir.path().join(".hidden"), "no").unwrap();

        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].file_name().unwrap().to_str().unwrap() == "visible.txt");
    }

    #[test]
    fn expand_directory_recursive() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(sub.join("b.txt"), "b").unwrap();

        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn expand_empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert!(result.is_empty());
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
        let files = vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
        ];
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
}
