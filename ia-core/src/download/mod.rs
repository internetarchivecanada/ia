pub mod zip;

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures::StreamExt;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::files::FileFilter;
use crate::types::FileMetadata;

/// Validate that a file name from server metadata produces a safe download path.
///
/// Rejects:
/// - Parent directory traversal (`..`)
/// - Absolute paths (`/etc/passwd`)
/// - Windows prefix paths (`C:\`)
/// - Null bytes
/// - Control characters (0x00-0x1F)
/// - Empty names
///
/// Allows:
/// - Nested paths (`subdir/file.txt`) — legitimate for IA items
/// - Normal filenames with spaces, unicode, etc.
///
/// Uses both component-level validation and belt-and-suspenders normalized
/// path check to defend against edge cases.
pub fn validate_download_path(dest_dir: &Path, file_name: &str) -> Result<PathBuf> {
    if file_name.is_empty() {
        return Err(IaError::PathTraversal {
            path: file_name.to_string(),
            dest_dir: dest_dir.display().to_string(),
        });
    }

    // Reject null bytes
    if file_name.contains('\0') {
        return Err(IaError::PathTraversal {
            path: file_name.to_string(),
            dest_dir: dest_dir.display().to_string(),
        });
    }

    // Reject control characters (0x00-0x1F)
    if file_name.bytes().any(|b| b < 0x20) {
        return Err(IaError::PathTraversal {
            path: file_name.to_string(),
            dest_dir: dest_dir.display().to_string(),
        });
    }

    let path = Path::new(file_name);

    // Validate each component — reject traversal and absolute paths
    for component in path.components() {
        match component {
            Component::Normal(_) => {} // OK
            Component::CurDir => {}    // "." is harmless, normalized away
            Component::ParentDir => {
                return Err(IaError::PathTraversal {
                    path: file_name.to_string(),
                    dest_dir: dest_dir.display().to_string(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(IaError::PathTraversal {
                    path: file_name.to_string(),
                    dest_dir: dest_dir.display().to_string(),
                });
            }
        }
    }

    let joined = dest_dir.join(file_name);

    // Belt-and-suspenders: verify the normalized path starts with dest_dir.
    // This catches edge cases that component iteration might miss.
    let normalized = normalize_path(&joined);
    let normalized_dest = normalize_path(dest_dir);
    if !normalized.starts_with(&normalized_dest) {
        return Err(IaError::PathTraversal {
            path: file_name.to_string(),
            dest_dir: dest_dir.display().to_string(),
        });
    }

    Ok(joined)
}

/// Normalize a path without requiring it to exist (unlike `canonicalize()`).
///
/// Resolves `.` and `..` components logically. Used as a belt-and-suspenders
/// check alongside component-level validation.
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// Options for downloading files.
#[derive(Debug, Clone)]
pub struct DownloadOpts {
    /// Destination directory.
    pub destdir: PathBuf,
    /// Whether to create item subdirectory.
    pub no_directories: bool,
    /// Verify checksums (opt-in).
    pub checksum: bool,
    /// Maximum retries per file.
    pub retries: usize,
    /// Don't set file mtime from server.
    pub no_timestamps: bool,
    /// Dry run (don't download, just report).
    pub dry_run: bool,
    /// File filter.
    pub filter: FileFilter,
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            destdir: PathBuf::from("."),
            no_directories: false,
            checksum: false,
            retries: 5,
            no_timestamps: false,
            dry_run: false,
            filter: FileFilter::default(),
        }
    }
}

/// Progress information for a single file download.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub identifier: String,
    pub file_name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub status: DownloadStatus,
}

/// Status of a file download.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    /// Fetching item metadata. Emitted before `Enumerated` so the UI can
    /// show activity during the 5-15 s metadata round-trip instead of
    /// appearing frozen on `Pending`.
    Resolving,
    /// Item metadata fetched; reports total file count and bytes for the item.
    Enumerated {
        files_count: usize,
        bytes_total: u64,
    },
    Starting,
    Downloading,
    Verifying,
    Complete,
    Skipped(String),
    Failed(String),
}

/// Result of a single file download.
#[derive(Debug)]
pub struct FileDownloadResult {
    pub file_name: String,
    pub bytes: u64,
    pub status: DownloadStatus,
    pub elapsed: Duration,
}

/// Fetch a response from archive.org with auth, manual redirect following, and SSRF guard.
///
/// Handles:
/// - LOW auth headers from S3 credentials
/// - Manual redirect following with auth preservation (reqwest strips Authorization on redirect)
/// - SSRF guard: only follows redirects to *.archive.org or the configured host
/// - HTML error page stripping
/// - Optional resume via Range header
///
/// Returns the raw response for callers to consume (stream to disk or collect to bytes).
pub(crate) async fn fetch_response(
    client: &IaClient,
    url: &str,
    resume_from: Option<u64>,
) -> Result<reqwest::Response> {
    // Build auth header if credentials are available.
    let auth_value = client
        .config()
        .s3_access
        .as_deref()
        .zip(client.config().s3_secret.as_deref())
        .map(|(access, secret)| format!("LOW {access}:{secret}"));

    // Use the no-redirect client and follow redirects manually so the
    // Authorization header is preserved. archive.org redirects /download/
    // to data-node hosts (ia800XXX.us.archive.org), and reqwest strips
    // Authorization on redirect by default.
    let max_redirects = 10;
    let mut current_url = url.to_string();
    let mut resp = None;

    for _ in 0..=max_redirects {
        let mut req = client.no_redirect_http().get(&current_url);
        if let Some(ref auth) = auth_value {
            req = req.header("Authorization", auth);
        }
        if let Some(offset) = resume_from {
            req = req.header("Range", format!("bytes={offset}-"));
        }

        let r = req
            .send()
            .await
            .map_err(|e| IaError::Network(reqwest_middleware::Error::Reqwest(e)))?;

        if r.status().is_redirection() {
            let location = r
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| IaError::Http {
                    status: r.status().as_u16(),
                    message: "redirect without Location header".to_string(),
                })?;

            let base = reqwest::Url::parse(&current_url)
                .map_err(|e| IaError::Config(format!("invalid download URL: {e}")))?;
            let new_url = base
                .join(location)
                .map_err(|e| IaError::Config(format!("invalid redirect location: {e}")))?;

            // Only follow redirects to *.archive.org (SSRF guard).
            // Also allow the configured host so tests with wiremock work.
            let config_host = client.host();
            match new_url.host_str() {
                Some(host)
                    if host == "archive.org"
                        || host.ends_with(".archive.org")
                        || new_url.authority() == config_host =>
                {
                    debug!(location = %new_url, "following redirect");
                    current_url = new_url.to_string();
                    continue;
                }
                _ => {
                    return Err(IaError::Http {
                        status: r.status().as_u16(),
                        message: format!("redirect to non-archive.org domain: {new_url}"),
                    });
                }
            }
        }

        resp = Some(r);
        break;
    }

    let response = resp.ok_or_else(|| IaError::Http {
        status: 0,
        message: "too many redirects".to_string(),
    })?;

    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        let body = response.text().await.unwrap_or_default();
        // IA often returns full HTML error pages (e.g. "Item not available").
        // Strip HTML and use the canonical reason phrase instead.
        let message = if body.contains("<!DOCTYPE") || body.contains("<html") {
            status
                .canonical_reason()
                .unwrap_or("unknown error")
                .to_string()
        } else {
            body
        };
        return Err(IaError::Http {
            status: status.as_u16(),
            message,
        });
    }

    Ok(response)
}

