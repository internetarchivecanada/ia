mod stall;
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
    /// Increment archive.org's public view counter on each fetch.
    ///
    /// `false` (default) sends `cnt=0` with every download request so the
    /// public view counter is not incremented. Set to `true` to omit the
    /// `cnt` parameter entirely — archive.org only counts a view when the
    /// `cnt` query parameter is absent (any value, including `cnt=1`,
    /// suppresses counting).
    pub count_views: bool,
    /// Minimum throughput in bytes per second before a stream is judged
    /// stalled; `0` disables the check.
    ///
    /// Once a stream is 30 s old, its average over the last 60 s, or over
    /// its whole life while younger than that, is compared with this floor
    /// once a second. Below it, the
    /// stream is abandoned and the file re-requested with `Range` from the
    /// bytes already on disk, exactly as a body-stream error is handled.
    /// Stalls have their own budget, capped at [`retries`](Self::retries);
    /// when it is spent the file fails with [`IaError::DownloadStalled`].
    /// The default is 10 KiB/s.
    pub min_speed: u64,
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
            count_views: false,
            min_speed: 10 * 1024,
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

/// Ensure a `/download/` URL carries the `cnt=0` query parameter so archive.org
/// does not increment the public view counter for the requested file.
///
/// Appends `?cnt=0` (or `&cnt=0` if the URL already has a `?` separator) when
/// `cnt=` is not present. The IA-specific `…/file.jp2&ext=jpg` quirk (an
/// `&ext=` segment without a preceding `?`) is preserved as part of the path —
/// in that case we still append `?cnt=0`.
///
/// If the URL already includes any `cnt=` value it is preserved as-is, since
/// archive.org's view-counter is suppressed by the *presence* of the `cnt`
/// parameter — to re-enable view counting the parameter must be omitted
/// entirely.
pub(crate) fn ensure_cnt_zero(url: &str) -> String {
    if url.contains("?cnt=") || url.contains("&cnt=") {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}cnt=0")
}

/// Extract the complete length from a `Content-Range` header value.
///
/// Accepts the two forms RFC 9110 allows for the `bytes` unit,
/// `bytes <first>-<last>/<complete-length>` and `bytes */<complete-length>`,
/// and returns `None` when the complete length is `*` (the server does not
/// know it), when the unit is not `bytes`, or when the value does not parse.
///
/// ```text
/// "bytes 13-21/22"  -> Some(22)
/// "bytes */22"      -> Some(22)
/// "bytes 13-21/*"   -> None
/// ```
#[must_use]
fn parse_content_range_total(value: &str) -> Option<u64> {
    let (unit, range_and_total) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (_range, total) = range_and_total.trim().rsplit_once('/')?;
    total.trim().parse().ok()
}

/// Whether a file's metadata `size` cannot be trusted by construction.
///
/// `{identifier}_files.xml` lists every file in the item, including
/// itself, so its own size is recorded before the final bytes exist and
/// never matches what the server sends. Nothing else is exempt.
#[must_use]
fn is_size_unknowable(identifier: &str, file_name: &str) -> bool {
    file_name.strip_prefix(identifier) == Some("_files.xml")
}

/// Fail before writing anything when a 206's `Content-Range` total disagrees
/// with the size in the item's metadata.
///
/// The server and the metadata describe the same file; if they disagree
/// about its length, retrying will not reconcile them, so this is the
/// permanent [`IaError::ServerSizeMismatch`]. Only 206 responses are
/// checked here; a 416's `Content-Range` is handled by
/// [`range_not_satisfiable`]. A total of `*`, a missing metadata
/// size, and `{identifier}_files.xml` (see [`is_size_unknowable`]) are all
/// skipped.
fn check_content_range(
    response: &reqwest::Response,
    identifier: &str,
    file: &FileMetadata,
) -> Result<()> {
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Ok(());
    }
    let Some(metadata_size) = file.size else {
        return Ok(());
    };
    if is_size_unknowable(identifier, &file.name) {
        return Ok(());
    }
    let server_size = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_total);
    match server_size {
        Some(server_size) if server_size != metadata_size => Err(IaError::ServerSizeMismatch {
            file: file.name.clone(),
            metadata_size,
            server_size,
        }),
        _ => Ok(()),
    }
}

/// Fetch a response from archive.org with auth, manual redirect following, and SSRF guard.
///
/// Handles:
/// - LOW auth headers from S3 credentials
/// - Manual redirect following with auth preservation (reqwest strips Authorization on redirect)
/// - SSRF guard: only follows redirects to *.archive.org or the configured host
/// - HTML error page stripping
/// - Optional resume via Range header. A 416 answering that header is
///   returned as a response, not an error, so the caller can read its
///   `Content-Range` (see [`range_not_satisfiable`]); without a Range
///   header a 416 is an [`IaError::Http`] like any other failure status
/// - View-counter suppression via `cnt=0` (see `count_views`)
///
/// When `count_views` is `false` (the default for every internal caller) the
/// URL is rewritten through [`ensure_cnt_zero`] to suppress archive.org's
/// public view counter. Set `count_views = true` to leave the URL untouched —
/// archive.org only counts a view when the `cnt` parameter is absent
/// entirely.
///
/// Returns the raw response for callers to consume (stream to disk or collect to bytes).
pub(crate) async fn fetch_response(
    client: &IaClient,
    url: &str,
    resume_from: Option<u64>,
    count_views: bool,
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
    //
    // Inject `cnt=0` on the initial URL (unless the caller opted in to view
    // counting) so archive.org does not count the request against the
    // public view counter. Redirected URLs from data nodes are not modified
    // (the view counter lives on archive.org).
    let max_redirects = 10;
    let mut current_url = if count_views {
        url.to_string()
    } else {
        ensure_cnt_zero(url)
    };
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
                    retry_after: None,
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
                        retry_after: None,
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
        retry_after: None,
    })?;

    let status = response.status();
    // A 416 answers the Range header we sent: the resume offset is at or
    // past the end of the server's copy. Its Content-Range names the
    // server's length, which `download_file` needs, so it is returned to
    // the caller instead of being collapsed into `IaError::Http` here.
    // Callers that send no Range keep seeing a 416 as the plain HTTP error.
    let range_not_satisfiable =
        resume_from.is_some() && status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE;
    if !status.is_success()
        && status != reqwest::StatusCode::PARTIAL_CONTENT
        && !range_not_satisfiable
    {
        return Err(http_error_from(response).await);
    }

    Ok(response)
}

/// Turn a non-success response into [`IaError::Http`], consuming the body
/// for the message.
///
/// IA often returns full HTML error pages (e.g. "Item not available"); those
/// are replaced by the status's canonical reason phrase.
async fn http_error_from(response: reqwest::Response) -> IaError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = if body.contains("<!DOCTYPE") || body.contains("<html") {
        status
            .canonical_reason()
            .unwrap_or("unknown error")
            .to_string()
    } else {
        body
    };
    IaError::Http {
        status: status.as_u16(),
        message,
        retry_after: None,
    }
}

/// Decide what a 416 on a resume `Range` request means for the `.part`.
///
/// The caller has already established that `response` is a 416 and that it
/// answered a request for `bytes={offset}-`, where `offset` is the length of
/// the `.part` file at `part_path`. The server is saying that offset is at
/// or past the end of its copy of the file, and its
/// `Content-Range: bytes */total` names that copy's length. Nothing more can
/// be streamed from this `.part`, so:
///
/// - `total` differs from the metadata size: [`IaError::ServerSizeMismatch`].
///   The two disagree and retrying cannot reconcile them. The `.part` is
///   left alone, as [`check_content_range`] leaves it on a 206.
/// - `total`, the metadata size, and `offset` all agree: the `.part` is the
///   whole file. `Ok(())` is returned and the caller finishes it in place
///   (md5 compare when `--checksum` is on, then the rename) instead of
///   downloading it again.
/// - `total` equals the metadata size but the `.part` is longer, or the size
///   is unknown or exempt (see [`is_size_unknowable`]) so there is no third
///   party to agree: the `.part` is removed and the retryable
///   [`IaError::DownloadSizeMismatch`] is returned so the next attempt starts
///   from byte 0.
/// - no parseable total: the plain [`IaError::Http`] a 416 always was.
///
/// The caller must close any writer on `part_path` before calling this.
async fn range_not_satisfiable(
    response: reqwest::Response,
    identifier: &str,
    file: &FileMetadata,
    offset: u64,
    part_path: &Path,
) -> Result<()> {
    debug_assert_eq!(
        response.status(),
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE
    );
    let server_size = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_total);
    let Some(server_size) = server_size else {
        return Err(http_error_from(response).await);
    };
    let metadata_size = file
        .size
        .filter(|_| !is_size_unknowable(identifier, &file.name));
    match metadata_size {
        Some(metadata_size) if metadata_size != server_size => {
            warn!(
                file = %file.name,
                metadata_size,
                server_size,
                offset,
                "416 on resume: server length differs from item metadata; keeping .part"
            );
            Err(IaError::ServerSizeMismatch {
                file: file.name.clone(),
                metadata_size,
                server_size,
            })
        }
        Some(metadata_size) if metadata_size == offset => {
            info!(
                file = %file.name,
                size = offset,
                "416 on resume: .part already holds the whole file; finishing it in place"
            );
            Ok(())
        }
        _ => {
            warn!(
                file = %file.name,
                server_size,
                offset,
                "416 on resume: .part is already at or past the file's length; deleting it"
            );
            let _ = fs::remove_file(part_path).await;
            Err(IaError::DownloadSizeMismatch {
                file: file.name.clone(),
                expected: server_size,
                received: offset,
            })
        }
    }
}

/// The `Last-Modified` header of `response` as a timestamp, if it has one
/// that parses.
fn last_modified_of(response: &reqwest::Response) -> Option<std::time::SystemTime> {
    response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
}

/// Feed the first `resumed_bytes` of the `.part` at `part_path` into a fresh
/// md5 state, so a hash that continues over the rest of the stream (or over
/// nothing, when the `.part` is already the whole file) covers exactly the
/// bytes the caller counts. Anything past `resumed_bytes` is not read, so
/// the hash matches the length the caller reports even if the file grew
/// after it was measured.
///
/// Reading the `.part` can take tens of seconds on a 10 GB partial, so a
/// `Verifying` progress event goes out first and again every 16 MiB.
async fn seed_hasher_from_part(
    identifier: &str,
    file: &FileMetadata,
    part_path: &Path,
    resumed_bytes: u64,
    progress: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
) -> Result<md5::Md5> {
    use md5::{Digest, Md5};
    use tokio::io::AsyncReadExt;

    let mut h = Md5::new();
    if let Some(p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: file.name.clone(),
            bytes_downloaded: 0,
            total_bytes: Some(resumed_bytes),
            status: DownloadStatus::Verifying,
        });
    }
    let mut seed = fs::File::open(part_path).await?.take(resumed_bytes);
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
    Ok(h)
}

/// Where a download that failed its md5 check is kept: beside the file, as
/// `<name>.md5-mismatch`. Nothing resumes from or skips on this name. An
/// item file literally named `<name>.md5-mismatch` would share the path,
/// as one named `<name>.part` shares the partial file's.
fn mismatch_path(file_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.md5-mismatch", file_path.display()))
}

