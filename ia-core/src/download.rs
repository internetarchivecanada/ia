use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures::{stream, StreamExt};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::files::FileFilter;
use crate::types::FileMetadata;

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
    let file_path = dest_dir.join(&file.name);

    // Ensure parent directory exists
    if let Some(parent) = file_path.parent() {
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

    // Checksum skip: compute local MD5 and compare
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

    // Check for partial file (.part) for resume
    let part_path = PathBuf::from(format!("{}.part", file_path.display()));
    let resume_from = if part_path.exists() {
        let meta = fs::metadata(&part_path).await?;
        Some(meta.len())
    } else {
        None
    };

    // Build request
    let mut req = client.http().get(&url);
    if let Some(offset) = resume_from {
        debug!(file = %file.name, offset, "resuming download");
        req = req.header("Range", format!("bytes={offset}-"));
    }

    if let Some(p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: file.name.clone(),
            bytes_downloaded: resume_from.unwrap_or(0),
            total_bytes: file.size,
            status: DownloadStatus::Starting,
        });
    }

    let response = req.send().await?;
    let status = response.status();

    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
        });
    }

    // Parse Last-Modified for mtime
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok());

    // Stream to .part file
    let mut output = if resume_from.is_some() {
        fs::OpenOptions::new()
            .append(true)
            .open(&part_path)
            .await?
    } else {
        fs::File::create(&part_path).await?
    };

    let mut bytes_downloaded = resume_from.unwrap_or(0);
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(reqwest_middleware::Error::from)?;
        output.write_all(&chunk).await?;
        bytes_downloaded += chunk.len() as u64;

        if let Some(p) = progress {
            p(DownloadProgress {
                identifier: identifier.to_string(),
                file_name: file.name.clone(),
                bytes_downloaded,
                total_bytes: file.size,
                status: DownloadStatus::Downloading,
            });
        }
    }

    output.flush().await?;
    drop(output);

    // Checksum verification if requested
    if opts.checksum {
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

            let actual_md5 = compute_md5(&part_path).await?;
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
        if let Some(mtime) = last_modified.or_else(|| {
            file.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t))
        }) {
            let _ = filetime::set_file_mtime(
                &file_path,
                filetime::FileTime::from_system_time(mtime),
            );
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
    if let Some(remote_size) = file.size {
        if local_size != remote_size {
            return None;
        }
    } else {
        return None; // Can't compare without remote size
    }

    // mtime must match (if available)
    if let Some(remote_mtime) = file.mtime {
        if let Ok(local_mtime) = meta.modified() {
            let local_ts = local_mtime
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if local_ts == remote_mtime {
                return Some("size+mtime match".to_string());
            }
        }
    }

    // If we have matching size but no mtime to compare, skip based on size alone
    if file.mtime.is_none() {
        return Some("size match (no remote mtime)".to_string());
    }

    None
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
    use md5::{Md5, Digest};
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer

    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 { break; }
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
    let start = std::time::Instant::now();

    // Fetch item metadata
    let item = crate::metadata::get(client, identifier).await?;

    // Filter files
    let files = crate::files::list(&item, &opts.filter);
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

    // Concurrent download with shared semaphore
    let mut handles = Vec::new();

    for file in files_owned {
        let client = client.clone();
        let identifier = identifier.to_string();
        let dest_dir = dest_dir.clone();
        let opts = opts.clone();
        let sem = Arc::clone(&semaphore);
        let progress = progress.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let prog_ref = progress.as_deref();
            let mut last_err = None;

            for attempt in 0..=opts.retries {
                if attempt > 0 {
                    let delay = Duration::from_secs(2u64.pow(attempt as u32).min(60));
                    warn!(file = %file.name, attempt, "retrying after {:?}", delay);
                    tokio::time::sleep(delay).await;
                }

                match download_file(
                    &client,
                    &identifier,
                    &file,
                    &dest_dir,
                    &opts,
                    prog_ref,
                )
                .await
                {
                    Ok(result) => return result,
                    Err(e) => {
                        warn!(file = %file.name, attempt, error = %e, "download failed");
                        last_err = Some(e);
                    }
                }
            }

            FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Failed(
                    last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".to_string()),
                ),
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

    let files_downloaded = results.iter().filter(|r| r.status == DownloadStatus::Complete).count();
    let files_skipped = results.iter().filter(|r| matches!(r.status, DownloadStatus::Skipped(_))).count();
    let files_failed = results.iter().filter(|r| matches!(r.status, DownloadStatus::Failed(_))).count();
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

/// Callback invoked when an item starts downloading: (identifier, index, total).
pub type OnItemStartFn = Arc<dyn Fn(&str, usize, usize) + Send + Sync>;

/// Callback invoked when an item finishes downloading.
pub type OnItemCompleteFn = Arc<dyn Fn(&ItemDownloadResult) + Send + Sync>;

/// Download multiple items with controlled concurrency.
///
/// `items_concurrency` limits how many items download simultaneously.
/// The `semaphore` separately limits total concurrent file transfers across all active items.
#[allow(clippy::too_many_arguments)]
pub async fn download_batch(
    client: &IaClient,
    identifiers: Vec<String>,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
    on_item_start: Option<OnItemStartFn>,
    on_item_complete: Option<OnItemCompleteFn>,
    items_concurrency: usize,
) -> BatchDownloadResult {
    let start = std::time::Instant::now();
    let items_total = identifiers.len();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>> =
        stream::iter(identifiers)
            .map(|identifier| {
                let client = client.clone();
                let opts = opts.clone();
                let semaphore = Arc::clone(&semaphore);
                let progress = progress.clone();
                let counter = Arc::clone(&counter);
                let on_item_start = on_item_start.clone();
                let on_item_complete = on_item_complete.clone();

                async move {
                    let idx =
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if let Some(ref cb) = on_item_start {
                        cb(&identifier, idx, items_total);
                    }
                    info!(item = %identifier, idx, "starting item download");

                    match download_item(&client, &identifier, &opts, semaphore, progress)
                        .await
                    {
                        Ok(result) => {
                            if let Some(ref cb) = on_item_complete {
                                cb(&result);
                            }
                            Ok(result)
                        }
                        Err(e) => {
                            warn!(identifier = %identifier, error = %e, "item download failed");
                            Err((identifier, e))
                        }
                    }
                }
            })
            .buffer_unordered(items_concurrency)
            .collect()
            .await;

    collect_batch_results(items_total, item_results, start.elapsed())
}

fn collect_batch_results(
    items_total: usize,
    item_results: Vec<std::result::Result<ItemDownloadResult, (String, IaError)>>,
    elapsed: Duration,
) -> BatchDownloadResult {
    let items_succeeded = item_results.iter().filter(|r| r.is_ok()).count();
    let items_failed = item_results.iter().filter(|r| r.is_err()).count();
    let files_downloaded: usize = item_results.iter().filter_map(|r| r.as_ref().ok()).map(|r| r.files_downloaded).sum();
    let files_skipped: usize = item_results.iter().filter_map(|r| r.as_ref().ok()).map(|r| r.files_skipped).sum();
    let files_failed: usize = item_results.iter().filter_map(|r| r.as_ref().ok()).map(|r| r.files_failed).sum();
    let bytes_total: u64 = item_results.iter().filter_map(|r| r.as_ref().ok()).map(|r| r.bytes_total).sum();

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
            sha1: None, crc32: None, original: None, rotation: None,
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

        let file = FileMetadata {
            name: "existing.txt".to_string(),
            size: Some(7), // "content".len()
            mtime: None,   // No mtime means skip on size alone
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
            &client, "test-item", &file, dir.path(),
            &DownloadOpts::default(), None,
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

        let result = download_item(&client, "test-item", &opts, semaphore, None).await.unwrap();

        assert_eq!(result.files_total, 3);
        assert_eq!(result.files_downloaded, 3);
        assert_eq!(result.files_failed, 0);
        assert!(dir.path().join("test-item/a.txt").exists());
        assert!(dir.path().join("test-item/b.txt").exists());
        assert!(dir.path().join("test-item/c.txt").exists());
    }
}
