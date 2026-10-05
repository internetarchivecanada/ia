use crate::error::{format_error_chain, IaError, Result};
use crate::upload::check_limit::{is_spam_response, parse_check_limit_response};
use crate::upload::checksum::{compute_file_md5_async, hash_file_and_parts_async, FileHashes};
use crate::upload::headers::encode_metadata_headers;
use crate::upload::s3_error::{describe_parsed, parse_s3_error, strip_xml};
use crate::upload::stall_watch::{watch_send, BodyWatch, SendEnd};
use crate::upload::types::*;
use crate::IaClient;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Upload a single file to an IA S3 bucket.
///
/// This is the core upload function that PUTs a single file to Internet Archive's
/// S3-like API. It handles:
/// - `Content-Length` (always set; IA S3 does NOT support chunked transfer)
/// - `Expect: 100-continue` for early rejection detection
/// - `Content-MD5` verification when `opts.verify` is true
/// - Retry with check_limit polling on 503 responses
/// - Spam detection for permanent 503 rejections
/// - Dry-run mode (validates everything, sends no HTTP)
///
/// # Parameters
///
/// - `is_first_file`: controls `x-archive-auto-make-bucket` and metadata headers
/// - `is_last_file`: controls `x-archive-queue-derive` (triggers derive on last file)
/// - `size_hint`: sent as `x-archive-size-hint` (typically total upload size, first file only)
/// - `progress`: optional callback for upload progress reporting
///
/// # Errors
///
/// Returns `IaError::SpamDetected` on permanent spam rejection (503 with spam message).
/// Returns `IaError::UploadFailed` after exhausting retries or on non-retryable HTTP errors.
/// Returns `IaError::Auth` if S3 credentials are not configured.
/// Returns `IaError::CheckLimitFailed` if rate-limit polling exhausts retries.
#[allow(clippy::too_many_arguments)]
pub async fn upload_file(
    client: &IaClient,
    identifier: &str,
    file: &Path,
    key: &str,
    opts: &UploadOpts,
    is_first_file: bool,
    is_last_file: bool,
    size_hint: Option<u64>,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
) -> Result<UploadResult> {
    let start = Instant::now();
    let file_size = tokio::fs::metadata(file).await?.len();

    // One read of the file when either the skip check or verification
    // needs its md5 (`--clobber --no-verify` reads nothing). For a
    // multipart upload the same pass yields every part's md5, which a
    // resume compares against the parts IA holds (#19, #20). A md5 from
    // --checksums is taken as given, with no part md5s; a resume then
    // hashes the file itself.
    let needs_md5 = opts.checksum || opts.verify;
    let hashes: Option<FileHashes> = if needs_md5 {
        if let Some(md5) = opts.checksum_file.as_ref().and_then(|cs| cs.get(key)) {
            Some(FileHashes {
                md5: md5.clone(),
                parts: Vec::new(),
                size: file_size,
            })
        } else {
            if let Some(ref cb) = progress {
                cb(UploadProgress {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    bytes_sent: 0,
                    total_bytes: file_size,
                    status: UploadProgressStatus::Verifying,
                });
            }
            if opts.multipart {
                Some(
                    hash_file_and_parts_async(file, crate::upload::multipart::DEFAULT_PART_SIZE)
                        .await?,
                )
            } else {
                Some(FileHashes {
                    md5: compute_file_md5_async(file).await?,
                    parts: Vec::new(),
                    size: file_size,
                })
            }
        }
    } else {
        None
    };
    let md5_hex = hashes.as_ref().map(|h| h.md5.clone());

    // Checksum skip: compare local MD5 with remote, skip if match
    if opts.checksum {
        let local_md5 = md5_hex.as_deref().ok_or_else(|| {
            IaError::Config("internal error: MD5 not computed for checksum skip".into())
        })?;
        match client.get_item(identifier).await {
            Ok(item) => {
                let remote_md5 = item
                    .files
                    .iter()
                    .find(|f| f.name == key)
                    .and_then(|f| f.md5.as_deref());

                if remote_md5 == Some(local_md5) {
                    if let Some(ref cb) = progress {
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
                        md5: Some(local_md5.to_string()),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        retries: 0,
                    });
                }
            }
            Err(e) => {
                // Item doesn't exist yet or metadata fetch failed — upload normally
                tracing::debug!("checksum skip: metadata fetch failed for {identifier}: {e}");
            }
        }
    }

    // Dry run: validate everything but don't upload
    // Multipart from here: the skip check above applies to both paths.
    if opts.multipart {
        return crate::upload::multipart::upload_file_multipart(
            client,
            identifier,
            file,
            key,
            opts,
            crate::upload::multipart::DEFAULT_PART_SIZE,
            is_first_file,
            is_last_file,
            size_hint,
            progress,
            hashes.as_ref(),
        )
        .await;
    }

    if opts.dry_run {
        return Ok(UploadResult {
            identifier: identifier.to_string(),
            key: key.to_string(),
            status: UploadStatus::DryRun,
            bytes: file_size,
            md5: md5_hex,
            elapsed_ms: start.elapsed().as_millis() as u64,
            retries: 0,
        });
    }

    // Build S3 URL
    let s3_url = build_s3_url(client, identifier, key);

    // Get auth
    let (access, secret) = client.require_auth()?;
    let auth_header = format!("LOW {access}:{secret}");

    // Build Content-MD5 header only if verify is on
    let content_md5_b64 = if opts.verify {
        md5_hex.as_ref().map(|hex| {
            let raw_bytes = hex_to_bytes(hex);
            base64_encode(&raw_bytes)
        })
    } else {
        None
    };

    // Pre-compute metadata headers (only used on first file)
    let metadata_headers = if is_first_file && !opts.metadata.is_empty() {
        encode_metadata_headers(&opts.metadata)?
    } else {
        Vec::new()
    };

    // Retry loop. Waits follow the standard schedule (see
    // `retry::backoff_policy`); a Retry-After header on the failed response
    // sets the wait instead.
    let backoff = opts.backoff();
    let mut retries = 0u32;
    let mut last_was_503 = false;
    let mut retry_after: Option<std::time::Duration> = None;
    // Stalled sends (#38) and unanswered bodies (#40): re-sent at once,
    // counted for the final error.
    let mut stalls: usize = 0;
    let mut unanswered: usize = 0;
    let mut last_was_stall = false;
    let window_secs = crate::stall::policy().0.as_secs();
    let response_secs = crate::stall::response_wait().as_secs();
    loop {
        // On retry: after a 503, wait out any Retry-After and then poll
        // check_limit until the rate limit clears; after a stall, re-send
        // at once (the problem is the peer, not load); otherwise back off.
        if retries > 0 && last_was_stall {
            last_was_stall = false;
        } else if retries > 0 {
            if last_was_503 {
                if retry_after.is_some() {
                    let wait = super::retry::wait_before_retry(retry_after, &backoff, retries - 1);
                    tracing::debug!(
                        identifier,
                        key,
                        wait_ms = wait.as_millis() as u64,
                        "honoring Retry-After before polling check_limit"
                    );
                    // The poll reports this status too, but only once it
                    // starts; the UI must not sit on "uploading" meanwhile.
                    if let Some(ref cb) = progress {
                        cb(UploadProgress {
                            identifier: identifier.to_string(),
                            key: key.to_string(),
                            bytes_sent: 0,
                            total_bytes: file_size,
                            status: UploadProgressStatus::WaitingRateLimit,
                        });
                    }
                    tokio::time::sleep(wait).await;
                }
                poll_check_limit(
                    client,
                    identifier,
                    key,
                    file_size,
                    opts,
                    &backoff,
                    progress.clone(),
                )
                .await?;
            } else {
                // Report retrying status for non-503 errors
                if let Some(ref cb) = progress {
                    cb(UploadProgress {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        bytes_sent: 0,
                        total_bytes: file_size,
                        status: UploadProgressStatus::Retrying,
                    });
                }
                let wait = super::retry::wait_before_retry(retry_after, &backoff, retries - 1);
                tokio::time::sleep(wait).await;
            }
        }

        // Report progress: uploading
        if let Some(ref cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: key.to_string(),
                bytes_sent: 0,
                total_bytes: file_size,
                status: UploadProgressStatus::Uploading,
            });
        }

        // Build request with all required headers.
        //
        // Uses raw_http() to bypass the retry middleware: reqwest-retry
        // requires cloneable request bodies (via try_clone()), but streaming
        // bodies (wrap_stream / File) cannot be cloned. This function has
        // its own retry loop with check_limit polling, so middleware retry
        // is redundant.
        let mut request = client
            .raw_http()
            .put(&s3_url)
            .header("Authorization", &auth_header)
            .header("Content-Length", file_size.to_string())
            .header("Expect", "100-continue");

        // Content-MD5 for server-side verification
        if let Some(b64) = &content_md5_b64 {
            request = request.header("Content-MD5", b64.as_str());
        }

        // x-archive-keep-old-version: 1 unless no_backup
        if !opts.no_backup {
            request = request.header("x-archive-keep-old-version", "1");
        }

        // x-archive-queue-derive: only 1 on last file (unless no_derive)
        if opts.no_derive || !is_last_file {
            request = request.header("x-archive-queue-derive", "0");
        } else {
            request = request.header("x-archive-queue-derive", "1");
        }

        // x-archive-auto-make-bucket: 1 on first file unless disabled
        if is_first_file && !opts.no_auto_make_bucket {
            request = request.header("x-archive-auto-make-bucket", "1");
        }

        // x-archive-size-hint (first file only, caller provides)
        if let Some(hint) = size_hint {
            request = request.header("x-archive-size-hint", hint.to_string());
        }

        // Metadata headers (first file only)
        for (k, v) in &metadata_headers {
            request = request.header(k.as_str(), v.as_str());
        }

        // Custom headers from opts
        for (k, v) in &opts.headers {
            request = request.header(k.as_str(), v.as_str());
        }

        // Open a fresh file handle per attempt — streams the body instead of
        // buffering the entire file in memory. Content-Length is already set from
        // file_size, so IA S3's no-chunked-transfer requirement is satisfied.
        let file_handle = tokio::fs::File::open(file).await?;

        // The body always streams through ProgressBody, with the progress
        // callback when the caller gave one, wrapped by this attempt's
        // stall watch so a server that stops reading is caught (#38). The
        // Arc<dyn Fn> is cloned into the move closure, satisfying the
        // 'static bound required by reqwest::Body::wrap_stream().
        let watch = BodyWatch::new();
        let progress_cb = progress.clone();
        let id = identifier.to_string();
        let k = key.to_string();
        let fs = file_size;
        let stream = super::progress_body::ProgressBody::new(file_handle, move |bytes_sent| {
            if let Some(cb) = &progress_cb {
                cb(UploadProgress {
                    identifier: id.clone(),
                    key: k.clone(),
                    bytes_sent,
                    total_bytes: fs,
                    status: UploadProgressStatus::Uploading,
                });
            }
        });
        let response = match watch_send(
            &watch,
            request
                .body(reqwest::Body::wrap_stream(watch.wrap(stream, file_size)))
                .send(),
        )
        .await
        {
            SendEnd::Done(response) => response,
            dead @ (SendEnd::Stalled { .. } | SendEnd::Unanswered { .. }) => {
                stalls += 1;
                let what = match dead {
                    SendEnd::Unanswered { .. } => {
                        unanswered += 1;
                        "no answer came within the response wait of the body"
                    }
                    _ => "body send moved no bytes for the whole window",
                };
                tracing::warn!(
                    identifier,
                    key,
                    retry = retries + 1,
                    window_secs,
                    response_secs,
                    "{what}, {}",
                    if retries < opts.retries {
                        "re-sending"
                    } else {
                        "giving up"
                    }
                );
                if retries < opts.retries {
                    if let Some(ref cb) = progress {
                        cb(UploadProgress {
                            identifier: identifier.to_string(),
                            key: key.to_string(),
                            bytes_sent: 0,
                            total_bytes: file_size,
                            status: UploadProgressStatus::Retrying,
                        });
                    }
                    last_was_503 = false;
                    last_was_stall = true;
                    retry_after = None;
                    retries += 1;
                    continue;
                }
                return Err(IaError::UploadStalled {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    window_secs,
                    stalls,
                    response_secs,
                    unanswered,
                });
            }
        };

        match response {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Some(ref cb) = progress {
                        cb(UploadProgress {
                            identifier: identifier.to_string(),
                            key: key.to_string(),
                            bytes_sent: file_size,
                            total_bytes: file_size,
                            status: UploadProgressStatus::Complete,
                        });
                    }

                    // Delete local file after successful upload if requested
                    if opts.delete_after_upload {
                        if let Err(e) = tokio::fs::remove_file(file).await {
                            tracing::warn!("failed to delete {} after upload: {e}", file.display());
                        }
                    }

                    return Ok(UploadResult {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        status: UploadStatus::Uploaded,
                        bytes: file_size,
                        md5: md5_hex,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        retries,
                    });
                } else if status.as_u16() == 503 {
                    retry_after = super::retry::retry_after_wait(resp.headers());
                    let body_text = resp.text().await.unwrap_or_default();

                    // Spam detection: permanent, no retry
                    if is_spam_response(&body_text) {
                        return Err(IaError::SpamDetected {
                            identifier: identifier.to_string(),
                        });
                    }

                    // A 503 carrying a non-retryable S3 code (AccessDenied,
                    // InvalidAccessKeyId, ...) is a refusal, not a throttle.
                    // Same classifier as every other IA-S3 request; polling
                    // check_limit and re-sending the file would not help.
                    if let Some(s3_err) = parse_s3_error(&body_text) {
                        if !s3_err.is_retryable() {
                            return Err(IaError::UploadFailed {
                                identifier: identifier.to_string(),
                                key: key.to_string(),
                                message: describe_parsed(status, Some(&s3_err), &body_text),
                                status: Some(503),
                            });
                        }
                    }

                    // Rate limited: retry with check_limit polling. A spent
                    // budget counts attempts, as every other branch does.
                    if retries >= opts.retries {
                        return Err(IaError::UploadFailed {
                            identifier: identifier.to_string(),
                            key: key.to_string(),
                            message: super::retry::describe_attempts(
                                &format!("HTTP 503: {}", strip_xml(&body_text)),
                                retries + 1,
                            ),
                            status: Some(503),
                        });
                    }
                    tracing::debug!(
                        identifier,
                        key,
                        retry = retries + 1,
                        "503 rate limited, will poll check_limit"
                    );
                    last_was_503 = true;
                    retries += 1;
                    continue;
                } else {
                    // Non-503 error — parse S3 XML to classify
                    retry_after = super::retry::retry_after_wait(resp.headers());
                    let body_text = resp.text().await.unwrap_or_default();
                    let s3_err = parse_s3_error(&body_text);

                    let err_msg = describe_parsed(status, s3_err.as_ref(), &body_text);

                    // Same policy as every other IA-S3 request.
                    let should_retry = crate::upload::s3_error::should_retry_s3(status, &body_text);

                    if should_retry && retries < opts.retries {
                        tracing::debug!(
                            identifier,
                            key,
                            retry = retries + 1,
                            %status,
                            "retrying upload: {err_msg}"
                        );
                        last_was_503 = false;
                        retries += 1;
                        continue;
                    }

                    // A spent budget says so, as the part path's message does;
                    // a first-try refusal keeps the bare S3 text.
                    return Err(IaError::UploadFailed {
                        identifier: identifier.to_string(),
                        key: key.to_string(),
                        message: super::retry::describe_attempts(&err_msg, retries + 1),
                        status: Some(status.as_u16()),
                    });
                }
            }
            Err(e) => {
                retry_after = None;
                let full_message = format_error_chain(&e);
                if retries < opts.retries {
                    tracing::debug!(
                        identifier,
                        key,
                        retry = retries + 1,
                        "network error, retrying: {full_message}"
                    );
                    last_was_503 = false;
                    retries += 1;
                    continue;
                }
                tracing::error!(
                    identifier,
                    key,
                    "upload failed after {retries} retries: {full_message}"
                );
                // A spent budget says so here too, as on an S3 error above
                // and as the part path's message does.
                return Err(IaError::UploadFailed {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    message: super::retry::describe_attempts(&full_message, retries + 1),
                    status: None,
                });
            }
        }
    }
}