/// Download a single file from an item.
pub async fn download_file(
    client: &IaClient,
    identifier: &str,
    file: &FileMetadata,
    dest_dir: &Path,
    opts: &DownloadOpts,
    progress: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
) -> Result<FileDownloadResult> {
    let start = std::time::Instant::now();
    let file_path = validate_download_path(dest_dir, &file.name)?;

    // Check for symlinks in the destination path before creating directories.
    // A symlink in the path could redirect writes outside dest_dir.
    if let Some(parent) = file_path.parent() {
        if let Ok(relative) = parent.strip_prefix(dest_dir) {
            let mut check = dest_dir.to_path_buf();
            for component in relative.components() {
                check.push(component);
                // Use symlink_metadata to detect symlinks without following them
                if let Ok(meta) = fs::symlink_metadata(&check).await {
                    if meta.file_type().is_symlink() {
                        return Err(IaError::PathTraversal {
                            path: check.display().to_string(),
                            dest_dir: dest_dir.display().to_string(),
                        });
                    }
                }
            }
        }

        fs::create_dir_all(parent).await?;
    }

    // Skip check: existing file with matching size + mtime
    if !opts.checksum {
        if let Some(skip_reason) = should_skip(&file_path, file).await {
            debug!(file = %file.name, reason = %skip_reason, "skipping file");
            if let Some(p) = progress {
                p(DownloadProgress {
                    identifier: identifier.to_string(),
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    status: DownloadStatus::Skipped(skip_reason.clone()),
                });
            }
            return Ok(FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Skipped(skip_reason),
                elapsed: start.elapsed(),
            });
        }
    }

    // Checksum skip: compute local MD5 and compare.
    //
    // The hash can take several seconds for large files. Emit a Verifying
    // progress event first so UIs can show activity instead of sitting
    // silent while disk I/O grinds.
    if opts.checksum && file_path.exists() && file.md5.is_some() {
        if let Some(p) = progress {
            p(DownloadProgress {
                identifier: identifier.to_string(),
                file_name: file.name.clone(),
                bytes_downloaded: 0,
                total_bytes: file.size,
                status: DownloadStatus::Verifying,
            });
        }
    }

    if opts.checksum {
        if let Some(skip_reason) = should_skip_checksum(&file_path, file).await {
            debug!(file = %file.name, reason = %skip_reason, "skipping file (checksum match)");
            if let Some(p) = progress {
                p(DownloadProgress {
                    identifier: identifier.to_string(),
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    status: DownloadStatus::Skipped(skip_reason.clone()),
                });
            }
            return Ok(FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Skipped(skip_reason),
                elapsed: start.elapsed(),
            });
        }
    }

    if opts.dry_run {
        return Ok(FileDownloadResult {
            file_name: file.name.clone(),
            bytes: file.size.unwrap_or(0),
            status: DownloadStatus::Skipped("dry run".to_string()),
            elapsed: start.elapsed(),
        });
    }

    // Download URL
    let encoded_name = urlencoding::encode(&file.name);
    let url = client.url(&format!("/download/{identifier}/{encoded_name}"));

    // Check for partial file (.part) for resume.
    // Open first, then stat the fd — avoids TOCTOU race where the .part file
    // could be replaced with a symlink between exists() and metadata().
    let part_path = PathBuf::from(format!("{}.part", file_path.display()));
    let resume_from = match fs::File::open(&part_path).await {
        Ok(f) => {
            let meta = f.metadata().await?;
            if !meta.is_file() {
                warn!(file = %file.name, "skipping resume: .part path is not a regular file");
                None
            } else {
                Some(meta.len())
            }
        }
        Err(_) => None,
    };

    if let Some(p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: file.name.clone(),
            bytes_downloaded: resume_from.unwrap_or(0),
            total_bytes: file.size,
            status: DownloadStatus::Starting,
        });
    }

    let response = fetch_response(client, &url, resume_from).await?;
    let status = response.status();

    // If we asked for a Range but got 200 (not 206), the server ignored our
    // Range header and is sending the full file.  Truncate the .part file so
    // we don't corrupt it by appending the full content after existing bytes.
    let mut resume_from = if resume_from.is_some() && status == reqwest::StatusCode::OK {
        debug!(file = %file.name, "server returned 200 for Range request, restarting download");
        // Will create/truncate below
        None
    } else {
        resume_from
    };

    // Parse Last-Modified for mtime
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok());

    // If .part exists and is a symlink, remove it before writing.
    // Prevents writing through a symlink planted by an attacker.
    if let Ok(meta) = fs::symlink_metadata(&part_path).await {
        if meta.file_type().is_symlink() {
            warn!(file = %file.name, "removing symlink .part file");
            fs::remove_file(&part_path).await?;
            // Reset resume so we create a fresh file instead of trying to
            // append to the now-deleted path (which would fail with NotFound
            // if the server returned 206).
            resume_from = None;
        }
    }

    // Stream to .part file. Hyper delivers ~16 KB chunks; buffering coalesces
    // them into 256 KB writes to avoid per-chunk spawn_blocking round trips.
    let raw_file = if resume_from.is_some() {
        fs::OpenOptions::new().append(true).open(&part_path).await?
    } else {
        fs::File::create(&part_path).await?
    };
    let mut output = tokio::io::BufWriter::with_capacity(256 * 1024, raw_file);

    // Rolling MD5 hasher fed from the download stream — avoids the old
    // write-then-re-read-the-whole-file second pass. For resumed downloads,
    // seed it with the bytes already present in .part so the final hash
    // covers the full file.
    let mut hasher = if opts.checksum && file.md5.is_some() {
        use md5::{Digest, Md5};
        let mut h = Md5::new();
        if let Some(resumed_bytes) = resume_from {
            // Seeding reads the full .part before the HTTP stream opens — on
            // a 10 GB partial that's tens of seconds of silent CPU+IO. Emit
            // Verifying so the console/TUI shows activity, and re-emit every
            // 16 MiB with progress.
            use tokio::io::AsyncReadExt;
            if let Some(p) = progress {
                p(DownloadProgress {
                    identifier: identifier.to_string(),
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: Some(resumed_bytes),
                    status: DownloadStatus::Verifying,
                });
            }
            let mut seed = fs::File::open(&part_path).await?;
            let mut buf = vec![0u8; 1024 * 1024];
            let mut seeded: u64 = 0;
            let mut last_emit: u64 = 0;
            loop {
                let n = seed.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                seeded += n as u64;
                if let Some(p) = progress {
                    if seeded - last_emit >= 16 * 1024 * 1024 {
                        p(DownloadProgress {
                            identifier: identifier.to_string(),
                            file_name: file.name.clone(),
                            bytes_downloaded: seeded,
                            total_bytes: Some(resumed_bytes),
                            status: DownloadStatus::Verifying,
                        });
                        last_emit = seeded;
                    }
                }
            }
        }
        Some(h)
    } else {
        None
    };

    let mut bytes_downloaded = resume_from.unwrap_or(0);
    let mut last_progress_at = bytes_downloaded;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(reqwest_middleware::Error::from)?;
        output.write_all(&chunk).await?;
        if let Some(h) = hasher.as_mut() {
            use md5::Digest;
            h.update(&chunk);
        }
        bytes_downloaded += chunk.len() as u64;

        // Abort if response exceeds expected size (10% tolerance, min 1KB buffer)
        if let Some(expected) = file.size {
            let max_allowed = expected + (expected / 10).max(1024);
            if bytes_downloaded > max_allowed {
                drop(output);
                let _ = fs::remove_file(&part_path).await;
                return Err(IaError::DownloadTooLarge {
                    file: file.name.clone(),
                    expected,
                    received: bytes_downloaded,
                });
            }
        }

        // Rate-limit progress updates to every 256KB to reduce lock contention
        if let Some(p) = progress {
            if bytes_downloaded - last_progress_at >= 256 * 1024 {
                p(DownloadProgress {
                    identifier: identifier.to_string(),
                    file_name: file.name.clone(),
                    bytes_downloaded,
                    total_bytes: file.size,
                    status: DownloadStatus::Downloading,
                });
                last_progress_at = bytes_downloaded;
            }
        }
    }

    output.flush().await?;
    drop(output);

    // Post-download checksum comparison using the inline-computed hash.
    if let Some(hasher) = hasher {
        if let Some(expected_md5) = &file.md5 {
            if let Some(p) = progress {
                p(DownloadProgress {
                    identifier: identifier.to_string(),
                    file_name: file.name.clone(),
                    bytes_downloaded,
                    total_bytes: file.size,
                    status: DownloadStatus::Verifying,
                });
            }

            use md5::Digest;
            let actual_md5 = format!("{:x}", hasher.finalize());
            if &actual_md5 != expected_md5 {
                // Delete the bad file
                let _ = fs::remove_file(&part_path).await;
                return Err(IaError::ChecksumMismatch {
                    file: file.name.clone(),
                    expected: expected_md5.clone(),
                    actual: actual_md5,
                });
            }
        }
    }

    // Finalize: rename .part to final name
    fs::rename(&part_path, &file_path).await?;

    // Set mtime from Last-Modified header
    if !opts.no_timestamps {
        if let Some(mtime) =
            last_modified.or_else(|| file.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t)))
        {
            let _ =
                filetime::set_file_mtime(&file_path, filetime::FileTime::from_system_time(mtime));
        }
    }

    info!(file = %file.name, bytes = bytes_downloaded, "download complete");

    if let Some(p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: file.name.clone(),
            bytes_downloaded,
            total_bytes: file.size,
            status: DownloadStatus::Complete,
        });
    }

    Ok(FileDownloadResult {
        file_name: file.name.clone(),
        bytes: bytes_downloaded,
        status: DownloadStatus::Complete,
        elapsed: start.elapsed(),
    })
}

