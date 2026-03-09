use crate::error::{IaError, Result};
use crate::upload::check_limit::{is_spam_response, parse_check_limit_response};
use crate::upload::checksum::compute_file_md5_async;
use crate::upload::headers::encode_metadata_headers;
use crate::upload::s3_error::parse_s3_error;
use crate::upload::types::*;
use crate::IaClient;
use std::path::Path;
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
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<UploadResult> {
    if opts.multipart {
        return crate::upload::multipart::upload_file_multipart(
            client,
            identifier,
            file,
            key,
            opts,
            crate::upload::multipart::DEFAULT_PART_SIZE,
            progress,
        )
        .await;
    }

    let start = Instant::now();
    let file_size = tokio::fs::metadata(file).await?.len();

    // Compute local MD5 if needed for either skip-existing or verify
    let needs_md5 = opts.skip_existing || opts.verify;
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

    // Skip-existing: compare local MD5 with remote, skip if match
    if opts.skip_existing {
        let local_md5 = md5_hex.as_deref().ok_or_else(|| IaError::Config(
            "internal error: MD5 not computed for skip_existing check".into()
        ))?;
        match client.get_item(identifier).await {
            Ok(item) => {
                let remote_md5 = item
                    .files
                    .iter()
                    .find(|f| f.name == key)
                    .and_then(|f| f.md5.as_deref());

                if remote_md5 == Some(local_md5) {
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
        encode_metadata_headers(&opts.metadata)
    } else {
        Vec::new()
    };

    // Retry loop
    let mut retries = 0u32;
    loop {
        // On retry, poll check_limit before re-uploading
        if retries > 0 {
            poll_check_limit(client, identifier, opts, progress).await?;
        }

        // Report progress: uploading
        if let Some(cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: key.to_string(),
                bytes_sent: 0,
                total_bytes: file_size,
                status: UploadProgressStatus::Uploading,
            });
        }

        // Build request with all required headers
        let mut request = client
            .http()
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
        let stream_body = reqwest::Body::from(file_handle);
        let response = request.body(stream_body).send().await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Some(cb) = progress {
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
                            tracing::warn!(
                                "failed to delete {} after upload: {e}",
                                file.display()
                            );
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
                    let body_text = resp.text().await.unwrap_or_default();

                    // Spam detection: permanent, no retry
                    if is_spam_response(&body_text) {
                        return Err(IaError::SpamDetected {
                            identifier: identifier.to_string(),
                        });
                    }

                    // Rate limited: retry with check_limit polling
                    if retries >= opts.retries {
                        return Err(IaError::UploadFailed {
                            identifier: identifier.to_string(),
                            key: key.to_string(),
                            message: format!(
                                "503 after {retries} retries: {body_text}"
                            ),
                            status: Some(503),
                        });
                    }
                    tracing::warn!(
                        identifier,
                        key,
                        retry = retries + 1,
                        "503 rate limited, will poll check_limit"
                    );
                    retries += 1;
                    continue;
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
                        status: Some(status.as_u16()),
                    });
                }
            }
            Err(e) => {
                if retries < opts.retries {
                    tracing::warn!(
                        identifier,
                        key,
                        retry = retries + 1,
                        "network error, retrying: {e}"
                    );
                    retries += 1;
                    continue;
                }
                return Err(IaError::UploadFailed {
                    identifier: identifier.to_string(),
                    key: key.to_string(),
                    message: e.to_string(),
                    status: None,
                });
            }
        }
    }
}

use super::build_s3_url;

/// Poll the check_limit endpoint until the rate limit clears.
///
/// Retries up to `opts.retries` times with `opts.retry_sleep` between polls.
/// Returns `Ok(())` when the rate limit has cleared.
/// Returns `Err(CheckLimitFailed)` if all retries are exhausted.
async fn poll_check_limit(
    client: &IaClient,
    identifier: &str,
    opts: &UploadOpts,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
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

    for _attempt in 0..opts.retries {
        if let Some(cb) = progress {
            cb(UploadProgress {
                identifier: identifier.to_string(),
                key: String::new(),
                bytes_sent: 0,
                total_bytes: 0,
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
                tracing::warn!(identifier, "check_limit request failed");
            }
        }

        tokio::time::sleep(opts.retry_sleep).await;
    }

    Err(IaError::CheckLimitFailed {
        identifier: identifier.to_string(),
    })
}

/// Encode bytes as base64 (standard alphabet, with padding).
///
/// Used for Content-MD5 header value. Avoids adding the `base64` crate
/// for a single call site.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
        .filter_map(|i| hex.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