use super::build_s3_url;

/// Poll the check_limit endpoint until the rate limit clears.
///
/// Polls up to `opts.retries` times, waiting between polls on the same
/// backoff schedule as the retries. There is no wait after the last poll:
/// nothing follows it but the error. The limit is per item, but the
/// `WaitingRateLimit` event each poll emits is addressed to the file being
/// uploaded (`key`, `file_size`), since the UIs keep their rows per file
/// and the file's row is what must show the wait.
/// Returns `Ok(())` when the rate limit has cleared.
/// Returns `Err(CheckLimitFailed)` if all retries are exhausted.
async fn poll_check_limit(
    client: &IaClient,
    identifier: &str,
    key: &str,
    file_size: u64,
    opts: &UploadOpts,
    backoff: &reqwest_retry::policies::ExponentialBackoff,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
) -> Result<()> {
    let (access, _) = client.require_auth()?;
    let protocol = client.protocol();
    let host = client.host();

    let check_url = if host == "archive.org" {
        format!(
            "{protocol}://s3.us.archive.org?check_limit=1&accesskey={access}&bucket={identifier}"
        )
    } else {
        format!("{protocol}://{host}?check_limit=1&accesskey={access}&bucket={identifier}")
    };

    for attempt in 0..opts.retries {
        if let Some(ref cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: key.to_string(),
                bytes_sent: 0,
                total_bytes: file_size,
                status: UploadProgressStatus::WaitingRateLimit,
            });
        }

        match client.http().get(&check_url).send().await {
            Ok(r) => {
                let body = r.text().await.unwrap_or_default();
                if !parse_check_limit_response(&body) {
                    // Rate limit cleared
                    return Ok(());
                }
            }
            Err(_) => {
                // Don't log the error — the check_limit URL contains the
                // access key as a query parameter, and reqwest may include
                // the URL in error messages.
                tracing::debug!(identifier, "check_limit request failed");
            }
        }

        if attempt + 1 < opts.retries {
            tokio::time::sleep(super::retry::backoff_wait(backoff, attempt)).await;
        }
    }

    Err(IaError::CheckLimitFailed {
        identifier: identifier.to_string(),
    })
}