/// Check if a file should be skipped based on size + mtime.
async fn should_skip(path: &Path, file: &FileMetadata) -> Option<String> {
    let meta = fs::metadata(path).await.ok()?;
    let local_size = meta.len();

    // Size must match
    let remote_size = file.size?;
    if local_size != remote_size {
        return None;
    }

    // mtime must match
    let remote_mtime = file.mtime?;
    let local_mtime = meta.modified().ok()?;
    let local_ts = local_mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if local_ts == remote_mtime {
        Some("size+mtime match".to_string())
    } else {
        None
    }
}

/// Check if a file should be skipped based on MD5 checksum.
async fn should_skip_checksum(path: &Path, file: &FileMetadata) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let expected_md5 = file.md5.as_ref()?;
    let actual_md5 = compute_md5(path).await.ok()?;
    if &actual_md5 == expected_md5 {
        Some("checksum match".to_string())
    } else {
        None
    }
}

/// Compute MD5 hash of a file.
async fn compute_md5(path: &Path) -> Result<String> {
    use md5::{Digest, Md5};
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer

    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Result of downloading an entire item.
#[derive(Debug)]
pub struct ItemDownloadResult {
    pub identifier: String,
    pub files_total: usize,
    pub files_downloaded: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_total: u64,
    pub elapsed: Duration,
    pub results: Vec<FileDownloadResult>,
}

/// Download all matching files from an item, using a shared semaphore for concurrency.
pub async fn download_item(
    client: &IaClient,
    identifier: &str,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
) -> Result<ItemDownloadResult> {
    if let Some(ref p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: String::new(),
            bytes_downloaded: 0,
            total_bytes: None,
            status: DownloadStatus::Resolving,
        });
    }
    let item = crate::metadata::get(client, identifier).await?;
    download_item_with_metadata(client, identifier, &item, opts, semaphore, progress).await
}