/// Turn a `.part` whose byte count already matches the item metadata into
/// the finished file: compare the md5 when `hasher` is present (keeping the
/// bytes as `<name>.md5-mismatch` on a mismatch, see [`mismatch_path`]),
/// rename it into place, remove an earlier `.md5-mismatch` once the file is
/// verified, set its mtime, and report completion.
///
/// Shared by the normal end of a stream and by the 416 shortcut in
/// [`range_not_satisfiable`], so both finish a file the same way.
#[allow(clippy::too_many_arguments)]
async fn finish_part(
    identifier: &str,
    file: &FileMetadata,
    file_path: &Path,
    part_path: &Path,
    bytes_downloaded: u64,
    hasher: Option<md5::Md5>,
    last_modified: Option<std::time::SystemTime>,
    opts: &DownloadOpts,
    progress: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
    start: std::time::Instant,
) -> Result<FileDownloadResult> {
    // Post-download checksum comparison using the inline-computed hash.
    let hasher_matched = hasher.is_some() && file.md5.is_some();
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
                // Keep the bytes under a name nothing resumes from, so the
                // copy can be compared against the source or a second
                // download. The rename replaces an earlier bad copy, or a
                // planted symlink at that path, without following it. If
                // it cannot (a directory in the way, say), the .part must
                // still go: left in place, every later run would re-hash
                // the same bytes and fail the same way.
                let kept_path = mismatch_path(file_path);
                let kept = match fs::rename(part_path, &kept_path).await {
                    Ok(()) => {
                        warn!(
                            file = %file.name,
                            expected = %expected_md5,
                            actual = %actual_md5,
                            kept = %kept_path.display(),
                            "md5 mismatch; keeping the download for inspection"
                        );
                        Some(kept_path.display().to_string())
                    }
                    Err(e) => {
                        warn!(
                            file = %file.name,
                            expected = %expected_md5,
                            actual = %actual_md5,
                            kept = %kept_path.display(),
                            error = %e,
                            "md5 mismatch; could not keep the download, removing it"
                        );
                        let _ = fs::remove_file(part_path).await;
                        None
                    }
                };
                return Err(IaError::ChecksumMismatch {
                    file: file.name.clone(),
                    expected: expected_md5.clone(),
                    actual: actual_md5,
                    kept,
                });
            }
        }
    }

    // Finalize: rename .part to final name
    fs::rename(part_path, file_path).await?;

    // Verified and in place. An earlier bad copy has told its story, so it
    // goes; if it cannot be removed, that is a stale file to mention, not a
    // failure of a download that already succeeded.
    if hasher_matched {
        let kept_path = mismatch_path(file_path);
        if fs::symlink_metadata(&kept_path).await.is_ok() {
            match fs::remove_file(&kept_path).await {
                Ok(()) => info!(
                    file = %file.name,
                    kept = %kept_path.display(),
                    "removed the earlier md5-mismatch copy"
                ),
                Err(e) => warn!(
                    file = %file.name,
                    kept = %kept_path.display(),
                    error = %e,
                    "could not remove the earlier md5-mismatch copy"
                ),
            }
        }
    }

    // Set mtime from Last-Modified header
    if !opts.no_timestamps {
        if let Some(mtime) =
            last_modified.or_else(|| file.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t)))
        {
            let _ =
                filetime::set_file_mtime(file_path, filetime::FileTime::from_system_time(mtime));
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
    //
    // A symlink .part goes first (#25). File::open follows links, so a
    // planted link would otherwise lend its target's length to the Range
    // request; the 206 tail would then land at offset 0 of a fresh .part,
    // and a later run could resume that tail into a wrong file of the
    // right length. Remove the link now, so no Range is ever shaped by it.
    let part_path = PathBuf::from(format!("{}.part", file_path.display()));
    if let Ok(meta) = fs::symlink_metadata(&part_path).await {
        if meta.file_type().is_symlink() {
            warn!(file = %file.name, "removing symlink .part file before resume");
            fs::remove_file(&part_path).await?;
        }
    }
    // Open first, then stat the fd, so the length is of the file actually
    // opened and a directory or other non-regular file is never resumed.
    // (The fd metadata cannot tell a link planted after the check above;
    // the two later symlink checks keep every write off such a link.)
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

    let response = fetch_response(client, &url, resume_from, opts.count_views).await?;
    // A 416 only comes back when a Range header was sent, so the resume
    // offset is always present alongside it.
    if let (Some(offset), reqwest::StatusCode::RANGE_NOT_SATISFIABLE) =
        (resume_from, response.status())
    {
        let last_modified = last_modified_of(&response);
        range_not_satisfiable(response, identifier, file, offset, &part_path).await?;
        // The .part is the whole file, but never finish it through a
        // symlink: renaming the link into place would leave a Complete
        // download pointing outside dest_dir and the mtime write would go
        // through it. The check before the resume offset was read removes
        // a link that was already there; this guards against one planted
        // since. Remove it and restart from byte 0, as the symlink check
        // on the streaming path does.
        if let Ok(meta) = fs::symlink_metadata(&part_path).await {
            if meta.file_type().is_symlink() {
                warn!(file = %file.name, "removing symlink .part file");
                fs::remove_file(&part_path).await?;
                return Err(IaError::ResumeFailed {
                    file: file.name.clone(),
                    reason: ".part path is a symlink".to_string(),
                });
            }
        }
        // Hash the .part from disk when asked to, then finish it without
        // streaming anything.
        let hasher = if opts.checksum && file.md5.is_some() {
            Some(seed_hasher_from_part(identifier, file, &part_path, offset, progress).await?)
        } else {
            None
        };
        return finish_part(
            identifier,
            file,
            &file_path,
            &part_path,
            offset,
            hasher,
            last_modified,
            opts,
            progress,
            start,
        )
        .await;
    }
    check_content_range(&response, identifier, file)?;
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
    let last_modified = last_modified_of(&response);

    // If .part is a symlink, remove it before writing. The check before
    // the resume offset was read handles a link that was already there;
    // this one guards against a link planted since, so no write ever goes
    // through a symlink.
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
        Some(match resume_from {
            // Seeding reads the full .part before the HTTP stream opens.
            Some(resumed_bytes) => {
                seed_hasher_from_part(identifier, file, &part_path, resumed_bytes, progress).await?
            }
            None => {
                use md5::Digest;
                md5::Md5::new()
            }
        })
    } else {
        None
    };

    let mut bytes_downloaded = resume_from.unwrap_or(0);
    let mut last_progress_at = bytes_downloaded;

    // Stream-level retry budget. `reqwest-retry` only sees the initial
    // response status; once we start reading the body it's on us.
    // Real-world incident (us-supreme-court, 2026-04-22): 39 body-stream
    // decode errors bypassed the middleware entirely, dropping items the
    // server would have happily served a few seconds later.
    const MAX_STREAM_RETRIES: usize = 3;
    let mut stream_attempt: usize = 0;
    // Stall budget, separate from the stream-error budget and capped at
    // `opts.retries` (#11). A stall is not a failure of the connection but
    // of its pace: the server is answering, too slowly to be worth waiting
    // for. Each stall re-requests with Range exactly as a stream error
    // does; when the budget is spent the file fails for good.
    let mut stall_attempt: usize = 0;
    let (stall_window, stall_grace) = stall::policy();
    let mut response = response;

    /// Why the chunk loop stopped reading a response body.
    enum StreamEnd {
        /// The body ended on its own.
        Done,
        /// The body stream returned an error.
        Error(reqwest_middleware::Error),
        /// The stall detector judged the stream too slow.
        Stalled { observed: u64, window_secs: u64 },
    }

    loop {
        let mut stream = response.bytes_stream();
        // Every stream is judged on its own clock: a re-request is a new
        // connection, possibly to a different backend, and gets the full
        // grace before its pace counts.
        let mut detector = (opts.min_speed > 0).then(|| {
            stall::StallDetector::new(
                opts.min_speed,
                stall_window,
                stall_grace,
                tokio::time::Instant::now(),
            )
        });
        // The check must run even when no chunk arrives, or a stream that
        // sends nothing would never be judged until the transport's read
        // timeout. `stream.next()` is cancel-safe, so a tick that wins the
        // race drops no bytes.
        let mut ticker = tokio::time::interval(stall::CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let end = loop {
            tokio::select! {
                next = stream.next() => match next {
                    Some(Ok(chunk)) => {
                        output.write_all(&chunk).await?;
                        if let Some(h) = hasher.as_mut() {
                            use md5::Digest;
                            h.update(&chunk);
                        }
                        bytes_downloaded += chunk.len() as u64;
                        if let Some(d) = detector.as_mut() {
                            d.record(tokio::time::Instant::now(), chunk.len() as u64);
                        }

                        // Early exit for a grossly oversized body (10% over,
                        // min 1 KB) so a runaway response cannot fill the
                        // disk. Any smaller discrepancy is caught by the
                        // exact count check after the stream ends.
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

                        // Rate-limit progress updates to every 256KB to
                        // reduce lock contention
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
                    Some(Err(stream_err)) => {
                        break StreamEnd::Error(reqwest_middleware::Error::from(stream_err));
                    }
                    None => break StreamEnd::Done,
                },
                _ = ticker.tick(), if detector.is_some() => {
                    if let Some(d) = detector.as_mut() {
                        if let Some(observed) = d.check(tokio::time::Instant::now()) {
                            break StreamEnd::Stalled {
                                observed,
                                window_secs: d.window_secs(),
                            };
                        }
                    }
                }
            }
        };
        // Close the abandoned connection before opening the next one.
        drop(stream);

        match end {
            StreamEnd::Done => break,
            StreamEnd::Error(m_err) => {
                // Any error after successful response headers is a
                // body-phase failure — connection drop, incomplete
                // message, decode error, or stream timeout. They all
                // share a remedy: sleep briefly, re-request with Range.
                // Flush buffered bytes to disk so the .part file size
                // matches `bytes_downloaded`: the Range offset for the
                // retry request, or what the next attempt resumes from.
                output.flush().await?;
                if stream_attempt >= MAX_STREAM_RETRIES {
                    return Err(IaError::Network(m_err));
                }
                stream_attempt += 1;
                let backoff = std::time::Duration::from_millis(
                    500 * 3u64.saturating_pow(stream_attempt as u32 - 1),
                );
                warn!(
                    file = %file.name,
                    attempt = stream_attempt,
                    max = MAX_STREAM_RETRIES,
                    bytes_downloaded,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %crate::error::format_error_chain(&m_err),
                    "body-stream error, retrying with Range",
                );
                tokio::time::sleep(backoff).await;
            }
            StreamEnd::Stalled {
                observed,
                window_secs,
            } => {
                // Flush first either way: the bytes that did arrive belong
                // on disk, as the Range offset or for a later resume.
                output.flush().await?;
                if stall_attempt >= opts.retries {
                    drop(output);
                    // This stall plus the ones already re-requested.
                    let stalls = stall_attempt + 1;
                    warn!(
                        file = %file.name,
                        stalls,
                        bytes_downloaded,
                        observed_bytes_per_sec = observed,
                        min_bytes_per_sec = opts.min_speed,
                        window_secs,
                        "stream below --min-speed and the stall budget is spent; keeping .part"
                    );
                    return Err(IaError::DownloadStalled {
                        file: file.name.clone(),
                        observed_bytes_per_sec: observed,
                        min_bytes_per_sec: opts.min_speed,
                        window_secs,
                        stalls,
                    });
                }
                stall_attempt += 1;
                // No backoff: the stream has already cost at least the
                // grace, and the server is answering, just too slowly.
                warn!(
                    file = %file.name,
                    attempt = stall_attempt,
                    max = opts.retries,
                    bytes_downloaded,
                    observed_bytes_per_sec = observed,
                    min_bytes_per_sec = opts.min_speed,
                    window_secs,
                    "stream below --min-speed, re-requesting with Range",
                );
            }
        }

        // Re-request from the bytes on disk. Shared by the stream-error
        // and stall paths.
        let new_resp =
            fetch_response(client, &url, Some(bytes_downloaded), opts.count_views).await?;
        if new_resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // Close the writer first: the mapping may remove .part.
            drop(output);
            let last_modified = last_modified_of(&new_resp).or(last_modified);
            range_not_satisfiable(new_resp, identifier, file, bytes_downloaded, &part_path).await?;
            // The .part is the whole file: the stream ended after its last
            // byte. The hasher already covers every byte written, so
            // finish in place.
            return finish_part(
                identifier,
                file,
                &file_path,
                &part_path,
                bytes_downloaded,
                hasher,
                last_modified,
                opts,
                progress,
                start,
            )
            .await;
        }
        check_content_range(&new_resp, identifier, file)?;
        // If the server ignores Range and returns 200, the safe thing is
        // to surface an error rather than try to splice a full-file
        // stream onto an existing `.part` offset.
        if new_resp.status() == reqwest::StatusCode::OK && bytes_downloaded > 0 {
            return Err(IaError::ResumeFailed {
                file: file.name.clone(),
                reason: "server ignored Range header on retry".to_string(),
            });
        }
        response = new_resp;
    }

    output.flush().await?;
    drop(output);

    // The stream ended. Refuse to rename a file whose byte count differs
    // from the item metadata. This runs before the md5 comparison because
    // that path moves .part away (to .md5-mismatch) on a mismatch.
    //
    // Short: the .part file is deliberately kept. The error is retryable
    // and the next attempt resumes it with Range.
    //
    // Long: the server sent more bytes than the metadata says the file has,
    // so the two disagree about its length and retrying cannot reconcile
    // them. A .part longer than the file is not a prefix of anything, and a
    // Range request from its end could only draw a 416, so it is removed.
    if let Some(expected) = file.size {
        if !is_size_unknowable(identifier, &file.name) {
            if bytes_downloaded > expected {
                warn!(
                    file = %file.name,
                    expected,
                    received = bytes_downloaded,
                    "body ran past the item metadata size; deleting .part"
                );
                let _ = fs::remove_file(&part_path).await;
                return Err(IaError::ServerSizeMismatch {
                    file: file.name.clone(),
                    metadata_size: expected,
                    server_size: bytes_downloaded,
                });
            }
            if bytes_downloaded < expected {
                warn!(
                    file = %file.name,
                    expected,
                    received = bytes_downloaded,
                    "byte count differs from item metadata; keeping .part for resume"
                );
                return Err(IaError::DownloadSizeMismatch {
                    file: file.name.clone(),
                    expected,
                    received: bytes_downloaded,
                });
            }
        }
    }

    finish_part(
        identifier,
        file,
        &file_path,
        &part_path,
        bytes_downloaded,
        hasher,
        last_modified,
        opts,
        progress,
        start,
    )
    .await
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
            // The wrong md5 the previous attempt produced, if it ended in a
            // checksum mismatch (#14). The same wrong md5 twice in a row
            // means the wire is fine and the source is wrong; downloading
            // again cannot change that. Any other outcome forgets it.
            let mut last_wrong_md5: Option<String> = None;

            for attempt in 0..=opts.retries {
                if attempt > 0 {
                    let delay = Duration::from_secs(2u64.pow(attempt as u32).min(60));
                    warn!(file = %file.name, attempt, "retrying after {:?}", delay);
                    tokio::time::sleep(delay).await;
                }

                match download_file(&client, &identifier, &file, &dest_dir, &opts, prog_ref).await {
                    Ok(result) => return result,
                    Err(IaError::ChecksumMismatch {
                        file: name,
                        expected,
                        actual,
                        kept,
                    }) if last_wrong_md5.as_deref() == Some(actual.as_str()) => {
                        warn!(
                            file = %file.name,
                            expected = %expected,
                            actual = %actual,
                            "same wrong md5 twice in a row; the source is wrong, not the transfer"
                        );
                        last_err = Some(IaError::SourceChecksumMismatch {
                            file: name,
                            expected,
                            actual,
                            kept,
                        });
                        break;
                    }
                    Err(e) => {
                        last_wrong_md5 = match &e {
                            IaError::ChecksumMismatch { actual, .. } => Some(actual.clone()),
                            _ => None,
                        };
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
        )
        .unwrap();

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
    async fn oversized_body_within_tolerance_is_permanent_and_deletes_part() {
        let mock_server = MockServer::start().await;
        // File metadata says 100 bytes. The mid-stream too-large abort only
        // fires past 100 + max(10, 1024) = 1124, so 105 bytes stream to the
        // end. A .part longer than the file is not a prefix of anything, so
        // the count check fails permanently and removes it.
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
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 100);
                assert_eq!(server_size, 105);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        assert!(!dir.path().join("normal.txt.part").exists());
        assert!(!dir.path().join("normal.txt").exists());
    }

    /// The oversize rule also covers a resumed stream: the 206's
    /// Content-Range total agrees with the metadata, but the body keeps
    /// going past the end it declared.
    #[tokio::test]
    async fn oversized_resume_body_deletes_part() {
        use wiremock::matchers::header_exists;
        let mock_server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("disk.img.part"), b"AAAAA").unwrap();
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(vec![b'B'; 27])
                    .insert_header("Content-Range", "bytes 5-29/30"),
            )
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let file = test_file_meta("disk.img", 30);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 30);
                assert_eq!(server_size, 32);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        assert!(!dir.path().join("disk.img.part").exists());
        assert!(!dir.path().join("disk.img").exists());
    }

    /// Through the outer retry loop an oversize body is tried exactly once:
    /// the error is permanent, so no retry and no second request.
    #[tokio::test]
    async fn oversized_body_is_not_retried() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/long-item/normal.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'Y'; 105]))
            .expect(1)
            .mount(&mock_server)
            .await;

        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("long-item".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("normal.txt", 100)],
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
            retries: 2,
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(1));

        let result =
            download_item_with_metadata(&client, "long-item", &item, &opts, semaphore, None)
                .await
                .unwrap();

        assert_eq!(result.files_failed, 1, "{result:?}");
        assert_eq!(result.files_downloaded, 0);
        assert!(!dir.path().join("long-item/normal.txt.part").exists());
        assert!(!dir.path().join("long-item/normal.txt").exists());
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

    /// A symlink `.part` is removed before the resume offset is read (#25),
    /// so no Range request is ever shaped by a planted link: the first
    /// attempt sends a plain GET and completes with the right bytes. Before
    /// the fix the link's target length went out as the Range offset, the
    /// 206 tail landed at offset 0 of a fresh `.part`, and a second run
    /// could resume that tail into a wrong file of the right length.
    #[cfg(unix)]
    #[tokio::test]
    async fn part_file_symlink_is_removed_before_the_range_request() {
        use wiremock::matchers::header_exists;

        let mock_server = MockServer::start().await;
        let full_body = b"complete file data here";

        // If a Range request still went out, this 206 would answer it and
        // the download could not come out right.
        Mock::given(method("GET"))
            .and(path("/download/test-item/ranged.txt"))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(b"data here".to_vec())
                    .insert_header("Content-Range", "bytes 14-22/23"),
            )
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/ranged.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full_body.to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target_file = target_dir.path().join("target.txt");
        std::fs::write(&target_file, "original content").unwrap(); // 16 bytes
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
        assert_eq!(
            std::fs::read(dir.path().join("ranged.txt")).unwrap(),
            full_body
        );
        let final_meta = std::fs::symlink_metadata(dir.path().join("ranged.txt")).unwrap();
        assert!(!final_meta.file_type().is_symlink());
        assert!(
            std::fs::symlink_metadata(&part_path).is_err(),
            "link removed"
        );
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "original content",
            "symlink target must not be modified"
        );
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
        assert!(requests[0].headers.get("range").is_none(), "no Range sent");
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
            IaError::Http {
                status, message, ..
            } => {
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
            IaError::Http {
                status, message, ..
            } => {
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

    /// Serve `body` for `b.txt` and return a client plus a temp dir.
    async fn mount_body(mock_server: &MockServer, body: &[u8]) -> (IaClient, tempfile::TempDir) {
        Mock::given(method("GET"))
            .and(path("/download/test-item/b.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        (client, tempfile::tempdir().unwrap())
    }

    const WRONG_MD5: &str = "00000000000000000000000000000000";

    fn checksum_opts() -> DownloadOpts {
        DownloadOpts {
            checksum: true,
            ..Default::default()
        }
    }

    /// A failed md5 check keeps the bytes as `<name>.md5-mismatch` and the
    /// error names that path (#14). Nothing is left as `.part`, so the
    /// next attempt starts from byte 0.
    #[tokio::test]
    async fn checksum_mismatch_keeps_the_download_as_md5_mismatch() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, WRONG_MD5);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap_err();

        let kept_path = dir.path().join("b.txt.md5-mismatch");
        match &err {
            IaError::ChecksumMismatch {
                expected,
                actual,
                kept,
                ..
            } => {
                assert_eq!(expected, WRONG_MD5);
                assert_eq!(actual, &md5_hex(&body));
                assert_eq!(
                    kept.as_deref(),
                    Some(kept_path.display().to_string().as_str())
                );
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        assert!(err.is_retryable());
        assert_eq!(std::fs::read(&kept_path).unwrap(), body);
        assert!(!dir.path().join("b.txt").exists());
        assert!(!dir.path().join("b.txt.part").exists());
    }

    /// Only one bad copy is kept per file: a new mismatch replaces it.
    #[tokio::test]
    async fn checksum_mismatch_overwrites_an_earlier_kept_copy() {
        let body = b"second bad copy".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::fs::write(&kept_path, b"first bad copy, longer than the second").unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, WRONG_MD5);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, IaError::ChecksumMismatch { .. }), "{err:?}");
        assert_eq!(std::fs::read(&kept_path).unwrap(), body);
    }

    /// Once a later attempt verifies, the bad copy has served its purpose
    /// and is removed.
    #[tokio::test]
    async fn verified_download_removes_an_earlier_kept_copy() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::fs::write(&kept_path, b"an earlier bad copy").unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, &md5_hex(&body));

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), body);
        assert!(!kept_path.exists(), "the earlier bad copy should be gone");
    }

    /// A completion without --checksum does not touch an existing bad
    /// copy: nothing was verified, so there is no reason to drop evidence.
    #[tokio::test]
    async fn unverified_download_leaves_an_earlier_kept_copy() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::fs::write(&kept_path, b"an earlier bad copy").unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, WRONG_MD5);

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
        assert_eq!(std::fs::read(&kept_path).unwrap(), b"an earlier bad copy");
    }

    /// Something the rename cannot replace sits at the kept path (here a
    /// directory). The copy cannot be kept, so the `.part` is removed
    /// instead, the next attempt starts from byte 0, and the error says the
    /// copy could not be kept. The old `.part` must not survive, or every
    /// later run would re-hash the same bytes and fail the same way.
    #[tokio::test]
    async fn checksum_mismatch_with_a_directory_at_the_kept_path_removes_the_part() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::fs::create_dir(&kept_path).unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, WRONG_MD5);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap_err();

        match &err {
            IaError::ChecksumMismatch { kept, .. } => assert_eq!(*kept, None),
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        assert!(err.to_string().contains("could not be kept"), "{err}");
        assert!(err.is_retryable());
        assert!(kept_path.is_dir(), "the directory is left alone");
        assert!(!dir.path().join("b.txt.part").exists());
        assert!(!dir.path().join("b.txt").exists());
    }

    /// A verified download with something unremovable at the kept path:
    /// the file is in place and Complete; the stale copy is a warning, not
    /// a failure.
    #[tokio::test]
    async fn verified_download_with_a_directory_at_the_kept_path_still_completes() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::fs::create_dir(&kept_path).unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, &md5_hex(&body));

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), body);
        assert!(!dir.path().join("b.txt.part").exists());
        assert!(kept_path.is_dir());
    }

    /// A planted symlink at the kept path is replaced by the rename, never
    /// written through.
    #[cfg(unix)]
    #[tokio::test]
    async fn checksum_mismatch_through_a_symlinked_kept_path_replaces_the_link() {
        let body = b"correct content".to_vec();
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_body(&mock_server, &body).await;
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("target.bin");
        std::fs::write(&target, b"untouchable").unwrap();
        let kept_path = dir.path().join("b.txt.md5-mismatch");
        std::os::unix::fs::symlink(&target, &kept_path).unwrap();
        let file = test_file_meta_with_md5("b.txt", body.len() as u64, WRONG_MD5);

        let err = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &checksum_opts(),
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, IaError::ChecksumMismatch { .. }), "{err:?}");
        let meta = std::fs::symlink_metadata(&kept_path).unwrap();
        assert!(
            meta.file_type().is_file(),
            "kept path is now a regular file"
        );
        assert_eq!(std::fs::read(&kept_path).unwrap(), body);
        assert_eq!(std::fs::read(&target).unwrap(), b"untouchable");
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

    /// A body-stream error mid-download (e.g. connection drop after headers)
    /// bypasses the middleware retry, which only inspects response status.
    /// The stream-level retry wrapper should catch the error, sleep briefly,
    /// and re-issue the request with `Range: bytes={bytes_downloaded}-`
    /// using the existing `.part` file.
    ///
    /// Simulated with a raw TCP listener because wiremock's hyper server
    /// refuses to send a response whose Content-Length doesn't match its
    /// body — the error surfaces as a pre-response connection drop rather
    /// than a mid-body drop. A raw listener lets us ship valid headers and
    /// then close the connection after a partial body.
    #[tokio::test]
    async fn stream_error_retries_with_range_and_completes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let full_body: Vec<u8> = (0..32u8).collect();
        let full_len = full_body.len() as u64;
        let chopped_at: u64 = 12;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_body = full_body.clone();

        let server_handle = tokio::spawn(async move {
            // Helper: read the HTTP request until "\r\n\r\n" to extract
            // interesting headers (we only care about presence of Range).
            async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
                let mut buf = vec![0u8; 4096];
                let mut acc = Vec::new();
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                String::from_utf8_lossy(&acc).to_string()
            }

            // Accept 1: full response headers promising 32 bytes, but close
            // the socket after sending only 12 bytes of body.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n\
                     content-length: {full_len}\r\n\
                     content-type: application/octet-stream\r\n\
                     accept-ranges: bytes\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream
                    .write_all(&server_body[..chopped_at as usize])
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
                drop(stream); // abrupt close mid-body
            }

            // Accept 2: Range request; honor it with 206 + remaining bytes.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = read_request(&mut stream).await;
                assert!(
                    req.to_ascii_lowercase()
                        .contains(&format!("range: bytes={chopped_at}-")),
                    "expected Range header on retry, got:\n{req}"
                );
                let remainder = &server_body[chopped_at as usize..];
                let headers = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     content-length: {}\r\n\
                     content-type: application/octet-stream\r\n\
                     content-range: bytes {chopped_at}-{end}/{full_len}\r\n\
                     connection: close\r\n\
                     \r\n",
                    remainder.len(),
                    end = full_len - 1,
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(remainder).await.unwrap();
                stream.flush().await.unwrap();
            }
        });

        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut file = test_file_meta("data.bin", full_len);
        // md5 of bytes 0..32:
        //   python -c "import hashlib;print(hashlib.md5(bytes(range(32))).hexdigest())"
        file.md5 = Some("b4ffcb23737cec315a4a4d1aa2a620ce".to_string());

        let result = download_file(
            &client,
            "flaky-item",
            &file,
            dir.path(),
            &DownloadOpts {
                checksum: true,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("download should succeed after retry");

        assert_eq!(result.bytes, full_len);
        let got = std::fs::read(dir.path().join("data.bin")).unwrap();
        assert_eq!(got, full_body, "final file should contain full body");

        server_handle.await.unwrap();
    }

    /// The Content-Range check must also cover the Range re-request that the
    /// body-stream retry sends. wiremock always sets a correct Content-Length,
    /// so the first response's mid-body drop needs the raw listener.
    #[tokio::test]
    async fn stream_retry_response_with_wrong_total_fails_permanently() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let full_body: Vec<u8> = (0..32u8).collect();
        let full_len = full_body.len() as u64;
        let chopped_at: u64 = 12;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_body = full_body.clone();

        let server_handle = tokio::spawn(async move {
            async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
                let mut buf = vec![0u8; 4096];
                let mut acc = Vec::new();
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                String::from_utf8_lossy(&acc).to_string()
            }

            // Accept 1: promise 32 bytes, send 12, close.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n\
                     content-length: {full_len}\r\n\
                     content-type: application/octet-stream\r\n\
                     accept-ranges: bytes\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream
                    .write_all(&server_body[..chopped_at as usize])
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
                drop(stream);
            }

            // Accept 2: the Range re-request. Answer 206 but claim the file
            // is 40 bytes long, not 32. The client may hang up without
            // reading the body, so write errors are ignored here.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = read_request(&mut stream).await;
                assert!(
                    req.to_ascii_lowercase()
                        .contains(&format!("range: bytes={chopped_at}-")),
                    "expected Range header on retry, got:\n{req}"
                );
                let remainder = &server_body[chopped_at as usize..];
                let headers = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     content-length: {}\r\n\
                     content-type: application/octet-stream\r\n\
                     content-range: bytes {chopped_at}-{end}/40\r\n\
                     connection: close\r\n\
                     \r\n",
                    remainder.len(),
                    end = full_len - 1,
                );
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.write_all(remainder).await;
                let _ = stream.flush().await;
            }
        });

        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", full_len);

        let result = download_file(
            &client,
            "flaky-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 32);
                assert_eq!(server_size, 40);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        // The first response's 12 bytes were flushed before the re-request;
        // nothing from the second response was written.
        let part = std::fs::read(dir.path().join("data.bin.part")).unwrap();
        assert_eq!(part, &full_body[..chopped_at as usize]);
        assert!(!dir.path().join("data.bin").exists());

        server_handle.await.unwrap();
    }

    /// The 416 mapping must also cover the Range re-request that the
    /// body-stream retry sends. The server here shrinks the file between
    /// the two requests: it promised 32 bytes, dropped after 12, and then
    /// answers the `Range: bytes=12-` re-request with 416 `bytes */12`.
    #[tokio::test]
    async fn stream_retry_response_416_fails_permanently() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let full_body: Vec<u8> = (0..32u8).collect();
        let full_len = full_body.len() as u64;
        let chopped_at: u64 = 12;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_body = full_body.clone();

        let server_handle = tokio::spawn(async move {
            async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
                let mut buf = vec![0u8; 4096];
                let mut acc = Vec::new();
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                String::from_utf8_lossy(&acc).to_string()
            }

            // Accept 1: promise 32 bytes, send 12, close.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n\
                     content-length: {full_len}\r\n\
                     content-type: application/octet-stream\r\n\
                     accept-ranges: bytes\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream
                    .write_all(&server_body[..chopped_at as usize])
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
                drop(stream);
            }

            // Accept 2: the Range re-request. Answer 416 and say the file is
            // only 12 bytes long.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = read_request(&mut stream).await;
                assert!(
                    req.to_ascii_lowercase()
                        .contains(&format!("range: bytes={chopped_at}-")),
                    "expected Range header on retry, got:\n{req}"
                );
                let headers = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                     content-length: 0\r\n\
                     content-range: bytes */{chopped_at}\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });

        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", full_len);

        let result = download_file(
            &client,
            "flaky-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 32);
                assert_eq!(server_size, 12);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        // The 12 bytes from the first response were flushed before the
        // re-request and stay on disk.
        let part = std::fs::read(dir.path().join("data.bin.part")).unwrap();
        assert_eq!(part, &full_body[..chopped_at as usize]);
        assert!(!dir.path().join("data.bin").exists());

        server_handle.await.unwrap();
    }

    /// The shortcut must also cover the Range re-request that the
    /// body-stream retry sends. The server here promises 33 bytes for a
    /// 32-byte file, sends all 32, and closes, so hyper raises a body-stream
    /// error after the last real byte arrived. The `Range: bytes=32-`
    /// re-request is answered 416 `bytes */32`: the .part on disk is the
    /// whole file, the rolling md5 already covers it, and the file is
    /// finished without a third connection.
    #[tokio::test]
    async fn stream_retry_response_416_at_full_length_completes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let full_body: Vec<u8> = (0..32u8).collect();
        let full_len = full_body.len() as u64;
        let promised_len = full_len + 1;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_body = full_body.clone();

        let server_handle = tokio::spawn(async move {
            async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
                let mut buf = vec![0u8; 4096];
                let mut acc = Vec::new();
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                String::from_utf8_lossy(&acc).to_string()
            }

            // Accept 1: promise 33 bytes, send the real 32, close.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n\
                     content-length: {promised_len}\r\n\
                     content-type: application/octet-stream\r\n\
                     accept-ranges: bytes\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(&server_body).await.unwrap();
                stream.flush().await.unwrap();
                drop(stream);
            }

            // Accept 2: the Range re-request from byte 32. The file is 32
            // bytes long, so that offset is past the end.
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = read_request(&mut stream).await;
                assert!(
                    req.to_ascii_lowercase()
                        .contains(&format!("range: bytes={full_len}-")),
                    "expected Range header on retry, got:\n{req}"
                );
                let headers = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                     content-length: 0\r\n\
                     content-range: bytes */{full_len}\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.flush().await;
            }

            // A third connection would mean the file was re-downloaded.
            let third =
                tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept())
                    .await;
            assert!(third.is_err(), "unexpected third connection");
        });

        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut file = test_file_meta("data.bin", full_len);
        // md5 of bytes 0..32, as in stream_error_retries_with_range_and_completes.
        file.md5 = Some("b4ffcb23737cec315a4a4d1aa2a620ce".to_string());

        let result = download_file(
            &client,
            "flaky-item",
            &file,
            dir.path(),
            &DownloadOpts {
                checksum: true,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, full_len);
        assert_eq!(
            std::fs::read(dir.path().join("data.bin")).unwrap(),
            full_body
        );
        assert!(!dir.path().join("data.bin.part").exists());

        server_handle.await.unwrap();
    }

    // -- stall detection (#11) --
    //
    // Drip-feeding servers on a raw TcpListener, in real time with the
    // window and grace shrunk through `stall::PolicyOverride` (window 2 s,
    // grace 1 s) so each stall costs about a second of wall time. tokio's
    // paused clock cannot be used: its auto-advance runs reqwest's connect
    // timeout out before a real TCP connect completes. The 60 s read timeout
    // on the transport stays real, which is what the silent-stream test
    // relies on to show the detector fired first.

    type ConnHandler = Box<
        dyn FnOnce(tokio::net::TcpStream, String) -> futures::future::BoxFuture<'static, ()> + Send,
    >;

    #[derive(Debug)]
    struct ConnRecord {
        request: String,
        accepted_at: tokio::time::Instant,
        finished_at: tokio::time::Instant,
    }

    impl ConnRecord {
        fn range_offset(&self) -> Option<u64> {
            range_offset_of(&self.request)
        }
    }

    /// The `N` of a `Range: bytes=N-` header in a request head.
    fn range_offset_of(request: &str) -> Option<u64> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("range") {
                return None;
            }
            value
                .trim()
                .strip_prefix("bytes=")?
                .strip_suffix('-')?
                .parse()
                .ok()
        })
    }

    async fn read_request_head(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 4096];
        let mut acc = Vec::new();
        loop {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&acc).to_string()
    }

    /// Serve `handlers` in order, one per connection, then make sure no
    /// further connection arrives within `quiet_for`. The task's result is
    /// one record per connection: the request head and when the handler
    /// started and finished.
    async fn spawn_script_server(
        handlers: Vec<ConnHandler>,
        quiet_for: Duration,
    ) -> (IaClient, tokio::task::JoinHandle<Vec<ConnRecord>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = handlers.len();
        let handle = tokio::spawn(async move {
            let mut records = Vec::new();
            for handler in handlers {
                let (mut stream, _) = listener.accept().await.unwrap();
                let accepted_at = tokio::time::Instant::now();
                let request = read_request_head(&mut stream).await;
                handler(stream, request.clone()).await;
                records.push(ConnRecord {
                    request,
                    accepted_at,
                    finished_at: tokio::time::Instant::now(),
                });
            }
            // Any further connection within `quiet_for` is a bug in the
            // client: a re-request or a retry that should not have happened.
            let extra = tokio::time::timeout(quiet_for, listener.accept()).await;
            assert!(
                extra.is_err(),
                "a connection arrived after the {expected} scripted ones"
            );
            records
        });
        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        (IaClient::from_config(config).unwrap(), handle)
    }

    /// Write a 200 (no Range in the request) or a 206 from the requested
    /// offset, promising `promised_total - offset` body bytes. Returns the
    /// offset. `promised_total` may exceed the real length to make the
    /// client see a chopped stream as an error.
    async fn send_head(
        stream: &mut tokio::net::TcpStream,
        request: &str,
        promised_total: u64,
    ) -> u64 {
        use tokio::io::AsyncWriteExt;
        let offset = range_offset_of(request);
        let head = match offset {
            Some(offset) => format!(
                "HTTP/1.1 206 Partial Content\r\n\
                 content-length: {}\r\n\
                 content-range: bytes {offset}-{}/{promised_total}\r\n\
                 content-type: application/octet-stream\r\n\
                 connection: close\r\n\
                 \r\n",
                promised_total - offset,
                promised_total - 1,
            ),
            None => format!(
                "HTTP/1.1 200 OK\r\n\
                 content-length: {promised_total}\r\n\
                 content-type: application/octet-stream\r\n\
                 accept-ranges: bytes\r\n\
                 connection: close\r\n\
                 \r\n"
            ),
        };
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        offset.unwrap_or(0)
    }

    async fn send_all(stream: &mut tokio::net::TcpStream, bytes: &[u8]) {
        use tokio::io::AsyncWriteExt;
        let _ = stream.write_all(bytes).await;
        let _ = stream.flush().await;
    }

    /// Write `step` bytes every `every` until the client goes away or the
    /// bytes run out. Returns how many bytes were written.
    async fn drip(
        stream: tokio::net::TcpStream,
        bytes: &[u8],
        step: usize,
        every: Duration,
    ) -> usize {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut rd, mut wr) = stream.into_split();
        let mut sink = [0u8; 64];
        let mut written = 0;
        while written < bytes.len() {
            tokio::select! {
                _ = tokio::time::sleep(every) => {
                    let end = (written + step).min(bytes.len());
                    if wr.write_all(&bytes[written..end]).await.is_err()
                        || wr.flush().await.is_err()
                    {
                        break;
                    }
                    written = end;
                }
                closed = rd.read(&mut sink) => {
                    if matches!(closed, Ok(0) | Err(_)) {
                        break;
                    }
                }
            }
        }
        written
    }

    /// Block until the client closes the connection or `max` passes.
    async fn wait_for_close(stream: &mut tokio::net::TcpStream, max: Duration) {
        use tokio::io::AsyncReadExt;
        let mut sink = [0u8; 64];
        let _ = tokio::time::timeout(max, stream.read(&mut sink)).await;
    }

    /// A connection that drips `body` one byte every `every` from the
    /// requested offset: below any sane floor, so the client must abandon
    /// it.
    fn dripping(body: Vec<u8>, every: Duration) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset = send_head(&mut stream, &request, body.len() as u64).await as usize;
                drip(stream, &body[offset..], 1, every).await;
            })
        })
    }

    /// A connection that serves the rest of `body` at once.
    fn serving(body: Vec<u8>) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset = send_head(&mut stream, &request, body.len() as u64).await as usize;
                send_all(&mut stream, &body[offset..]).await;
            })
        })
    }

    /// A connection that sends the head and nothing else until the client
    /// hangs up (or `max` passes).
    fn silent(total: u64, max: Duration) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                send_head(&mut stream, &request, total).await;
                wait_for_close(&mut stream, max).await;
            })
        })
    }

    /// A connection that sends the head, waits `pause`, then serves the rest
    /// of `body` at once.
    fn pausing_then_serving(body: Vec<u8>, pause: Duration) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset = send_head(&mut stream, &request, body.len() as u64).await as usize;
                tokio::time::sleep(pause).await;
                send_all(&mut stream, &body[offset..]).await;
            })
        })
    }

    /// A connection that promises the rest of `body`, sends `chunk` bytes
    /// of it, and closes: a body-stream error on the client.
    fn chopping(body: Vec<u8>, chunk: usize) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset = send_head(&mut stream, &request, body.len() as u64).await as usize;
                let end = (offset + chunk).min(body.len());
                send_all(&mut stream, &body[offset..end]).await;
            })
        })
    }

    fn stall_body() -> Vec<u8> {
        (0..64 * 1024u32).map(|i| (i % 251) as u8).collect()
    }

    const TEST_WINDOW: Duration = Duration::from_secs(2);
    const TEST_GRACE: Duration = Duration::from_secs(1);
    /// One byte this often is far below any floor.
    const DRIP: Duration = Duration::from_millis(250);
    const FLOOR: u64 = 10 * 1024;

    fn shrink_policy() -> stall::PolicyOverride {
        stall::PolicyOverride::new(TEST_WINDOW, TEST_GRACE)
    }

    fn stall_opts(min_speed: u64, retries: usize) -> DownloadOpts {
        DownloadOpts {
            min_speed,
            retries,
            ..Default::default()
        }
    }

    /// Real time. A hung download fails the test instead of the suite.
    const STALL_TEST_LIMIT: Duration = Duration::from_secs(60);

    async fn run_download(
        client: &IaClient,
        file: &FileMetadata,
        dir: &Path,
        opts: &DownloadOpts,
    ) -> Result<FileDownloadResult> {
        tokio::time::timeout(
            STALL_TEST_LIMIT,
            download_file(client, "slow-item", file, dir, opts, None),
        )
        .await
        .expect("download_file hung past the time limit")
    }

    #[tokio::test]
    async fn drip_feed_stalls_then_resumes_with_range() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![dripping(body.clone(), DRIP), serving(body.clone())],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        let result = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 5))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert!(!dir.path().join("data.bin.part").exists());

        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        assert_eq!(records[0].range_offset(), None);
        // The re-request resumed from the dripped bytes: a handful during
        // the grace, give or take the byte in flight when the stream was
        // abandoned. The final file proves no byte was lost or doubled.
        let resumed_from = records[1]
            .range_offset()
            .expect("second request carried Range");
        assert!(
            (1..=12).contains(&resumed_from),
            "resumed from {resumed_from}"
        );
    }

    #[tokio::test]
    async fn silent_stream_stalls_at_the_end_of_grace() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![
                silent(body.len() as u64, Duration::from_secs(30)),
                serving(body.clone()),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        let result = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 5))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);

        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        // No chunk ever arrived, so only the once-a-second check could have
        // noticed. It fired at the end of the grace, far inside the
        // transport's 60 s read timeout.
        let held_for = records[0].finished_at - records[0].accepted_at;
        assert!(
            (TEST_GRACE..Duration::from_secs(10)).contains(&held_for),
            "first connection held for {held_for:?}"
        );
        assert_eq!(records[1].range_offset(), Some(0));
    }

    #[tokio::test]
    async fn stall_budget_is_separate_from_stream_error_budget() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![
                dripping(body.clone(), DRIP),
                chopping(body.clone(), 1000),
                serving(body.clone()),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        // One stall allowed. The stall uses it; the chopped stream that
        // follows is a body-stream error on its own budget of three.
        let result = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 1))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        let records = server.await.unwrap();
        assert_eq!(records.len(), 3, "{records:#?}");
        let second = records[1].range_offset().unwrap();
        let third = records[2].range_offset().unwrap();
        assert_eq!(third, second + 1000, "{records:#?}");
    }

    #[tokio::test]
    async fn spent_stall_budget_fails_permanently() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![
                dripping(body.clone(), DRIP),
                dripping(body.clone(), DRIP),
                dripping(body.clone(), DRIP),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        let result = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 2)).await;

        let err = result.expect_err("every stream dripped");
        match &err {
            IaError::DownloadStalled {
                file: name,
                min_bytes_per_sec,
                window_secs,
                stalls,
                observed_bytes_per_sec,
            } => {
                assert_eq!(name, "data.bin");
                assert_eq!(*min_bytes_per_sec, FLOOR);
                assert_eq!(*window_secs, TEST_WINDOW.as_secs());
                // Three streams stalled; the first two were re-requested.
                assert_eq!(*stalls, 3);
                assert!(*observed_bytes_per_sec < FLOOR);
            }
            other => panic!("expected DownloadStalled, got {other:?}"),
        }
        assert!(!err.is_retryable());
        // Every dripped byte is on disk for a later resume.
        let part = std::fs::read(dir.path().join("data.bin.part")).unwrap();
        assert!(!part.is_empty());
        assert_eq!(&body[..part.len()], &part[..]);
        assert!(!dir.path().join("data.bin").exists());
        let records = server.await.unwrap();
        assert_eq!(records.len(), 3, "{records:#?}");
    }

    #[tokio::test]
    async fn min_speed_zero_disables_detection() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![pausing_then_serving(
                body.clone(),
                Duration::from_millis(2500),
            )],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        // 2.5 s of silence after the head is past the grace and the window,
        // but with the check off only the 60 s read timeout could end the
        // stream, and it does not get there.
        let result = run_download(&client, &file, dir.path(), &stall_opts(0, 5))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn grace_restarts_on_each_stream() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![
                dripping(body.clone(), DRIP),
                pausing_then_serving(body.clone(), Duration::from_millis(600)),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        // The second stream is silent for 600 ms. Judged by the first
        // stream's clock it would be well past the grace with nothing in the
        // window; on its own clock it is still inside the grace when the
        // body arrives.
        let result = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 5))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn stall_keeps_rolling_md5_correct() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![dripping(body.clone(), DRIP), serving(body.clone())],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta_with_md5("data.bin", body.len() as u64, &md5_hex(&body));
        let opts = DownloadOpts {
            checksum: true,
            ..stall_opts(FLOOR, 5)
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        // The hasher saw the dripped bytes and then the resumed stream; a
        // reset at the re-request would fail the compare.
        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn stalled_file_is_not_retried_by_the_outer_loop() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};
        let _policy = shrink_policy();
        let body = stall_body();
        // The outer loop's first retry delay is 2 s; a wrongful retry would
        // connect again within the 3 s quiet period.
        let (client, server) = spawn_script_server(
            vec![dripping(body.clone(), DRIP), dripping(body.clone(), DRIP)],
            Duration::from_secs(3),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("slow-item".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("data.bin", body.len() as u64)],
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra: HashMap::new(),
        };
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            ..stall_opts(FLOOR, 1)
        };

        // retries = 1 is one stall re-request. The permanent error that
        // follows must not earn the file a fresh attempt from the top,
        // which would be two more dripping connections.
        let result = tokio::time::timeout(
            STALL_TEST_LIMIT,
            download_item_with_metadata(
                &client,
                "slow-item",
                &item,
                &opts,
                Arc::new(Semaphore::new(1)),
                None,
            ),
        )
        .await
        .expect("download hung past the time limit")
        .unwrap();

        assert_eq!(result.files_failed, 1, "{result:?}");
        assert_eq!(result.files_downloaded, 0);
        let failed = &result.results[0];
        match &failed.status {
            DownloadStatus::Failed(msg) => assert!(msg.contains("stalled 2 times"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(server.await.unwrap().len(), 2);
    }

    /// A connection that answers a Range request with 416 and the given
    /// total, as the server does when the offset is at the end of the file.
    fn range_not_satisfiable_at(total: u64) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                assert!(
                    range_offset_of(&request).is_some(),
                    "expected a Range request, got:\n{request}"
                );
                let head = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                     content-length: 0\r\n\
                     content-range: bytes */{total}\r\n\
                     connection: close\r\n\
                     \r\n"
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.flush().await;
            })
        })
    }

    /// A connection that promises one byte more than it has, serves the
    /// whole body at once, and then goes quiet: the client has every byte
    /// but the stream never ends.
    fn serving_then_hanging(body: Vec<u8>, max: Duration) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset = send_head(&mut stream, &request, body.len() as u64 + 1).await as usize;
                send_all(&mut stream, &body[offset..]).await;
                wait_for_close(&mut stream, max).await;
            })
        })
    }

    #[test]
    fn default_min_speed_is_ten_kib_per_second() {
        assert_eq!(DownloadOpts::default().min_speed, 10 * 1024);
    }

    #[tokio::test]
    async fn retries_zero_fails_on_the_first_stall() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) =
            spawn_script_server(vec![dripping(body.clone(), DRIP)], Duration::ZERO).await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", body.len() as u64);

        let err = run_download(&client, &file, dir.path(), &stall_opts(FLOOR, 0))
            .await
            .expect_err("the only stream dripped");

        match &err {
            IaError::DownloadStalled { stalls, .. } => assert_eq!(*stalls, 1),
            other => panic!("expected DownloadStalled, got {other:?}"),
        }
        assert!(err.to_string().contains("stalled 1 time:"), "{err}");
        assert!(dir.path().join("data.bin.part").exists());
        assert_eq!(server.await.unwrap().len(), 1);
    }

    /// The stream delivered every byte and then stalled before ending. The
    /// Range re-request from the end of the file draws a 416 at the
    /// metadata size, and the shared tail finishes the .part in place.
    #[tokio::test]
    async fn stall_after_the_last_byte_finishes_through_416() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![
                serving_then_hanging(body.clone(), Duration::from_secs(30)),
                range_not_satisfiable_at(body.len() as u64),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta_with_md5("data.bin", body.len() as u64, &md5_hex(&body));
        let opts = DownloadOpts {
            checksum: true,
            ..stall_opts(FLOOR, 5)
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, body.len() as u64);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert!(!dir.path().join("data.bin.part").exists());
        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        assert_eq!(records[1].range_offset(), Some(body.len() as u64));
    }

    /// A stall part way through a resumed download: the hasher was seeded
    /// from the existing .part, rolled over the dripped bytes, and rolls on
    /// over the second resume. The final md5 must still match.
    #[tokio::test]
    async fn stall_during_resumed_download_keeps_seeded_md5() {
        let _policy = shrink_policy();
        let body = stall_body();
        let (client, server) = spawn_script_server(
            vec![dripping(body.clone(), DRIP), serving(body.clone())],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let on_disk = 10 * 1024;
        std::fs::write(dir.path().join("data.bin.part"), &body[..on_disk]).unwrap();
        let file = test_file_meta_with_md5("data.bin", body.len() as u64, &md5_hex(&body));
        let opts = DownloadOpts {
            checksum: true,
            ..stall_opts(FLOOR, 5)
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        assert_eq!(records[0].range_offset(), Some(on_disk as u64));
        let resumed = records[1].range_offset().unwrap();
        assert!(
            resumed > on_disk as u64 && resumed <= on_disk as u64 + 12,
            "{resumed}"
        );
    }

    // -- 416 shortcut corner cases (#12 follow-up, PR #27 audit) --

    /// `.part` length equals the server total but the metadata disagrees:
    /// the disagreement wins, as on a 206, and nothing is touched.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_with_different_total_keeps_part() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 30, Some("bytes */30")).await;
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 32);
                assert_eq!(server_size, 30);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(dir.path().join("disk.img.part"))
                .unwrap()
                .len(),
            30
        );
        assert!(!dir.path().join("disk.img").exists());
    }

    /// A 416 for an offset inside the file violates the protocol (the range
    /// was satisfiable). The `.part` is not trusted: it is removed and the
    /// download restarts from zero.
    #[tokio::test]
    async fn range_not_satisfiable_before_part_length_deletes_part_and_restarts() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 20, Some("bytes */32")).await;
        let full = vec![b'F'; 32];
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full.clone()))
            .mount(&mock_server)
            .await;
        let file = test_file_meta("disk.img", 32);

        let first = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match &first {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 20);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(first.unwrap_err().is_retryable());
        assert!(!dir.path().join("disk.img.part").exists());

        let second = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("disk.img")).unwrap(), full);
    }

    /// `_files.xml` has no trustworthy metadata size, so even a `.part` at
    /// the server's length is not finished in place (the literal reading of
    /// "all three agree"). It is removed and the download restarts.
    #[tokio::test]
    async fn range_not_satisfiable_on_files_xml_at_part_length_restarts() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "test-item_files.xml", 30, Some("bytes */30")).await;
        let file = test_file_meta("test-item_files.xml", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match &result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 30);
                assert_eq!(*received, 30);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(result.unwrap_err().is_retryable());
        assert!(!dir.path().join("test-item_files.xml.part").exists());
        assert!(!dir.path().join("test-item_files.xml").exists());
    }

    /// An empty file whose empty `.part` draws `416 bytes */0`: zero, zero
    /// and zero agree, and the empty file is finished, md5 and all.
    #[tokio::test]
    async fn range_not_satisfiable_at_zero_length_completes_empty_file() {
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_resume_416(&mock_server, "empty.txt", 0, Some("bytes */0")).await;
        // test_file_meta's md5 is the md5 of the empty string.
        let file = test_file_meta("empty.txt", 0);
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, 0);
        assert_eq!(std::fs::read(dir.path().join("empty.txt")).unwrap(), b"");
        assert!(!dir.path().join("empty.txt.part").exists());
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    /// A 416 may carry Last-Modified. It is preferred over the metadata
    /// mtime, exactly as on a 200 or 206.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_uses_last_modified_from_the_416() {
        use wiremock::matchers::header_exists;
        let mock_server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("disk.img.part"), vec![b'A'; 32]).unwrap();
        let server_mtime = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(416)
                    .insert_header("Content-Range", "bytes */32")
                    .insert_header(
                        "Last-Modified",
                        httpdate::fmt_http_date(server_mtime).as_str(),
                    ),
            )
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        // Metadata says 1700000000; the header must win.
        let file = test_file_meta("disk.img", 32);

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
        let mtime = std::fs::metadata(dir.path().join("disk.img"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime, server_mtime);
    }

    /// `--checksum` on a file with no md5 in metadata: nothing to compare,
    /// so the `.part` is not read back. The only events are Starting and
    /// Complete; in particular no Downloading, since nothing streamed.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_with_checksum_but_no_md5_skips_hashing() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let mut file = test_file_meta("disk.img", 32);
        file.md5 = None;
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

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, Some(&*cb))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(
            std::fs::read(dir.path().join("disk.img")).unwrap(),
            vec![b'A'; 32]
        );
        let seen = events.lock().unwrap();
        assert_eq!(
            *seen,
            vec![DownloadStatus::Starting, DownloadStatus::Complete],
            "{seen:?}"
        );
    }

    /// The shortcut's Complete is an ordinary completion to the item loop:
    /// it counts as downloaded with its byte count, after one request.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_counts_as_downloaded_in_item_result() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};
        use wiremock::matchers::header_exists;

        let mock_server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let item_dir = dir.path().join("done-item");
        std::fs::create_dir_all(&item_dir).unwrap();
        std::fs::write(item_dir.join("disk.img.part"), vec![b'A'; 32]).unwrap();
        Mock::given(method("GET"))
            .and(path("/download/done-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(ResponseTemplate::new(416).insert_header("Content-Range", "bytes */32"))
            .expect(1)
            .mount(&mock_server)
            .await;
        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("done-item".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("disk.img", 32)],
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
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let result = download_item_with_metadata(
            &client,
            "done-item",
            &item,
            &opts,
            Arc::new(Semaphore::new(1)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.files_downloaded, 1, "{result:?}");
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_skipped, 0);
        assert_eq!(result.bytes_total, 32);
        assert_eq!(
            std::fs::read(item_dir.join("disk.img")).unwrap(),
            vec![b'A'; 32]
        );
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    /// Mid-stream 416 at full length on a resumed download: the hasher was
    /// seeded from the existing `.part`, rolled over the streamed bytes, and
    /// the 416 confirms the file is whole. An unseeded hasher would give the
    /// md5 of bytes 12..32 and fail the compare.
    #[tokio::test]
    async fn stream_retry_response_416_at_full_length_completes_resumed_download() {
        let body: Vec<u8> = (0..32u8).collect();
        let on_disk = 12usize;
        let body_for_handler = body.clone();
        // 206 from byte 12 that promises 21 bytes but sends 20 and closes:
        // the client has all 32 bytes and a body-stream error.
        let over_promising: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                assert_eq!(range_offset_of(&request), Some(on_disk as u64), "{request}");
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     content-length: {}\r\n\
                     content-range: bytes {on_disk}-31/32\r\n\
                     content-type: application/octet-stream\r\n\
                     connection: close\r\n\
                     \r\n",
                    32 - on_disk + 1
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                send_all(&mut stream, &body_for_handler[on_disk..]).await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![over_promising, range_not_satisfiable_at(32)],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin.part"), &body[..on_disk]).unwrap();
        let mut file = test_file_meta("data.bin", 32);
        file.md5 = Some("b4ffcb23737cec315a4a4d1aa2a620ce".to_string());
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, 32);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert!(!dir.path().join("data.bin.part").exists());
        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        assert_eq!(records[1].range_offset(), Some(32));
    }

    /// Mid-stream 416 at full length without --checksum: nothing to hash,
    /// the `.part` is simply renamed.
    #[tokio::test]
    async fn stream_retry_response_416_at_full_length_completes_without_checksum() {
        let body: Vec<u8> = (0..32u8).collect();
        let body_for_handler = body.clone();
        let over_promising: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                send_head(&mut stream, &request, 33).await;
                send_all(&mut stream, &body_for_handler).await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![over_promising, range_not_satisfiable_at(32)],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", 32);

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        assert_eq!(server.await.unwrap().len(), 2);
    }

    /// The server sent more bytes than the metadata size (within the 10%
    /// mid-stream tolerance) and then dropped. The Range re-request from
    /// byte 35 draws `416 bytes */32`: total and metadata agree, the `.part`
    /// is longer than both, so it is removed and the download restarts.
    #[tokio::test]
    async fn stream_retry_response_416_past_metadata_size_deletes_part_and_restarts() {
        let sent: Vec<u8> = (0..35u8).collect();
        let sent_for_handler = sent.clone();
        let over_long: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                send_head(&mut stream, &request, 40).await;
                send_all(&mut stream, &sent_for_handler).await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![over_long, range_not_satisfiable_at(32)],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", 32);

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default()).await;

        match &result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 35);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(result.unwrap_err().is_retryable());
        assert!(!dir.path().join("data.bin.part").exists());
        assert!(!dir.path().join("data.bin").exists());
        let records = server.await.unwrap();
        assert_eq!(records.len(), 2, "{records:#?}");
        assert_eq!(records[1].range_offset(), Some(35));
    }

    // -- 416 shortcut corner cases, second round (reviewer findings) --

    /// A 200 or 206 head carrying extra raw header lines.
    async fn send_head_with(
        stream: &mut tokio::net::TcpStream,
        request: &str,
        promised_total: u64,
        extra_headers: &str,
    ) -> u64 {
        use tokio::io::AsyncWriteExt;
        let offset = range_offset_of(request);
        let head = match offset {
            Some(offset) => format!(
                "HTTP/1.1 206 Partial Content\r\n\
                 content-length: {}\r\n\
                 content-range: bytes {offset}-{}/{promised_total}\r\n\
                 {extra_headers}\
                 connection: close\r\n\
                 \r\n",
                promised_total - offset,
                promised_total - 1,
            ),
            None => format!(
                "HTTP/1.1 200 OK\r\n\
                 content-length: {promised_total}\r\n\
                 accept-ranges: bytes\r\n\
                 {extra_headers}\
                 connection: close\r\n\
                 \r\n"
            ),
        };
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        offset.unwrap_or(0)
    }

    /// A 416 with the given total and extra raw header lines.
    fn range_not_satisfiable_with(total: u64, extra_headers: &'static str) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                assert!(range_offset_of(&request).is_some(), "{request}");
                let head = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                     content-length: 0\r\n\
                     content-range: bytes */{total}\r\n\
                     {extra_headers}\
                     connection: close\r\n\
                     \r\n"
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.flush().await;
            })
        })
    }

    /// Promise one byte more than `body`, send `body` from the requested
    /// offset, close: the client has every byte and a stream error.
    fn over_promising(body: Vec<u8>, extra_headers: &'static str) -> ConnHandler {
        Box::new(move |mut stream, request| {
            Box::pin(async move {
                let offset =
                    send_head_with(&mut stream, &request, body.len() as u64 + 1, extra_headers)
                        .await as usize;
                send_all(&mut stream, &body[offset..]).await;
            })
        })
    }

    fn http_date(secs: u64) -> String {
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn mtime_secs(path: &Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// `--no-timestamps` leaves the renamed file with the `.part`'s own
    /// mtime, ignoring both the 416's Last-Modified and the metadata mtime.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_honors_no_timestamps() {
        use wiremock::matchers::header_exists;
        let mock_server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let part_path = dir.path().join("disk.img.part");
        std::fs::write(&part_path, vec![b'A'; 32]).unwrap();
        filetime::set_file_mtime(
            &part_path,
            filetime::FileTime::from_unix_time(1_500_000_000, 0),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(416)
                    .insert_header("Content-Range", "bytes */32")
                    .insert_header("Last-Modified", http_date(1_600_000_000).as_str()),
            )
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let file = test_file_meta("disk.img", 32); // metadata mtime 1700000000
        let opts = DownloadOpts {
            no_timestamps: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(mtime_secs(&dir.path().join("disk.img")), 1_500_000_000);
    }

    /// Every progress payload on the shortcut with `--checksum` and an md5:
    /// Starting at the resume offset, Verifying while the `.part` is hashed
    /// (from zero), Verifying again before the rename, Complete. No
    /// Downloading, since nothing streamed.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_progress_payloads() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let file = test_file_meta_with_md5("disk.img", 32, &md5_hex(&[b'A'; 32]));
        let events: Arc<std::sync::Mutex<Vec<(DownloadStatus, u64, Option<u64>)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let cb: Arc<dyn Fn(DownloadProgress) + Send + Sync> = Arc::new(move |p| {
            if let Ok(mut v) = captured.lock() {
                assert_eq!(p.identifier, "test-item");
                assert_eq!(p.file_name, "disk.img");
                v.push((p.status, p.bytes_downloaded, p.total_bytes));
            }
        });
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        download_file(&client, "test-item", &file, dir.path(), &opts, Some(&*cb))
            .await
            .unwrap();

        let seen = events.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                (DownloadStatus::Starting, 32, Some(32)),
                (DownloadStatus::Verifying, 0, Some(32)),
                (DownloadStatus::Verifying, 32, Some(32)),
                (DownloadStatus::Complete, 32, Some(32)),
            ],
            "{seen:?}"
        );
    }

    /// `--dry-run` returns before the `.part` is even looked at: no request,
    /// the `.part` untouched.
    #[tokio::test]
    async fn dry_run_with_full_length_part_sends_nothing() {
        let mock_server = MockServer::start().await; // no mocks: any request 404s
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("disk.img.part"), vec![b'A'; 32]).unwrap();
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let file = test_file_meta("disk.img", 32);
        let opts = DownloadOpts {
            dry_run: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert_eq!(
            result.status,
            DownloadStatus::Skipped("dry run".to_string())
        );
        assert_eq!(result.bytes, 32);
        assert_eq!(
            std::fs::read(dir.path().join("disk.img.part"))
                .unwrap()
                .len(),
            32
        );
        assert!(!dir.path().join("disk.img").exists());
        assert!(mock_server.received_requests().await.unwrap().is_empty());
    }

    /// When the rename itself fails, the error surfaces and the `.part` is
    /// left where it was: the bytes are not lost to a permissions problem.
    #[cfg(unix)]
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_rename_failure_keeps_part() {
        use std::os::unix::fs::PermissionsExt;
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let file = test_file_meta("disk.img", 32);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        // Restore before asserting so the tempdir can be cleaned up even if
        // an assertion fails.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        match &result {
            Err(IaError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
            other => panic!("expected Io(PermissionDenied), got {other:?}"),
        }
        assert!(!result.unwrap_err().is_retryable());
        assert_eq!(
            std::fs::read(dir.path().join("disk.img.part"))
                .unwrap()
                .len(),
            32
        );
        assert!(!dir.path().join("disk.img").exists());
    }

    /// A symlink `.part` whose target is longer than the file. Since #25 the
    /// link is removed before the resume offset is read, so no Range goes
    /// out at all: a plain GET completes the file and the target is never
    /// touched.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_part_longer_than_the_file_is_removed_and_the_file_downloaded() {
        use wiremock::matchers::header_exists;
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(ResponseTemplate::new(416).insert_header("Content-Range", "bytes */32"))
            .mount(&mock_server)
            .await;
        let full = vec![b'F'; 32];
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full.clone()))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target_file = target_dir.path().join("target.bin");
        std::fs::write(&target_file, vec![b'A'; 40]).unwrap();
        let part_path = dir.path().join("disk.img.part");
        std::os::unix::fs::symlink(&target_file, &part_path).unwrap();
        let file = test_file_meta("disk.img", 32);

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
        assert_eq!(std::fs::read(dir.path().join("disk.img")).unwrap(), full);
        assert!(
            std::fs::symlink_metadata(&part_path).is_err(),
            "link removed"
        );
        assert_eq!(std::fs::read(&target_file).unwrap(), vec![b'A'; 40]);
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
        assert!(requests[0].headers.get("range").is_none());
    }

    /// A `.part` that is a directory: not a regular file, so no Range is
    /// sent, and opening it for writing fails with an I/O error rather than
    /// anything being renamed.
    #[tokio::test]
    async fn directory_part_sends_no_range_and_fails_on_open() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'F'; 32]))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("disk.img.part")).unwrap();
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        assert!(matches!(result, Err(IaError::Io(_))), "got {result:?}");
        assert!(dir.path().join("disk.img.part").is_dir());
        assert!(!dir.path().join("disk.img").exists());
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("range").is_none());
    }

    /// The first response answered a Range request with 200, so the stale
    /// `.part` was truncated and the hasher started fresh. The full body
    /// then arrived with a stream error; the 416 at full length finishes
    /// the file and the md5 is of the new bytes, not the stale ones.
    #[tokio::test]
    async fn stream_retry_response_416_at_full_length_completes_after_resume_reset() {
        let body: Vec<u8> = (0..32u8).collect();
        let body_for_handler = body.clone();
        let ignores_range: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                assert_eq!(range_offset_of(&request), Some(12), "{request}");
                let head = "HTTP/1.1 200 OK\r\n\
                            content-length: 33\r\n\
                            connection: close\r\n\
                            \r\n";
                stream.write_all(head.as_bytes()).await.unwrap();
                send_all(&mut stream, &body_for_handler).await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![ignores_range, range_not_satisfiable_at(32)],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin.part"), vec![b'Z'; 12]).unwrap();
        let mut file = test_file_meta("data.bin", 32);
        file.md5 = Some("b4ffcb23737cec315a4a4d1aa2a620ce".to_string());
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        let records = server.await.unwrap();
        assert_eq!(records[1].range_offset(), Some(32));
    }

    /// Mid-stream: the first response's Last-Modified is kept when the 416
    /// has none.
    #[tokio::test]
    async fn stream_retry_response_416_keeps_first_response_last_modified() {
        let body: Vec<u8> = (0..32u8).collect();
        let (client, server) = spawn_script_server(
            vec![
                over_promising(
                    body.clone(),
                    "last-modified: Sun, 13 Sep 2020 12:26:40 GMT\r\n",
                ),
                range_not_satisfiable_at(32),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", 32); // metadata mtime 1700000000

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(mtime_secs(&dir.path().join("data.bin")), 1_600_000_000);
        server.await.unwrap();
    }

    /// Mid-stream: a Last-Modified on the 416 wins over the first
    /// response's and over the metadata.
    #[tokio::test]
    async fn stream_retry_response_416_last_modified_wins() {
        let body: Vec<u8> = (0..32u8).collect();
        let (client, server) = spawn_script_server(
            vec![
                over_promising(
                    body.clone(),
                    "last-modified: Sun, 13 Sep 2020 12:26:40 GMT\r\n",
                ),
                // 1500000000 = Fri, 14 Jul 2017 02:40:00 GMT
                range_not_satisfiable_with(32, "last-modified: Fri, 14 Jul 2017 02:40:00 GMT\r\n"),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", 32);

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(mtime_secs(&dir.path().join("data.bin")), 1_500_000_000);
        server.await.unwrap();
    }

    /// Two body-stream errors, then a 416 at full length. The hasher is
    /// carried across every re-request, so the md5 of the whole file comes
    /// out right.
    #[tokio::test]
    async fn stream_retry_response_416_after_two_re_requests_keeps_md5() {
        let body: Vec<u8> = (0..32u8).collect();
        let b1 = body.clone();
        let first: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                send_head(&mut stream, &request, 33).await;
                send_all(&mut stream, &b1[..12]).await;
            })
        });
        let b2 = body.clone();
        let second: ConnHandler = Box::new(move |mut stream, request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                assert_eq!(range_offset_of(&request), Some(12), "{request}");
                let head = "HTTP/1.1 206 Partial Content\r\n\
                            content-length: 21\r\n\
                            content-range: bytes 12-31/32\r\n\
                            connection: close\r\n\
                            \r\n";
                stream.write_all(head.as_bytes()).await.unwrap();
                send_all(&mut stream, &b2[12..]).await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![first, second, range_not_satisfiable_at(32)],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut file = test_file_meta("data.bin", 32);
        file.md5 = Some("b4ffcb23737cec315a4a4d1aa2a620ce".to_string());
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        let result = run_download(&client, &file, dir.path(), &opts)
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("data.bin")).unwrap(), body);
        let records = server.await.unwrap();
        assert_eq!(records[1].range_offset(), Some(12));
        assert_eq!(records[2].range_offset(), Some(32));
    }

    /// Mid-stream 416 at full length for a file with no metadata size: the
    /// literal reading has no third party to agree, so the `.part` is
    /// deleted and the retryable restart follows, even though every byte
    /// had arrived. (A clean stream end would have finished it.)
    #[tokio::test]
    async fn stream_retry_response_416_without_metadata_size_restarts() {
        let body: Vec<u8> = (0..32u8).collect();
        let (client, server) = spawn_script_server(
            vec![
                over_promising(body.clone(), ""),
                range_not_satisfiable_at(32),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut file = test_file_meta("data.bin", 32);
        file.size = None;

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default()).await;

        match &result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 32);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(result.unwrap_err().is_retryable());
        assert!(!dir.path().join("data.bin.part").exists());
        assert!(!dir.path().join("data.bin").exists());
        server.await.unwrap();
    }

    /// The `_files.xml` twin of the test above.
    #[tokio::test]
    async fn stream_retry_response_416_on_files_xml_restarts() {
        let body: Vec<u8> = (0..32u8).collect();
        let (client, server) = spawn_script_server(
            vec![
                over_promising(body.clone(), ""),
                range_not_satisfiable_at(32),
            ],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        // run_download uses the identifier "slow-item".
        let file = test_file_meta("slow-item_files.xml", 100);

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default()).await;

        match &result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 32);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(!dir.path().join("slow-item_files.xml.part").exists());
        server.await.unwrap();
    }

    /// Mid-stream 416 without a parseable total: the plain HTTP error, the
    /// flushed bytes kept in `.part`.
    #[tokio::test]
    async fn stream_retry_response_416_without_content_range_is_an_http_error() {
        let body: Vec<u8> = (0..32u8).collect();
        let bare_416: ConnHandler = Box::new(|mut stream, _request| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                let head = "HTTP/1.1 416 Range Not Satisfiable\r\n\
                            content-length: 0\r\n\
                            connection: close\r\n\
                            \r\n";
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.flush().await;
            })
        });
        let (client, server) = spawn_script_server(
            vec![over_promising(body.clone(), ""), bare_416],
            Duration::ZERO,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("data.bin", 32);

        let result = run_download(&client, &file, dir.path(), &DownloadOpts::default()).await;

        assert!(
            matches!(result, Err(IaError::Http { status: 416, .. })),
            "got {result:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("data.bin.part")).unwrap(),
            body
        );
        assert!(!dir.path().join("data.bin").exists());
        server.await.unwrap();
    }

    // -- repeated wrong md5 stops the retries (#14) --

    fn item_with(identifier: &str, file: FileMetadata) -> crate::types::ItemMetadata {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};
        ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single(identifier.to_string())),
                ..Default::default()
            },
            files: vec![file],
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra: HashMap::new(),
        }
    }

    /// Serve `body` for the next `times` requests only. wiremock tries
    /// mocks in mount order and skips one whose budget is spent, so a
    /// sequence of these plays bodies in order.
    async fn mount_times(mock_server: &MockServer, status: u16, body: &[u8], times: u64) {
        Mock::given(method("GET"))
            .and(path("/download/bad-item/disk.img"))
            .respond_with(ResponseTemplate::new(status).set_body_bytes(body.to_vec()))
            .up_to_n_times(times)
            .mount(mock_server)
            .await;
    }

    async fn download_bad_item(
        mock_server: &MockServer,
        dir: &Path,
        md5: &str,
        retries: usize,
    ) -> ItemDownloadResult {
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = item_with("bad-item", test_file_meta_with_md5("disk.img", 16, md5));
        let opts = DownloadOpts {
            destdir: dir.to_path_buf(),
            checksum: true,
            retries,
            ..Default::default()
        };
        download_item_with_metadata(
            &client,
            "bad-item",
            &item,
            &opts,
            Arc::new(Semaphore::new(1)),
            None,
        )
        .await
        .unwrap()
    }

    fn failure_message(result: &ItemDownloadResult) -> String {
        match &result.results[0].status {
            DownloadStatus::Failed(msg) => msg.clone(),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The same bytes twice means the wire is fine and the source or its
    /// metadata is wrong: stop after the second download, keep the copy.
    #[tokio::test]
    async fn same_wrong_md5_twice_stops_retrying() {
        let body_a = vec![b'A'; 16];
        let mock_server = MockServer::start().await;
        mount_times(&mock_server, 200, &body_a, 100).await;
        let dir = tempfile::tempdir().unwrap();

        let result = download_bad_item(&mock_server, dir.path(), WRONG_MD5, 5).await;

        assert_eq!(result.files_failed, 1, "{result:?}");
        let msg = failure_message(&result);
        assert!(msg.contains("twice in a row"), "{msg}");
        assert!(
            msg.contains("source file or its metadata is likely wrong"),
            "{msg}"
        );
        let kept = dir.path().join("bad-item").join("disk.img.md5-mismatch");
        assert!(msg.contains(&kept.display().to_string()), "{msg}");
        assert_eq!(std::fs::read(&kept).unwrap(), body_a);
        assert!(!dir.path().join("bad-item").join("disk.img.part").exists());
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    /// Different wrong bytes each time means the transfer is corrupting
    /// data: keep retrying up to --retries, and keep the latest copy.
    #[tokio::test]
    async fn different_wrong_md5_keeps_retrying() {
        let mock_server = MockServer::start().await;
        mount_times(&mock_server, 200, &[b'A'; 16], 1).await;
        mount_times(&mock_server, 200, &[b'B'; 16], 1).await;
        mount_times(&mock_server, 200, &[b'C'; 16], 1).await;
        let dir = tempfile::tempdir().unwrap();

        let result = download_bad_item(&mock_server, dir.path(), WRONG_MD5, 2).await;

        assert_eq!(result.files_failed, 1, "{result:?}");
        let msg = failure_message(&result);
        assert!(!msg.contains("twice in a row"), "{msg}");
        assert!(msg.contains("checksum mismatch"), "{msg}");
        let kept = dir.path().join("bad-item").join("disk.img.md5-mismatch");
        assert_eq!(std::fs::read(&kept).unwrap(), vec![b'C'; 16]);
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 3);
    }

    /// A corrupt first transfer followed by a good one: the file completes
    /// and the bad copy is removed.
    #[tokio::test]
    async fn different_wrong_md5_then_success_completes_and_removes_kept_copy() {
        let good = vec![b'G'; 16];
        let mock_server = MockServer::start().await;
        mount_times(&mock_server, 200, &[b'A'; 16], 1).await;
        mount_times(&mock_server, 200, &good, 1).await;
        let dir = tempfile::tempdir().unwrap();

        let result = download_bad_item(&mock_server, dir.path(), &md5_hex(&good), 5).await;

        assert_eq!(result.files_downloaded, 1, "{result:?}");
        assert_eq!(result.files_failed, 0);
        let item_dir = dir.path().join("bad-item");
        assert_eq!(std::fs::read(item_dir.join("disk.img")).unwrap(), good);
        assert!(!item_dir.join("disk.img.md5-mismatch").exists());
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    /// --retries 0: the first mismatch is final; the copy is kept.
    #[tokio::test]
    async fn retries_zero_keeps_the_mismatch_and_stops() {
        let body_a = vec![b'A'; 16];
        let mock_server = MockServer::start().await;
        mount_times(&mock_server, 200, &body_a, 100).await;
        let dir = tempfile::tempdir().unwrap();

        let result = download_bad_item(&mock_server, dir.path(), WRONG_MD5, 0).await;

        assert_eq!(result.files_failed, 1, "{result:?}");
        let msg = failure_message(&result);
        assert!(!msg.contains("twice in a row"), "{msg}");
        let kept = dir.path().join("bad-item").join("disk.img.md5-mismatch");
        assert_eq!(std::fs::read(&kept).unwrap(), body_a);
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    /// "Twice in a row" means consecutive. A different error in between
    /// (here a 500) forgets the first mismatch, so the next mismatch is a
    /// first one again; the one after that is the repeat and stops the
    /// loop. Attempts: A (mismatch), 500 (reset), A (first again), A
    /// (repeat): four requests, with a budget that would have allowed five.
    /// The download transport has no retry middleware, so the 500 reaches
    /// the per-file loop once and the count is exact.
    #[tokio::test]
    async fn a_different_error_between_mismatches_resets_the_repeat_check() {
        let body_a = vec![b'A'; 16];
        let mock_server = MockServer::start().await;
        mount_times(&mock_server, 200, &body_a, 1).await;
        mount_times(&mock_server, 500, b"boom", 1).await;
        mount_times(&mock_server, 200, &body_a, 100).await;
        let dir = tempfile::tempdir().unwrap();

        let result = download_bad_item(&mock_server, dir.path(), WRONG_MD5, 4).await;

        assert_eq!(result.files_failed, 1, "{result:?}");
        let msg = failure_message(&result);
        assert!(msg.contains("twice in a row"), "{msg}");
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 4);
    }

    /// All download requests must send `cnt=0` to suppress the archive.org
    /// view-counter. Verified by gating the wiremock response on the
    /// `query_param("cnt", "0")` matcher — if the param is missing the
    /// request returns 404 and the test fails.
    #[tokio::test]
    async fn download_sends_cnt_zero_query_param() {
        use wiremock::matchers::query_param;

        let mock_server = MockServer::start().await;
        let body = b"counted? no.";

        Mock::given(method("GET"))
            .and(path("/download/test-item/test.txt"))
            .and(query_param("cnt", "0"))
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
        .expect("download should succeed when cnt=0 is sent");

        assert_eq!(result.status, DownloadStatus::Complete);
    }

    #[test]
    fn ensure_cnt_zero_appends_when_no_query() {
        assert_eq!(
            ensure_cnt_zero("https://archive.org/download/item/file.txt"),
            "https://archive.org/download/item/file.txt?cnt=0"
        );
    }

    #[test]
    fn ensure_cnt_zero_appends_when_query_present() {
        assert_eq!(
            ensure_cnt_zero("https://archive.org/download/item/file.txt?foo=bar"),
            "https://archive.org/download/item/file.txt?foo=bar&cnt=0"
        );
    }

    #[test]
    fn ensure_cnt_zero_does_not_duplicate() {
        let already = "https://archive.org/download/item/file.txt?cnt=0";
        assert_eq!(ensure_cnt_zero(already), already);
        let already_mid = "https://archive.org/download/item/file.txt?foo=bar&cnt=0";
        assert_eq!(ensure_cnt_zero(already_mid), already_mid);
    }

    /// Zip member converted URLs use the quirky `…/file.jp2&ext=jpg` form
    /// (an `&ext=` segment without a preceding `?`). We treat that as
    /// "no query string" and append `?cnt=0`, preserving the existing
    /// `&ext=` segment in the path.
    #[test]
    fn ensure_cnt_zero_with_ia_ext_quirk() {
        assert_eq!(
            ensure_cnt_zero("https://archive.org/download/item/zip.zip/path/file.jp2&ext=jpg"),
            "https://archive.org/download/item/zip.zip/path/file.jp2&ext=jpg?cnt=0"
        );
    }

    /// `count_views: true` must omit the `cnt` parameter entirely so
    /// archive.org records the view. (Sending `cnt=1` would still
    /// suppress counting — the counter is gated on *absence* of `cnt`.)
    #[tokio::test]
    async fn download_with_count_views_omits_cnt_param() {
        use wiremock::matchers::query_param_is_missing;

        let mock_server = MockServer::start().await;
        let body = b"counted!";

        // Match only when `cnt` is absent from the query string. Any cnt
        // value (including cnt=0) means our opt-out is broken.
        Mock::given(method("GET"))
            .and(path("/download/test-item/test.txt"))
            .and(query_param_is_missing("cnt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test.txt", body.len() as u64);

        let opts = DownloadOpts {
            count_views: true,
            ..Default::default()
        };
        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .expect("download with count_views=true should omit cnt entirely");
        assert_eq!(result.status, DownloadStatus::Complete);
    }

    // -- Content-Range parsing and the _files.xml exemption (#12) --

    #[test]
    fn content_range_total_parses_normal_form() {
        assert_eq!(parse_content_range_total("bytes 13-21/22"), Some(22));
        assert_eq!(parse_content_range_total("bytes 0-0/1"), Some(1));
    }

    #[test]
    fn content_range_total_accepts_unsatisfied_range_form() {
        assert_eq!(parse_content_range_total("bytes */22"), Some(22));
    }

    #[test]
    fn content_range_total_is_none_when_server_does_not_know_it() {
        assert_eq!(parse_content_range_total("bytes 13-21/*"), None);
    }

    #[test]
    fn content_range_total_unit_is_case_insensitive() {
        assert_eq!(parse_content_range_total("BYTES 13-21/22"), Some(22));
        assert_eq!(parse_content_range_total("  bytes 13-21/22  "), Some(22));
    }

    #[test]
    fn content_range_total_rejects_other_units_and_garbage() {
        assert_eq!(parse_content_range_total("items 1-2/3"), None);
        assert_eq!(parse_content_range_total("bytes 13-21"), None);
        assert_eq!(parse_content_range_total(""), None);
        assert_eq!(parse_content_range_total("bytes 1-2/abc"), None);
        assert_eq!(parse_content_range_total("bytes 1-2/"), None);
        assert_eq!(parse_content_range_total("bytes */*"), None);
    }

    #[test]
    fn content_range_total_accepts_zero() {
        assert_eq!(parse_content_range_total("bytes */0"), Some(0));
    }

    #[test]
    fn files_xml_size_is_unknowable() {
        assert!(is_size_unknowable("abc", "abc_files.xml"));
    }

    #[test]
    fn other_files_have_knowable_sizes() {
        assert!(!is_size_unknowable("abc", "abc_meta.xml"));
        assert!(!is_size_unknowable("abc", "other_files.xml"));
        assert!(!is_size_unknowable("abc", "sub/abc_files.xml"));
        assert!(!is_size_unknowable("abc", "abc_files.xml.bak"));
    }

    // -- Content-Range check on 206 responses (#12) --

    /// Put a 5-byte `.part` on disk and mount a 206 mock for the Range
    /// request that follows. Returns the client and the destination dir.
    async fn mount_resume_206(
        mock_server: &MockServer,
        name: &str,
        content_range: &str,
    ) -> (IaClient, tempfile::TempDir) {
        use wiremock::matchers::header_exists;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{name}.part")), b"AAAAA").unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/download/test-item/{name}")))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(vec![b'B'; 25])
                    .insert_header("Content-Range", content_range),
            )
            .mount(mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        (client, dir)
    }

    #[tokio::test]
    async fn content_range_total_mismatch_fails_permanently() {
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_resume_206(&mock_server, "disk.img", "bytes 5-29/30").await;
        // Metadata claims 40 bytes; the server says the file is 30.
        let file = test_file_meta("disk.img", 40);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 40);
                assert_eq!(server_size, 30);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        // Nothing was written: .part is untouched and no final file exists.
        let part = std::fs::read(dir.path().join("disk.img.part")).unwrap();
        assert_eq!(part, b"AAAAA");
        assert!(!dir.path().join("disk.img").exists());
    }

    #[tokio::test]
    async fn content_range_star_total_is_ignored() {
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_resume_206(&mock_server, "disk.img", "bytes 5-29/*").await;
        let file = test_file_meta("disk.img", 30);

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
        let got = std::fs::read(dir.path().join("disk.img")).unwrap();
        assert_eq!(got.len(), 30);
        assert_eq!(&got[..5], b"AAAAA");
    }

    #[tokio::test]
    async fn content_range_mismatch_on_files_xml_is_ignored() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_206(&mock_server, "test-item_files.xml", "bytes 5-29/30").await;
        // Metadata size is wrong by construction for _files.xml.
        let file = test_file_meta("test-item_files.xml", 40);

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
        assert_eq!(result.bytes, 30);
    }

    // -- post-stream byte-count check (#12) --

    /// Mount the two responses a short-then-resumed download sees: a 200
    /// carrying only the first 20 of 32 bytes, and a 206 for the Range
    /// request that follows with the remaining 12. wiremock tries mocks in
    /// mount order, so the Range-only mock goes first.
    async fn mount_short_then_range(mock_server: &MockServer, item: &str, name: &str) -> Vec<u8> {
        use wiremock::matchers::header_exists;
        let full: Vec<u8> = (0..32u8).collect();
        Mock::given(method("GET"))
            .and(path(format!("/download/{item}/{name}")))
            .and(header_exists("Range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(full[20..].to_vec())
                    .insert_header("Content-Range", "bytes 20-31/32"),
            )
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/download/{item}/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full[..20].to_vec()))
            .mount(mock_server)
            .await;
        full
    }

    // -- 416 on a resume Range request (#12 follow-up) --

    /// Put a `.part` of `part_len` bytes on disk and answer the Range
    /// request that follows with 416, carrying `content_range` if given.
    async fn mount_resume_416(
        mock_server: &MockServer,
        name: &str,
        part_len: usize,
        content_range: Option<&str>,
    ) -> (IaClient, tempfile::TempDir) {
        use wiremock::matchers::header_exists;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("{name}.part")),
            vec![b'A'; part_len],
        )
        .unwrap();
        let mut template = ResponseTemplate::new(416);
        if let Some(content_range) = content_range {
            template = template.insert_header("Content-Range", content_range);
        }
        Mock::given(method("GET"))
            .and(path(format!("/download/test-item/{name}")))
            .and(header_exists("Range"))
            .respond_with(template)
            .mount(mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        (client, dir)
    }

    #[tokio::test]
    async fn range_not_satisfiable_with_different_total_fails_permanently() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 40, Some("bytes */30")).await;
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::ServerSizeMismatch {
                metadata_size,
                server_size,
                ..
            }) => {
                assert_eq!(metadata_size, 32);
                assert_eq!(server_size, 30);
            }
            other => panic!("expected ServerSizeMismatch, got {other:?}"),
        }
        // Server and metadata disagree; nothing is written and the .part is
        // left alone, as with the Content-Range check on a 206.
        let part = std::fs::read(dir.path().join("disk.img.part")).unwrap();
        assert_eq!(part.len(), 40);
        assert!(!dir.path().join("disk.img").exists());
    }

    #[tokio::test]
    async fn range_not_satisfiable_at_metadata_total_deletes_part_and_restarts() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 40, Some("bytes */32")).await;
        // A plain GET (no Range) gets the whole file.
        let full = vec![b'F'; 32];
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full.clone()))
            .mount(&mock_server)
            .await;
        let file = test_file_meta("disk.img", 32);

        let first = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        // The .part already holds at least the whole file. It is removed and
        // the error is retryable so the next attempt starts from byte 0.
        match &first {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 40);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(first.unwrap_err().is_retryable());
        assert!(!dir.path().join("disk.img.part").exists());
        assert!(!dir.path().join("disk.img").exists());

        let second = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.status, DownloadStatus::Complete);
        assert_eq!(std::fs::read(dir.path().join("disk.img")).unwrap(), full);

        // Exactly two requests: the Range request that drew the 416 and the
        // plain GET that followed.
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "{requests:#?}");
        assert!(requests[0].headers.get("range").is_some());
        assert!(requests[1].headers.get("range").is_none());
    }

    /// End to end through the outer retry loop: the 416 at the metadata
    /// total deletes `.part`, the retryable error earns a second attempt,
    /// and the plain GET that follows completes the file.
    #[tokio::test]
    async fn range_not_satisfiable_at_metadata_total_recovers_through_outer_retry_loop() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};
        use wiremock::matchers::header_exists;

        let mock_server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let item_dir = dir.path().join("stale-item");
        std::fs::create_dir_all(&item_dir).unwrap();
        std::fs::write(item_dir.join("disk.img.part"), vec![b'A'; 40]).unwrap();
        Mock::given(method("GET"))
            .and(path("/download/stale-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(ResponseTemplate::new(416).insert_header("Content-Range", "bytes */32"))
            .expect(1)
            .mount(&mock_server)
            .await;
        let full = vec![b'F'; 32];
        Mock::given(method("GET"))
            .and(path("/download/stale-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full.clone()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("stale-item".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("disk.img", 32)],
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
        let opts = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            retries: 1,
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(1));

        let result =
            download_item_with_metadata(&client, "stale-item", &item, &opts, semaphore, None)
                .await
                .unwrap();

        assert_eq!(result.files_downloaded, 1, "{result:?}");
        assert_eq!(result.files_failed, 0);
        assert_eq!(std::fs::read(item_dir.join("disk.img")).unwrap(), full);
        assert!(!item_dir.join("disk.img.part").exists());
    }

    #[tokio::test]
    async fn range_not_satisfiable_on_files_xml_deletes_part() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "test-item_files.xml", 40, Some("bytes */30")).await;
        // Metadata size is wrong by construction for _files.xml, so the
        // server's total is the only length that counts.
        let file = test_file_meta("test-item_files.xml", 100);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(expected, 30);
                assert_eq!(received, 40);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(!dir.path().join("test-item_files.xml.part").exists());
    }

    #[tokio::test]
    async fn range_not_satisfiable_without_content_range_is_an_http_error() {
        let mock_server = MockServer::start().await;
        let (client, dir) = mount_resume_416(&mock_server, "disk.img", 40, None).await;
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        // Without a total there is nothing to compare; today's behavior.
        assert!(
            matches!(result, Err(IaError::Http { status: 416, .. })),
            "got {result:?}"
        );
        assert!(dir.path().join("disk.img.part").exists());
    }

    /// Callers that send no Range header (scandata, zip listing) must keep
    /// seeing a 416 as the plain HTTP error it always was.
    #[tokio::test]
    async fn fetch_response_416_without_range_is_an_http_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(416).insert_header("Content-Range", "bytes */30"))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let url = client.url("/download/test-item/disk.img");

        let result = fetch_response(&client, &url, None, false).await;

        assert!(
            matches!(result, Err(IaError::Http { status: 416, .. })),
            "got {result:?}"
        );
    }

    // -- 416 at the file's full length finishes the file (#12 follow-up) --

    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_completes_without_redownload() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let file = test_file_meta("disk.img", 32);

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

        // .part length, 416 total, and metadata size all agree: the .part is
        // the whole file and is renamed into place without another request.
        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, 32);
        assert_eq!(
            std::fs::read(dir.path().join("disk.img")).unwrap(),
            vec![b'A'; 32]
        );
        assert!(!dir.path().join("disk.img.part").exists());

        // The mtime comes from the item metadata, as a 416 carries no
        // Last-Modified here.
        let mtime = std::fs::metadata(dir.path().join("disk.img"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime, 1700000000);

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
        assert!(requests[0].headers.get("range").is_some());
    }

    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_with_checksum_verifies() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let file = test_file_meta_with_md5("disk.img", 32, &md5_hex(&[b'A'; 32]));

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

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, Some(&*cb))
            .await
            .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, 32);
        assert_eq!(
            std::fs::read(dir.path().join("disk.img")).unwrap(),
            vec![b'A'; 32]
        );
        assert!(!dir.path().join("disk.img.part").exists());

        // The .part was hashed on disk, so the UI saw a Verifying event
        // before Complete.
        let seen = events.lock().unwrap();
        let verifying_idx = seen.iter().position(|s| *s == DownloadStatus::Verifying);
        let complete_idx = seen.iter().position(|s| *s == DownloadStatus::Complete);
        assert!(verifying_idx.is_some(), "no Verifying event: {seen:?}");
        assert!(complete_idx.is_some(), "no Complete event: {seen:?}");
        assert!(verifying_idx < complete_idx, "{seen:?}");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
    }

    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_with_checksum_mismatch_keeps_md5_mismatch() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let file = test_file_meta_with_md5("disk.img", 32, "00000000000000000000000000000000");
        let opts = DownloadOpts {
            checksum: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None).await;

        // The .part has the right length but the wrong bytes. The md5
        // compare catches it exactly as it would after a stream, and keeps
        // the bytes as .md5-mismatch (#14).
        assert!(
            matches!(result, Err(IaError::ChecksumMismatch { .. })),
            "got {result:?}"
        );
        assert!(!dir.path().join("disk.img.part").exists());
        assert!(!dir.path().join("disk.img").exists());
        assert_eq!(
            std::fs::read(dir.path().join("disk.img.md5-mismatch")).unwrap(),
            vec![b'A'; 32]
        );

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
    }

    /// A symlink `.part` whose target has the agreed length never reaches
    /// the 416 shortcut: the link is removed before the resume offset is
    /// read (#25), so a plain GET goes out and completes on the first
    /// attempt. The 416 mock here would only answer a Range request.
    #[cfg(unix)]
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_through_symlink_part_never_reaches_the_shortcut()
    {
        use wiremock::matchers::header_exists;
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .and(header_exists("Range"))
            .respond_with(ResponseTemplate::new(416).insert_header("Content-Range", "bytes */32"))
            .mount(&mock_server)
            .await;
        let full = vec![b'F'; 32];
        Mock::given(method("GET"))
            .and(path("/download/test-item/disk.img"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(full.clone()))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target_file = target_dir.path().join("target.bin");
        std::fs::write(&target_file, vec![b'A'; 32]).unwrap();
        let target_mtime = std::fs::metadata(&target_file).unwrap().modified().unwrap();
        let part_path = dir.path().join("disk.img.part");
        std::os::unix::fs::symlink(&target_file, &part_path).unwrap();
        let file = test_file_meta("disk.img", 32);

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
        let final_meta = std::fs::symlink_metadata(dir.path().join("disk.img")).unwrap();
        assert!(!final_meta.file_type().is_symlink());
        assert_eq!(std::fs::read(dir.path().join("disk.img")).unwrap(), full);
        assert!(
            std::fs::symlink_metadata(&part_path).is_err(),
            "link removed"
        );
        // The target is untouched: same bytes, same mtime.
        assert_eq!(std::fs::read(&target_file).unwrap(), vec![b'A'; 32]);
        assert_eq!(
            std::fs::metadata(&target_file).unwrap().modified().unwrap(),
            target_mtime
        );
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{requests:#?}");
        assert!(requests[0].headers.get("range").is_none());
    }

    /// With no metadata size there is no third party to agree, so the
    /// shortcut does not apply: the .part is deleted and the download
    /// restarts from byte 0, as PR #26 left it.
    #[tokio::test]
    async fn range_not_satisfiable_at_part_length_without_metadata_size_restarts() {
        let mock_server = MockServer::start().await;
        let (client, dir) =
            mount_resume_416(&mock_server, "disk.img", 32, Some("bytes */32")).await;
        let mut file = test_file_meta("disk.img", 32);
        file.size = None;

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        match &result {
            Err(IaError::DownloadSizeMismatch {
                expected, received, ..
            }) => {
                assert_eq!(*expected, 32);
                assert_eq!(*received, 32);
            }
            other => panic!("expected DownloadSizeMismatch, got {other:?}"),
        }
        assert!(result.unwrap_err().is_retryable());
        assert!(!dir.path().join("disk.img.part").exists());
        assert!(!dir.path().join("disk.img").exists());
    }

    #[tokio::test]
    async fn short_body_keeps_part_and_returns_retryable_error() {
        let mock_server = MockServer::start().await;
        let full = mount_short_then_range(&mock_server, "test-item", "disk.img").await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;

        let err = match result {
            Err(e) => e,
            Ok(r) => panic!("expected an error, got {r:?}"),
        };
        assert!(
            matches!(
                err,
                IaError::DownloadSizeMismatch {
                    expected: 32,
                    received: 20,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert!(err.is_retryable());
        let part = std::fs::read(dir.path().join("disk.img.part")).unwrap();
        assert_eq!(part, &full[..20]);
        assert!(!dir.path().join("disk.img").exists());
    }

    #[tokio::test]
    async fn short_body_then_rerun_resumes_and_completes() {
        let mock_server = MockServer::start().await;
        let full = mount_short_then_range(&mock_server, "test-item", "disk.img").await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("disk.img", 32);

        let first = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await;
        assert!(matches!(first, Err(IaError::DownloadSizeMismatch { .. })));

        let second = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(second.status, DownloadStatus::Complete);
        assert_eq!(second.bytes, 32);
        let got = std::fs::read(dir.path().join("disk.img")).unwrap();
        assert_eq!(got, full);
        assert!(!dir.path().join("disk.img.part").exists());

        // Exactly two requests: the short 200 and one Range request.
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "{requests:#?}");
        let range = requests[1]
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(range, "bytes=20-");
    }

    #[tokio::test]
    async fn short_body_recovers_through_outer_retry_loop() {
        use crate::types::{ItemMetadata, MetadataFields, MetadataValue};

        let mock_server = MockServer::start().await;
        let full = mount_short_then_range(&mock_server, "short-item", "disk.img").await;

        let item = ItemMetadata {
            metadata: MetadataFields {
                identifier: Some(MetadataValue::Single("short-item".to_string())),
                ..Default::default()
            },
            files: vec![test_file_meta("disk.img", 32)],
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
            retries: 1,
            ..Default::default()
        };
        let semaphore = Arc::new(Semaphore::new(1));

        let result =
            download_item_with_metadata(&client, "short-item", &item, &opts, semaphore, None)
                .await
                .unwrap();

        assert_eq!(result.files_downloaded, 1, "{result:?}");
        assert_eq!(result.files_failed, 0);
        let got = std::fs::read(dir.path().join("short-item/disk.img")).unwrap();
        assert_eq!(got, full);
    }

    #[tokio::test]
    async fn short_body_with_checksum_keeps_part() {
        let mock_server = MockServer::start().await;
        mount_short_then_range(&mock_server, "test-item", "disk.img").await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        // test_file_meta carries an md5 that cannot match 20 bytes of data.
        let file = test_file_meta("disk.img", 32);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts {
                checksum: true,
                ..Default::default()
            },
            None,
        )
        .await;

        // The size check runs before the md5 check, which would have deleted
        // the .part file.
        assert!(
            matches!(result, Err(IaError::DownloadSizeMismatch { .. })),
            "got {result:?}"
        );
        assert!(dir.path().join("disk.img.part").exists());
    }

    #[tokio::test]
    async fn file_without_size_is_unaffected() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/nosize.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'Z'; 20]))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut file = test_file_meta("nosize.bin", 0);
        file.size = None;

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
        assert_eq!(result.bytes, 20);
    }

    #[tokio::test]
    async fn files_xml_size_mismatch_is_ignored() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/test-item_files.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'<'; 20]))
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test-item_files.xml", 100);

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
        assert_eq!(result.bytes, 20);
    }
}