/// Encode bytes as base64 (standard alphabet, with padding).
///
/// Used for the Content-MD5 header value on the single PUT and on each
/// multipart part. Avoids adding the `base64` crate for two call sites.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Convert a hex string to raw bytes.
///
/// Returns an empty vec if the input contains invalid hex.
/// Input from `compute_file_md5()` is always valid 32-char hex.
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| {
            hex.get(i..i + 2)
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // format_error_chain is tested in crate::error

    #[test]
    fn base64_encode_empty() {
        assert_eq!(base64_encode(&[]), "");
    }

    #[test]
    fn base64_encode_one_byte() {
        // 'M' = 0x4D -> base64 "TQ=="
        assert_eq!(base64_encode(&[0x4D]), "TQ==");
    }

    #[test]
    fn base64_encode_two_bytes() {
        // 'Ma' = 0x4D 0x61 -> base64 "TWE="
        assert_eq!(base64_encode(&[0x4D, 0x61]), "TWE=");
    }

    #[test]
    fn base64_encode_three_bytes() {
        // 'Man' = 0x4D 0x61 0x6E -> base64 "TWFu"
        assert_eq!(base64_encode(&[0x4D, 0x61, 0x6E]), "TWFu");
    }

    #[test]
    fn base64_encode_md5_of_hello() {
        // MD5("hello") = 5d41402abc4b2a76b9719d911017c592
        let hex = "5d41402abc4b2a76b9719d911017c592";
        let bytes = hex_to_bytes(hex);
        assert_eq!(bytes.len(), 16);
        let b64 = base64_encode(&bytes);
        // Known correct base64 for this MD5
        assert_eq!(b64, "XUFAKrxLKna5cZ2REBfFkg==");
    }

    #[test]
    fn base64_encode_md5_of_empty() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        let hex = "d41d8cd98f00b204e9800998ecf8427e";
        let bytes = hex_to_bytes(hex);
        let b64 = base64_encode(&bytes);
        assert_eq!(b64, "1B2M2Y8AsgTpgAmY7PhCfg==");
    }

    #[test]
    fn hex_to_bytes_known_value() {
        assert_eq!(hex_to_bytes("ff00ab"), vec![0xFF, 0x00, 0xAB]);
    }

    #[test]
    fn hex_to_bytes_odd_length_does_not_panic() {
        let result = hex_to_bytes("abc");
        assert!(result.len() <= 1);
    }

    #[test]
    fn hex_to_bytes_empty() {
        assert!(hex_to_bytes("").is_empty());
    }

    #[test]
    fn build_s3_url_production() {
        let config = crate::IaConfig::default();
        let client = crate::IaClient::from_config(config).unwrap();
        let url = build_s3_url(&client, "test-item", "file.txt");
        assert_eq!(url, "https://s3.us.archive.org/test-item/file.txt");
    }

    #[test]
    fn build_s3_url_mock() {
        let mut config = crate::IaConfig::default();
        config.general.host = "127.0.0.1:8080".to_string();
        config.general.secure = false;
        let client = crate::IaClient::from_config(config).unwrap();
        let url = build_s3_url(&client, "test-item", "file.txt");
        assert_eq!(url, "http://127.0.0.1:8080/test-item/file.txt");
    }

    #[test]
    fn build_s3_url_encodes_special_chars() {
        let config = crate::IaConfig::default();
        let client = crate::IaClient::from_config(config).unwrap();
        let url = build_s3_url(&client, "test-item", "path/to/my file.txt");
        // Slashes preserved, spaces encoded per-segment
        assert_eq!(
            url,
            "https://s3.us.archive.org/test-item/path/to/my%20file.txt"
        );
    }

    // -- Stall detection on the body send (#38) --

    use crate::upload::stall_watch::test_support::{
        shrink_policy, silent_listener, stalling_listener,
    };

    fn stalling_client(addr: std::net::SocketAddr) -> crate::IaClient {
        let mut config = crate::IaConfig::default();
        config.s3_access = Some("a".into());
        config.s3_secret = Some("s".into());
        config.general.host = addr.to_string();
        config.general.secure = false;
        crate::IaClient::from_config_no_retry(config).unwrap()
    }

    /// 16 MiB on disk: larger than the loopback socket buffers, so the
    /// send really stops when the server stops reading.
    fn big_temp_file() -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&crate::upload::stall_watch::test_support::big_body())
            .unwrap();
        f.flush().unwrap();
        f
    }

    /// A single-file PUT whose body send stalls is abandoned and re-sent;
    /// when the retries are spent the file fails as stalled, with one stall
    /// per attempt.
    #[tokio::test]
    async fn stalled_single_put_is_retried_then_fails_as_stalled() {
        let _policy = shrink_policy();
        let (addr, connections) = stalling_listener().await;
        let client = stalling_client(addr);
        let f = big_temp_file();
        let opts = UploadOpts {
            verify: false,
            checksum: false,
            retries: 1,
            ..Default::default()
        };
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            upload_file(
                &client,
                "test-item",
                f.path(),
                "big.bin",
                &opts,
                true,
                true,
                None,
                None,
            ),
        )
        .await
        .expect("the stalled sends must be judged within a minute")
        .expect_err("a stalled send must fail once the retries are spent");
        assert!(
            matches!(
                err,
                IaError::UploadStalled {
                    stalls: 2,
                    window_secs: 2,
                    unanswered: 0,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A single-file PUT the server reads in full and never answers is
    /// abandoned after the response wait and re-sent; when the retries are
    /// spent the file fails as stalled, every stall an unanswered body (#40).
    #[tokio::test]
    async fn unanswered_single_put_is_retried_then_fails_as_stalled() {
        let _policy = shrink_policy();
        let (addr, connections) = silent_listener().await;
        let client = stalling_client(addr);
        let f = {
            use std::io::Write;
            let mut f = tempfile::NamedTempFile::new().unwrap();
            f.write_all(&[9u8; 200 * 1024]).unwrap();
            f.flush().unwrap();
            f
        };
        let opts = UploadOpts {
            verify: false,
            checksum: false,
            retries: 1,
            ..Default::default()
        };
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            upload_file(
                &client,
                "test-item",
                f.path(),
                "small.bin",
                &opts,
                true,
                true,
                None,
                None,
            ),
        )
        .await
        .expect("the unanswered sends must be judged within the wait")
        .expect_err("an unanswered body must fail once the retries are spent");
        assert!(
            matches!(
                err,
                IaError::UploadStalled {
                    stalls: 2,
                    unanswered: 2,
                    response_secs: 2,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