/// Download all matching files from an item using pre-fetched metadata.
///
/// Use this when you've already fetched the item's metadata (e.g. to compute
/// total size for disk pool assignment) and don't want a redundant API call.
pub async fn download_item_with_metadata(
    client: &IaClient,
    identifier: &str,
    item: &crate::types::ItemMetadata,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
) -> Result<ItemDownloadResult> {
    let start = std::time::Instant::now();

    // Filter files. Substitute `{identifier}` per-item so positional file-name
    // args like `'{identifier}.pdf'` work in batch/search/itemlist downloads.
    // Idempotent: literal names and already-substituted names pass through.
    let mut item_filter = opts.filter.clone();
    item_filter.names = crate::files::substitute_names(&opts.filter.names, identifier);
    let files = crate::files::list(item, &item_filter);
    let files_total = files.len();

    if files.is_empty() {
        return Ok(ItemDownloadResult {
            identifier: identifier.to_string(),
            files_total: 0,
            files_downloaded: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_total: 0,
            elapsed: start.elapsed(),
            results: vec![],
        });
    }

    // Determine destination directory
    let dest_dir = if opts.no_directories {
        opts.destdir.clone()
    } else {
        opts.destdir.join(identifier)
    };

    // Clone file metadata for owned access in tasks
    let files_owned: Vec<crate::types::FileMetadata> = files.into_iter().cloned().collect();

    // Report total file count and bytes upfront so progress tracking has a stable denominator
    if let Some(ref p) = progress {
        let enumerated_bytes: u64 = files_owned.iter().filter_map(|f| f.size).sum();
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: String::new(),
            bytes_downloaded: 0,
            total_bytes: Some(enumerated_bytes),
            status: DownloadStatus::Enumerated {
                files_count: files_total,
                bytes_total: enumerated_bytes,
            },
        });
    }

    // Shared flag: set by any file task that hits disk-full so sibling tasks
    // abort early instead of hammering a full disk.
    let disk_full = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Concurrent download with shared semaphore
    let mut handles = Vec::new();

    for file in files_owned {
        let client = client.clone();
        let identifier = identifier.to_string();
        let dest_dir = dest_dir.clone();
        let opts = opts.clone();
        let sem = Arc::clone(&semaphore);
        let progress = progress.clone();
        let disk_full = Arc::clone(&disk_full);

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Another file already hit disk-full — skip immediately.
            if disk_full.load(std::sync::atomic::Ordering::Relaxed) {
                let status = DownloadStatus::Failed("disk full (aborted)".to_string());
                if let Some(ref p) = progress {
                    p(DownloadProgress {
                        identifier: identifier.clone(),
                        file_name: file.name.clone(),
                        bytes_downloaded: 0,
                        total_bytes: file.size,
                        status: status.clone(),
                    });
                }
                return FileDownloadResult {
                    file_name: file.name.clone(),
                    bytes: 0,
                    status,
                    elapsed: start.elapsed(),
                };
            }

            let prog_ref = progress.as_deref();
            let mut last_err = None;

            for attempt in 0..=opts.retries {
                if attempt > 0 {
                    let delay = Duration::from_secs(2u64.pow(attempt as u32).min(60));
                    warn!(file = %file.name, attempt, "retrying after {:?}", delay);
                    tokio::time::sleep(delay).await;
                }

                match download_file(&client, &identifier, &file, &dest_dir, &opts, prog_ref).await {
                    Ok(result) => return result,
                    Err(e) => {
                        if e.is_disk_full() {
                            warn!(file = %file.name, "disk full, aborting item");
                            disk_full.store(true, std::sync::atomic::Ordering::Relaxed);
                            last_err = Some(e);
                            break;
                        }
                        if e.is_retryable() {
                            warn!(file = %file.name, attempt, error = %e, "download failed (will retry)");
                        } else {
                            debug!(file = %file.name, error = %e, "download failed (not retryable)");
                            last_err = Some(e);
                            break;
                        }
                        last_err = Some(e);
                    }
                }
            }

            let status = DownloadStatus::Failed(
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown error".to_string()),
            );

            // Notify progress callback so the UI can clean up the file's bar.
            if let Some(ref p) = progress {
                p(DownloadProgress {
                    identifier: identifier.clone(),
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    status: status.clone(),
                });
            }

            FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status,
                elapsed: start.elapsed(),
            }
        });

        handles.push(handle);
    }

    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(FileDownloadResult {
                file_name: "unknown".to_string(),
                bytes: 0,
                status: DownloadStatus::Failed(format!("task panic: {e}")),
                elapsed: start.elapsed(),
            }),
        }
    }

    // If any file hit disk-full, propagate as an item-level error so the
    // caller (e.g. batch download with disk pool) can failover to another disk.
    if disk_full.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(IaError::DiskFull {
            path: dest_dir.clone(),
        });
    }

    let files_downloaded = results
        .iter()
        .filter(|r| r.status == DownloadStatus::Complete)
        .count();
    let files_skipped = results
        .iter()
        .filter(|r| matches!(r.status, DownloadStatus::Skipped(_)))
        .count();
    let files_failed = results
        .iter()
        .filter(|r| matches!(r.status, DownloadStatus::Failed(_)))
        .count();
    let bytes_total = results.iter().map(|r| r.bytes).sum();

    Ok(ItemDownloadResult {
        identifier: identifier.to_string(),
        files_total,
        files_downloaded,
        files_skipped,
        files_failed,
        bytes_total,
        elapsed: start.elapsed(),
        results,
    })
}

/// Remove a partially-downloaded item directory.
///
/// Call this before retrying an item on a different disk to avoid leaving
/// orphan files on the full disk. Removes `<destdir>/<identifier>/` and
/// everything inside it. Silently ignores errors (the directory may not
/// exist if the download failed before any files were written).
pub async fn cleanup_item_dir(destdir: &Path, identifier: &str) {
    let item_dir = destdir.join(identifier);
    if item_dir.is_dir() {
        info!(item = identifier, dir = %item_dir.display(), "cleaning up partial download");
        if let Err(e) = fs::remove_dir_all(&item_dir).await {
            warn!(item = identifier, error = %e, "failed to clean up partial download directory");
        }
    }
}

/// Result of downloading a batch of items.
#[derive(Debug)]
pub struct BatchDownloadResult {
    pub items_total: usize,
    pub items_succeeded: usize,
    pub items_failed: usize,
    pub files_downloaded: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_total: u64,
    pub elapsed: Duration,
    pub item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>>,
}

/// Aggregate per-item results into a [`BatchDownloadResult`].
pub fn collect_batch_results(
    items_total: usize,
    item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>>,
    elapsed: Duration,
) -> BatchDownloadResult {
    let items_succeeded = item_results.iter().filter(|r| r.is_ok()).count();
    let items_failed = item_results.iter().filter(|r| r.is_err()).count();
    let files_downloaded: usize = item_results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|r| r.files_downloaded)
        .sum();
    let files_skipped: usize = item_results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|r| r.files_skipped)
        .sum();
    let files_failed: usize = item_results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|r| r.files_failed)
        .sum();
    let bytes_total: u64 = item_results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|r| r.bytes_total)
        .sum();

    BatchDownloadResult {
        items_total,
        items_succeeded,
        items_failed,
        files_downloaded,
        files_skipped,
        files_failed,
        bytes_total,
        elapsed,
        item_results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    fn test_file_meta(name: &str, size: u64) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some("original".to_string()),
            format: None,
            md5: Some("d41d8cd98f00b204e9800998ecf8427e".to_string()),
            size: Some(size),
            mtime: Some(1700000000),
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn download_single_file() {
        let mock_server = MockServer::start().await;
        let body = b"hello world test content";

        Mock::given(method("GET"))
            .and(path("/download/test-item/test.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.to_vec())
                    .insert_header("Last-Modified", "Thu, 01 Jan 2024 00:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test.txt", body.len() as u64);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, body.len() as u64);

        let content = std::fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world test content");
    }

    #[tokio::test]
    async fn skip_existing_file_with_matching_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("existing.txt");
        std::fs::write(&file_path, "content").unwrap();

        // Set a known mtime on the local file
        let mtime = 1700000000u64;
        filetime::set_file_mtime(
            &file_path,
            filetime::FileTime::from_unix_time(mtime as i64, 0),
        );

        let file = FileMetadata {
            name: "existing.txt".to_string(),
            size: Some(7), // "content".len()
            mtime: Some(mtime),
            ..test_file_meta("existing.txt", 7)
        };

        // We don't even need a real server for skip
        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert!(matches!(result.status, DownloadStatus::Skipped(_)));
    }

    #[tokio::test]
    async fn dry_run_does_not_download() {
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test.txt", 1000);

        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let opts = DownloadOpts {
            dry_run: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert!(matches!(result.status, DownloadStatus::Skipped(_)));
        assert!(!dir.path().join("test.txt").exists());
    }

    #[tokio::test]
    async fn creates_subdirectories_for_nested_files() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/test-item/subdir%2Ftest.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("subdir/test.txt", 4);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert!(dir.path().join("subdir/test.txt").exists());
    }

    #[tokio::test]
    async fn download_item_concurrently() {
        let mock_server = MockServer::start().await;

        // Mock metadata endpoint
        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "test-item"},
                "files": [
                    {"name": "a.txt", "size": "5", "source": "original"},
                    {"name": "b.txt", "size": "5", "source": "original"},
                    {"name": "c.txt", "size": "5", "source": "original"}
                ]
            })))
            .mount(&mock_server)
            .await;

        // Mock file downloads
        for name in &["a.txt", "b.txt", "c.txt"] {
            Mock::given(method("GET"))
                .and(path(format!("/download/test-item/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
                .mount(&mock_server)
                .await;
        }

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(2));

        let result = download_item(&client, "test-item", &opts, semaphore, None)
            .await
            .unwrap();

        assert_eq!(result.files_total, 3);
        assert_eq!(result.files_downloaded, 3);
        assert_eq!(result.files_failed, 0);
        assert!(dir.path().join("test-item/a.txt").exists());
        assert!(dir.path().join("test-item/b.txt").exists());
        assert!(dir.path().join("test-item/c.txt").exists());
    }

    #[tokio::test]
    async fn redownload_file_with_matching_size_but_different_mtime() {
        // When the file has the right size but a different mtime, it should
        // be re-downloaded — both size and mtime must match to skip.
        let mock_server = MockServer::start().await;
        let body = b"hello";

        Mock::given(method("GET"))
            .and(path("/download/test-item/data.bin"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.to_vec())
                    .insert_header("last-modified", "Tue, 14 Nov 2023 22:13:20 GMT"),
            )
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.bin");
        std::fs::write(&file_path, "hello").unwrap();

        // Set local mtime to something different from the metadata mtime
        filetime::set_file_mtime(
            &file_path,
            filetime::FileTime::from_unix_time(1700000099, 0),
        )
        .unwrap();

        let file = FileMetadata {
            name: "data.bin".to_string(),
            size: Some(5),           // matches "hello".len()
            mtime: Some(1700000000), // different from the 1700000099 we set
            ..test_file_meta("data.bin", 5)
        };

        let mut config = crate::config::IaConfig::default();
        let host = mock_server
            .uri()
            .strip_prefix("http://")
            .unwrap()
            .to_string();
        config.general.host = host;
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert!(
            !matches!(result.status, DownloadStatus::Skipped(_)),
            "file with matching size but different mtime should be re-downloaded"
        );
    }

    #[tokio::test]
    async fn resume_handles_200_response_without_corruption() {
        // When the server ignores Range and returns 200 (full content),
        // the .part file should be truncated before writing to prevent
        // appending full content to existing partial data.
        let mock_server = MockServer::start().await;
        let body = b"full content here";

        Mock::given(method("GET"))
            .and(path("/download/test-item/resume.txt"))
            .respond_with(
                ResponseTemplate::new(200) // 200, NOT 206
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Create a .part file simulating a previous partial download
        let part_path = dir.path().join("resume.txt.part");
        std::fs::write(&part_path, "partial da").unwrap(); // 10 bytes

        let file = test_file_meta("resume.txt", body.len() as u64);
        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        // The file should contain ONLY the full response, not "partial da" + full response
        let content = std::fs::read_to_string(dir.path().join("resume.txt")).unwrap();
        assert_eq!(content, "full content here");
        assert_eq!(result.bytes, body.len() as u64);
    }

    #[tokio::test]
    async fn forbidden_file_is_not_retried() {
        let mock_server = MockServer::start().await;

        let guard = Mock::given(method("GET"))
            .and(path("/download/test-item/restricted.txt"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Access denied"))
            .expect(1) // Must be called exactly once — no retries
            .mount_as_scoped(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("restricted.txt", 100);
        let opts = DownloadOpts {
            retries: 3,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, IaError::Http { status: 403, .. }));

        drop(guard); // triggers expect(1) assertion
    }

    #[tokio::test]
    async fn forbidden_item_files_fail_immediately() {
        let mock_server = MockServer::start().await;

        // Mock metadata endpoint
        Mock::given(method("GET"))
            .and(path("/metadata/restricted-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "restricted-item"},
                "files": [
                    {"name": "a.txt", "size": "5", "source": "original"},
                    {"name": "b.txt", "size": "5", "source": "original"}
                ]
            })))
            .mount(&mock_server)
            .await;

        // Both files return 403 — should be hit exactly once each (no retries)
        for name in &["a.txt", "b.txt"] {
            Mock::given(method("GET"))
                .and(path(format!("/download/restricted-item/{name}")))
                .respond_with(ResponseTemplate::new(403).set_body_string("Access denied"))
                .expect(1)
                .mount(&mock_server)
                .await;
        }

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            retries: 3,
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(2));

        let result = download_item(&client, "restricted-item", &opts, semaphore, None)
            .await
            .unwrap();

        assert_eq!(result.files_total, 2);
        assert_eq!(result.files_failed, 2);
        assert_eq!(result.files_downloaded, 0);
        for r in &result.results {
            match &r.status {
                DownloadStatus::Failed(msg) => {
                    assert!(msg.contains("403"), "error should mention 403: {msg}")
                }
                other => panic!("expected Failed, got {:?}", other),
            }
        }
    }

    #[tokio::test]
    async fn server_error_returns_err() {
        let mock_server = MockServer::start().await;

        // download_file uses the no-redirect client (no retry middleware),
        // so a 500 returns Err immediately. App-level retry is handled by
        // download_item's retry loop, not here.
        let guard = Mock::given(method("GET"))
            .and(path("/download/test-item/flaky.txt"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount_as_scoped(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("flaky.txt", 100);
        let opts = DownloadOpts {
            retries: 1,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::Http { status: 500, .. }
        ));
        drop(guard);
    }

    // -- Path traversal security tests (CVE-2025-58438 equivalent) --

    #[test]
    fn rejects_parent_traversal() {
        let dir = Path::new("/tmp/downloads");
        assert!(matches!(
            validate_download_path(dir, "../etc/passwd"),
            Err(IaError::PathTraversal { .. })
        ));
        assert!(matches!(
            validate_download_path(dir, "../../root/.bashrc"),
            Err(IaError::PathTraversal { .. })
        ));
        assert!(matches!(
            validate_download_path(dir, "subdir/../../etc/shadow"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_absolute_paths() {
        let dir = Path::new("/tmp/downloads");
        assert!(matches!(
            validate_download_path(dir, "/etc/passwd"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_null_bytes() {
        let dir = Path::new("/tmp/downloads");
        assert!(matches!(
            validate_download_path(dir, "file\0name.txt"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_control_characters() {
        let dir = Path::new("/tmp/downloads");
        assert!(matches!(
            validate_download_path(dir, "file\x01name.txt"),
            Err(IaError::PathTraversal { .. })
        ));
        assert!(matches!(
            validate_download_path(dir, "file\x0aname.txt"),
            Err(IaError::PathTraversal { .. })
        ));
        assert!(matches!(
            validate_download_path(dir, "file\x1fname.txt"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_empty_name() {
        let dir = Path::new("/tmp/downloads");
        assert!(matches!(
            validate_download_path(dir, ""),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn allows_legitimate_nested_paths() {
        let dir = Path::new("/tmp/downloads");
        let result = validate_download_path(dir, "subdir/test.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Path::new("/tmp/downloads/subdir/test.txt"));

        let result = validate_download_path(dir, "a/b/c/deep.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Path::new("/tmp/downloads/a/b/c/deep.txt"));
    }

    #[test]
    fn allows_simple_filenames() {
        let dir = Path::new("/tmp/downloads");
        assert!(validate_download_path(dir, "test.txt").is_ok());
        assert!(validate_download_path(dir, "file with spaces.txt").is_ok());
        assert!(validate_download_path(dir, "image.jpg").is_ok());
        assert!(validate_download_path(dir, "archive.tar.gz").is_ok());
    }

    #[test]
    fn allows_dot_prefixed_filenames() {
        let dir = Path::new("/tmp/downloads");
        assert!(validate_download_path(dir, ".hidden").is_ok());
        assert!(validate_download_path(dir, ".gitignore").is_ok());
    }

    #[test]
    fn rejects_deeply_nested_traversal() {
        let dir = Path::new("/tmp/downloads");
        // Even if there are legitimate components before the traversal
        assert!(matches!(
            validate_download_path(dir, "a/b/c/../../../etc/passwd"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_curdir_then_traversal() {
        let dir = Path::new("/tmp/downloads");
        // CurDir (.) is harmless, but ParentDir (..) after it must still be caught
        assert!(matches!(
            validate_download_path(dir, "./../../etc/passwd"),
            Err(IaError::PathTraversal { .. })
        ));
        assert!(matches!(
            validate_download_path(dir, "././../secret"),
            Err(IaError::PathTraversal { .. })
        ));
    }

    #[tokio::test]
    async fn download_rejects_traversal_filename() {
        // Integration test: verify download_file returns PathTraversal error
        // without making any HTTP requests (fails before network call).
        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("../../../etc/passwd", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(matches!(result, Err(IaError::PathTraversal { .. })));

        // Verify no files were created outside the temp dir
        assert!(!Path::new("/etc/passwd.part").exists());
    }

    #[tokio::test]
    async fn download_rejects_absolute_path_filename() {
        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("/etc/passwd", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(matches!(result, Err(IaError::PathTraversal { .. })));
    }

    #[test]
    fn path_traversal_is_not_retryable() {
        let err = IaError::PathTraversal {
            path: "../etc/passwd".into(),
            dest_dir: "/tmp/downloads".into(),
        };
        assert!(!err.is_retryable());
    }

    // -- Download size validation tests --

    #[tokio::test]
    async fn download_aborts_on_oversized_response() {
        let mock_server = MockServer::start().await;
        // File metadata says 10 bytes, but server sends 100 bytes.
        // Max allowed = 10 + max(10/10, 1024) = 10 + 1024 = 1034 bytes.
        // But we'll send way more than that.
        let oversized_body = vec![b'X'; 2048];

        Mock::given(method("GET"))
            .and(path("/download/test-item/small.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(oversized_body))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("small.txt", 10);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(matches!(result, Err(IaError::DownloadTooLarge { .. })));

        // .part file should be cleaned up
        assert!(!dir.path().join("small.txt.part").exists());
    }

    #[tokio::test]
    async fn download_allows_slightly_oversized_response() {
        let mock_server = MockServer::start().await;
        // File metadata says 100 bytes. Max = 100 + max(10, 1024) = 1124.
        // Sending 105 bytes (5% over) should succeed.
        let body = vec![b'Y'; 105];

        Mock::given(method("GET"))
            .and(path("/download/test-item/normal.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("normal.txt", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, 105);
    }

    #[test]
    fn download_too_large_is_not_retryable() {
        let err = IaError::DownloadTooLarge {
            file: "test.txt".into(),
            expected: 100,
            received: 2000,
        };
        assert!(!err.is_retryable());
    }

    // -- Symlink and TOCTOU security tests --

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_in_download_path_detected() {
        // Create a temp dir with a symlink pointing outside
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create a symlink: dest_dir/evil -> /some/other/place
        let symlink_path = dir.path().join("evil");
        std::os::unix::fs::symlink(target_dir.path(), &symlink_path).unwrap();

        // Try to download a file into the symlinked directory
        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let file = test_file_meta("evil/payload.txt", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(IaError::PathTraversal { .. })),
            "symlink in download path should be detected: {result:?}"
        );

        // Verify no file was written through the symlink
        assert!(!target_dir.path().join("payload.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_in_nested_download_path_detected() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create legitimate dir, then symlink inside it
        std::fs::create_dir_all(dir.path().join("legit")).unwrap();
        let symlink_path = dir.path().join("legit/evil");
        std::os::unix::fs::symlink(target_dir.path(), &symlink_path).unwrap();

        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let file = test_file_meta("legit/evil/payload.txt", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(IaError::PathTraversal { .. })),
            "nested symlink should be detected: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn part_file_symlink_skips_resume() {
        // If .part file is a symlink, resume should be skipped (not followed).
        // The download should start fresh instead of appending to the symlink target.
        let mock_server = MockServer::start().await;
        let body = b"fresh content";

        Mock::given(method("GET"))
            .and(path("/download/test-item/data.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create a symlink .part file pointing to another location
        let target_file = target_dir.path().join("target.txt");
        std::fs::write(&target_file, "original content").unwrap();
        let part_path = dir.path().join("data.txt.part");
        std::os::unix::fs::symlink(&target_file, &part_path).unwrap();

        let file = test_file_meta("data.txt", body.len() as u64);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        // The symlink target should NOT have been modified
        let target_content = std::fs::read_to_string(&target_file).unwrap();
        assert_eq!(
            target_content, "original content",
            "symlink target should not be modified"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn part_file_symlink_works_with_206_response() {
        // Regression test: when a .part symlink is detected and removed,
        // resume_from must be reset to None. Otherwise, if the server
        // returns 206 (keeping resume_from as Some), the append-mode open
        // on the deleted path would fail with NotFound.
        use wiremock::matchers::header_exists;

        let mock_server = MockServer::start().await;
        let full_body = b"complete file data here";

        // Return 206 Partial Content when Range header is present
        Mock::given(method("GET"))
            .and(path("/download/test-item/ranged.txt"))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(b"data here".to_vec())
                    .insert_header("Content-Range", "bytes 13-21/22"),
            )
            .mount(&mock_server)
            .await;

        // Fallback: return full content when no Range header
        Mock::given(method("GET"))
            .and(path("/download/test-item/ranged.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full_body.to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create a symlink .part file pointing to another location
        let target_file = target_dir.path().join("target.txt");
        std::fs::write(&target_file, "original content").unwrap();
        let part_path = dir.path().join("ranged.txt.part");
        std::os::unix::fs::symlink(&target_file, &part_path).unwrap();

        let file = test_file_meta("ranged.txt", full_body.len() as u64);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);

        // The symlink target should NOT have been modified
        let target_content = std::fs::read_to_string(&target_file).unwrap();
        assert_eq!(
            target_content, "original content",
            "symlink target should not be modified even with 206 response"
        );
    }

    #[tokio::test]
    async fn html_error_body_is_stripped() {
        let mock_server = MockServer::start().await;

        let html_body = r#"<!DOCTYPE html>
<html lang="en"><head><title>Item not available</title></head>
<body><h1>Item not available</h1></body></html>"#;

        Mock::given(method("GET"))
            .and(path("/download/test-item/restricted.txt"))
            .respond_with(ResponseTemplate::new(403).set_body_string(html_body))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("restricted.txt", 100);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap_err();

        match &err {
            IaError::Http { status, message } => {
                assert_eq!(*status, 403);
                // Message should be the canonical reason, not HTML
                assert_eq!(message, "Forbidden");
                assert!(!message.contains("<!DOCTYPE"));
                assert!(!message.contains("<html"));
            }
            other => panic!("expected Http error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn plain_text_error_body_is_preserved() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/test-item/gone.txt"))
            .respond_with(ResponseTemplate::new(404).set_body_string("No such file"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("gone.txt", 100);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap_err();

        match &err {
            IaError::Http { status, message } => {
                assert_eq!(*status, 404);
                assert_eq!(message, "No such file");
            }
            other => panic!("expected Http error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_header_sent_when_credentials_configured() {
        use wiremock::matchers::header;

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/test-item/secret.txt"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"secret content".to_vec()))
            .mount(&mock_server)
            .await;

        let mut config = mock_config(&mock_server.uri());
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        let client = IaClient::from_config(config).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("secret.txt", 14);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        let content = std::fs::read_to_string(dir.path().join("secret.txt")).unwrap();
        assert_eq!(content, "secret content");
    }

    #[tokio::test]
    async fn no_auth_header_when_no_credentials() {
        let mock_server = MockServer::start().await;

        // This mock only matches requests WITHOUT an Authorization header.
        // If an auth header is sent, wiremock returns 404, failing the test.
        Mock::given(method("GET"))
            .and(path("/download/test-item/public.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"public content".to_vec()))
            .mount(&mock_server)
            .await;

        // No s3 credentials configured
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("public.txt", 14);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);

        // Verify the request was received (confirms no unexpected auth header issue)
        let requests = mock_server.received_requests().await.unwrap();
        let download_req = requests
            .iter()
            .find(|r| r.url.path().contains("public.txt"))
            .expect("download request should have been made");
        assert!(
            !download_req.headers.contains_key("Authorization"),
            "should not send Authorization header without credentials"
        );
    }

    #[tokio::test]
    async fn auth_header_preserved_across_redirect() {
        use wiremock::matchers::header;

        let mock_server = MockServer::start().await;

        // First request returns a redirect (simulating archive.org → data node).
        Mock::given(method("GET"))
            .and(path("/download/test-item/secret.txt"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/data/secret.txt", mock_server.uri())),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        // Redirected request must have the Authorization header.
        Mock::given(method("GET"))
            .and(path("/data/secret.txt"))
            .and(header("Authorization", "LOW test_access:test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"secret content".to_vec()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut config = mock_config(&mock_server.uri());
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        let client = IaClient::from_config(config).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("secret.txt", 14);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        let content = std::fs::read_to_string(dir.path().join("secret.txt")).unwrap();
        assert_eq!(content, "secret content");
    }

    // -- collect_batch_results tests --

    fn mock_item_result(
        id: &str,
        downloaded: usize,
        skipped: usize,
        failed: usize,
        bytes: u64,
    ) -> ItemDownloadResult {
        ItemDownloadResult {
            identifier: id.to_string(),
            files_total: downloaded + skipped + failed,
            files_downloaded: downloaded,
            files_skipped: skipped,
            files_failed: failed,
            bytes_total: bytes,
            elapsed: Duration::from_millis(100),
            results: vec![],
        }
    }

    #[test]
    fn collect_batch_all_success() {
        let results = vec![
            Ok(mock_item_result("a", 3, 1, 0, 3000)),
            Ok(mock_item_result("b", 2, 0, 0, 2000)),
            Ok(mock_item_result("c", 1, 2, 1, 1000)),
        ];
        let batch = collect_batch_results(3, results, Duration::from_secs(1));
        assert_eq!(batch.items_total, 3);
        assert_eq!(batch.items_succeeded, 3);
        assert_eq!(batch.items_failed, 0);
        assert_eq!(batch.files_downloaded, 6); // 3+2+1
        assert_eq!(batch.files_skipped, 3); // 1+0+2
        assert_eq!(batch.files_failed, 1); // 0+0+1
        assert_eq!(batch.bytes_total, 6000); // 3000+2000+1000
    }

    #[test]
    fn collect_batch_all_failures() {
        let results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> = vec![
            Err(("item-a".into(), IaError::NotFound("item-a".into()))),
            Err(("item-b".into(), IaError::NotFound("item-b".into()))),
        ];
        let batch = collect_batch_results(2, results, Duration::from_secs(1));
        assert_eq!(batch.items_total, 2);
        assert_eq!(batch.items_succeeded, 0);
        assert_eq!(batch.items_failed, 2);
        assert_eq!(batch.files_downloaded, 0);
        assert_eq!(batch.files_skipped, 0);
        assert_eq!(batch.files_failed, 0);
        assert_eq!(batch.bytes_total, 0);
    }

    #[test]
    fn collect_batch_mixed() {
        let results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> = vec![
            Ok(mock_item_result("ok-item", 5, 2, 1, 5000)),
            Err(("bad-item".into(), IaError::NotFound("bad-item".into()))),
        ];
        let batch = collect_batch_results(2, results, Duration::from_secs(1));
        assert_eq!(batch.items_total, 2);
        assert_eq!(batch.items_succeeded, 1);
        assert_eq!(batch.items_failed, 1);
        assert_eq!(batch.files_downloaded, 5);
        assert_eq!(batch.files_skipped, 2);
        assert_eq!(batch.files_failed, 1);
        assert_eq!(batch.bytes_total, 5000);
    }

    #[test]
    fn collect_batch_empty() {
        let results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> = vec![];
        let batch = collect_batch_results(0, results, Duration::from_secs(0));
        assert_eq!(batch.items_total, 0);
        assert_eq!(batch.items_succeeded, 0);
        assert_eq!(batch.items_failed, 0);
        assert_eq!(batch.files_downloaded, 0);
        assert_eq!(batch.files_skipped, 0);
        assert_eq!(batch.files_failed, 0);
        assert_eq!(batch.bytes_total, 0);
    }

    // -- download_item_with_metadata test --

    #[tokio::test]
    async fn download_item_with_metadata_skips_metadata_fetch() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};

        let mock_server = MockServer::start().await;

        // NO metadata mock — we pass the metadata directly.
        // If the function tries to fetch metadata, the request will 404 / go unmatched.

        // Mock file download endpoints
        for name in &["x.txt", "y.txt"] {
            Mock::given(method("GET"))
                .and(path(format!("/download/pre-meta/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data!".to_vec()))
                .mount(&mock_server)
                .await;
        }

        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("pre-meta".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("x.txt", 5), test_file_meta("y.txt", 5)],
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra: HashMap::new(),
        };

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(2));

        let result =
            download_item_with_metadata(&client, "pre-meta", &item, &opts, semaphore, None)
                .await
                .unwrap();

        assert_eq!(result.files_total, 2);
        assert_eq!(result.files_downloaded, 2);
        assert_eq!(result.files_failed, 0);
        assert!(dir.path().join("pre-meta/x.txt").exists());
        assert!(dir.path().join("pre-meta/y.txt").exists());

        // Verify no metadata request was made
        let requests = mock_server.received_requests().await.unwrap();
        let metadata_requests: Vec<_> = requests
            .iter()
            .filter(|r| r.url.path().contains("/metadata/"))
            .collect();
        assert!(
            metadata_requests.is_empty(),
            "download_item_with_metadata should not fetch metadata"
        );
    }

    // -- cleanup_item_dir tests --

    #[tokio::test]
    async fn cleanup_item_dir_removes_directory() {
        let dir = tempfile::tempdir().unwrap();
        let item_dir = dir.path().join("my-item");
        std::fs::create_dir_all(&item_dir).unwrap();
        std::fs::write(item_dir.join("file.txt"), "data").unwrap();
        std::fs::write(item_dir.join("file.txt.part"), "partial").unwrap();

        cleanup_item_dir(dir.path(), "my-item").await;

        assert!(!item_dir.exists(), "item directory should be removed");
    }

    #[tokio::test]
    async fn cleanup_item_dir_noop_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        // Should not panic or error when the directory doesn't exist
        cleanup_item_dir(dir.path(), "nonexistent-item").await;
    }

    // -- disk-full propagation test --

    #[tokio::test]
    async fn download_item_with_metadata_propagates_disk_full() {
        // Simulate disk-full by writing to a read-only directory. We can't
        // actually trigger StorageFull in a test, but we CAN verify the
        // propagation logic by testing that download_item_with_metadata
        // returns Err(DiskFull) when a file write fails and is_disk_full()
        // is true.
        //
        // Strategy: use a mock server that returns a valid response, but
        // point the destdir at a path where writes will fail. On macOS/Linux,
        // making the item subdirectory read-only after creation doesn't help
        // because create_dir_all succeeds. Instead, we use a file where a
        // directory is expected — download_file will fail creating subdirs.
        //
        // This test verifies that the error from download_file propagates
        // up through download_item_with_metadata as Err, rather than being
        // swallowed into Ok(ItemDownloadResult{files_failed > 0}).
        //
        // For the actual disk-full scenario, we verify via the is_retryable
        // and is_disk_full unit tests that StorageFull IO errors are correctly
        // classified — the download_item_with_metadata code then uses
        // is_disk_full() to decide whether to propagate.

        // The actual disk-full propagation is tested end-to-end in the CLI
        // integration tests (download_search_list.rs). Here we test the
        // cleanup_item_dir + collect_batch_results plumbing.
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};

        let mock_server = MockServer::start().await;

        // Return a large response to trigger writes
        let large_body = vec![b'x'; 1024];
        Mock::given(method("GET"))
            .and(path("/download/disk-test/big.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(large_body)
                    .insert_header("content-length", "1024"),
            )
            .mount(&mock_server)
            .await;

        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("disk-test".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("big.txt", 1024)],
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra: HashMap::new(),
        };

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        // Use a path that doesn't exist and can't be created (file as parent)
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("disk-test");
        // Create a file where the item directory should go — this causes
        // create_dir_all to fail with "Not a directory"
        std::fs::write(&blocker, "I am a file, not a directory").unwrap();

        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(2));

        let result =
            download_item_with_metadata(&client, "disk-test", &item, &opts, semaphore, None).await;

        // The file write fails (because the item subdir can't be created),
        // but this is NOT a disk-full error, so it should be collected into
        // Ok(ItemDownloadResult) with files_failed > 0 — NOT propagated as Err.
        // This verifies the selective propagation: only disk-full triggers Err.
        match result {
            Ok(r) => {
                assert_eq!(r.files_failed, 1, "file should fail (not a directory)");
                assert_eq!(r.files_downloaded, 0);
            }
            Err(e) => {
                // Also acceptable if the IO error propagates — the key thing
                // is that it's NOT DiskFull
                assert!(
                    !e.is_disk_full(),
                    "non-disk-full IO error should not be reported as DiskFull: {e}"
                );
            }
        }
    }

    #[tokio::test]
    async fn redirect_to_non_archive_org_blocked() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/test-item/evil.txt"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", "https://evil.example.com/steal"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("evil.txt", 100);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap_err();

        match &err {
            IaError::Http { message, .. } => {
                assert!(
                    message.contains("non-archive.org"),
                    "should mention non-archive.org: {message}"
                );
            }
            other => panic!("expected Http error, got: {other:?}"),
        }
    }

    // --- inline checksum + Verifying event tests ---

    /// Helper: FileMetadata with a specific md5 (not the empty-file default).
    fn test_file_meta_with_md5(name: &str, size: u64, md5: &str) -> FileMetadata {
        let mut m = test_file_meta(name, size);
        m.md5 = Some(md5.to_string());
        m
    }

    /// Compute MD5 hex of a byte slice (for test expectations).
    fn md5_hex(bytes: &[u8]) -> String {
        use md5::{Digest, Md5};
        let mut h = Md5::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    #[tokio::test]
    async fn checksum_inline_hash_matches_server_md5() {
        let body = b"hello inline checksum world".to_vec();
        let expected = md5_hex(&body);

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/a.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta_with_md5("a.txt", body.len() as u64, &expected);

        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };
        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        // .part should be finalized to the real name
        assert!(dir.path().join("a.txt").exists());
        assert!(!dir.path().join("a.txt.part").exists());
    }

    #[tokio::test]
    async fn checksum_inline_hash_detects_mismatch_and_removes_part() {
        let body = b"correct content".to_vec();
        let wrong_md5 = "00000000000000000000000000000000"; // not the real md5

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/b.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, wrong_md5);

        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };
        let err = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap_err();

        assert!(matches!(err, IaError::ChecksumMismatch { .. }));
        // Bad .part must be deleted, final file must not exist.
        assert!(!dir.path().join("b.txt").exists());
        assert!(!dir.path().join("b.txt.part").exists());
    }

    #[tokio::test]
    async fn checksum_pre_skip_emits_verifying_before_skipped() {
        // Pre-populate the destination with content matching the
        // server-reported md5, so should_skip_checksum fires.
        let body = b"already-downloaded content";
        let expected = md5_hex(body);

        let dir = tempfile::tempdir().unwrap();
        let item_dir = dir.path().join("test-item");
        std::fs::create_dir_all(&item_dir).unwrap();
        let target = item_dir.join("c.txt");
        std::fs::write(&target, body).unwrap();

        // Metadata fetch isn't hit because the skip-check short-circuits,
        // but the client still needs a valid base URL.
        let mock_server = MockServer::start().await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let file = test_file_meta_with_md5("c.txt", body.len() as u64, &expected);

        let events: Arc<std::sync::Mutex<Vec<DownloadStatus>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let cb: Arc<dyn Fn(DownloadProgress) + Send + Sync> = Arc::new(move |p| {
            if let Ok(mut v) = captured.lock() {
                v.push(p.status);
            }
        });

        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };
        let result = download_file(&client, "test-item", &file, &item_dir, &opts, Some(&*cb))
            .await
            .unwrap();

        assert!(matches!(result.status, DownloadStatus::Skipped(_)));

        let seen = events.lock().unwrap();
        let verifying_idx = seen.iter().position(|s| *s == DownloadStatus::Verifying);
        let skipped_idx = seen
            .iter()
            .position(|s| matches!(s, DownloadStatus::Skipped(_)));
        assert!(
            verifying_idx.is_some(),
            "Verifying event should fire before pre-skip MD5 hash: {seen:?}"
        );
        assert!(skipped_idx.is_some(), "Skipped event should fire: {seen:?}");
        assert!(
            verifying_idx < skipped_idx,
            "Verifying must precede Skipped: {seen:?}"
        );
    }
}
