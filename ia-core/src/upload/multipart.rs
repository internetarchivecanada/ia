//! Multipart upload support for IA S3.
//!
//! Implements the S3 multipart upload protocol:
//! - Initiate: POST /{id}/{key}?uploads → UploadId
//! - Upload part: PUT /{id}/{key}?partNumber={N}&uploadId={ID} → ETag
//! - Complete: POST /{id}/{key}?uploadId={ID} with XML manifest
//! - Resume: GET /{id}?uploads → list, GET /{id}/{key}?uploadId={ID} → parts
//! - Abort: DELETE /{id}/{key}?uploadId={ID}
//! - Cleanup: GET /{id}?uploads (list all), then abort

use crate::error::{IaError, Result};
use crate::upload::s3_error::{parse_s3_error, strip_xml};
use crate::upload::types::{MultipartUploadInfo, PartInfo};
use crate::IaClient;

/// Default part size: 100 MiB.
pub const DEFAULT_PART_SIZE: u64 = 100 * 1024 * 1024;

// ── XML parsing helpers ─────────────────────────────────────────────────────
//
// S3 returns XML for multipart operations. We use simple string matching
// (consistent with s3_error.rs) since the XML shapes are well-defined.

/// Extract text between `<Tag>` and `</Tag>`.
fn extract_xml_field(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

/// Extract all occurrences of `<Tag>...</Tag>` blocks.
fn extract_xml_blocks<'a>(body: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut search_from = 0;
    while let Some(start) = body[search_from..].find(&open) {
        let abs_start = search_from + start;
        let content_start = abs_start + open.len();
        if let Some(end) = body[content_start..].find(&close) {
            let abs_end = content_start + end + close.len();
            blocks.push(&body[abs_start..abs_end]);
            search_from = abs_end;
        } else {
            break;
        }
    }
    blocks
}

/// Parse the UploadId from an InitiateMultipartUpload response.
///
/// Example XML:
/// ```xml
/// <InitiateMultipartUploadResult>
///   <Bucket>my-item</Bucket>
///   <Key>file.zip</Key>
///   <UploadId>abc123</UploadId>
/// </InitiateMultipartUploadResult>
/// ```
pub fn parse_initiate_response(body: &str) -> Option<String> {
    extract_xml_field(body, "UploadId")
}

/// Parse the list of in-progress multipart uploads for an item.
///
/// Example XML:
/// ```xml
/// <ListMultipartUploadsResult>
///   <Upload>
///     <Key>file.zip</Key>
///     <UploadId>abc123</UploadId>
///     <Initiated>2026-03-06T12:00:00.000Z</Initiated>
///   </Upload>
/// </ListMultipartUploadsResult>
/// ```
pub fn parse_list_uploads_response(body: &str) -> Vec<MultipartUploadInfo> {
    extract_xml_blocks(body, "Upload")
        .into_iter()
        .filter_map(|block| {
            let key = extract_xml_field(block, "Key")?;
            let upload_id = extract_xml_field(block, "UploadId")?;
            let initiated = extract_xml_field(block, "Initiated").unwrap_or_default();
            Some(MultipartUploadInfo {
                key,
                upload_id,
                initiated,
            })
        })
        .collect()
}

/// Parse the list of completed parts for a multipart upload.
///
/// Example XML:
/// ```xml
/// <ListPartsResult>
///   <Part>
///     <PartNumber>1</PartNumber>
///     <ETag>"abc123"</ETag>
///     <Size>104857600</Size>
///   </Part>
/// </ListPartsResult>
/// ```
pub fn parse_list_parts_response(body: &str) -> Vec<PartInfo> {
    extract_xml_blocks(body, "Part")
        .into_iter()
        .filter_map(|block| {
            let part_number: u32 = extract_xml_field(block, "PartNumber")?.parse().ok()?;
            let etag = extract_xml_field(block, "ETag")?;
            let size: u64 = extract_xml_field(block, "Size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            Some(PartInfo {
                part_number,
                etag,
                size,
            })
        })
        .collect()
}

/// Build the XML manifest for CompleteMultipartUpload.
///
/// Output:
/// ```xml
/// <CompleteMultipartUpload>
///   <Part><PartNumber>1</PartNumber><ETag>"abc"</ETag></Part>
///   <Part><PartNumber>2</PartNumber><ETag>"def"</ETag></Part>
/// </CompleteMultipartUpload>
/// ```
pub fn build_complete_manifest(parts: &[(u32, String)]) -> String {
    let mut xml = String::from("<CompleteMultipartUpload>");
    for (num, etag) in parts {
        // Minimal XML escaping for ETags (defense-in-depth; S3 ETags are
        // always hex strings, but we guard against unexpected characters).
        let safe_etag = etag
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        xml.push_str(&format!(
            "<Part><PartNumber>{num}</PartNumber><ETag>{safe_etag}</ETag></Part>"
        ));
    }
    xml.push_str("</CompleteMultipartUpload>");
    xml
}

use super::{build_s3_item_url, build_s3_url};

// ── S3 operations ───────────────────────────────────────────────────────

/// Initiate a multipart upload. Returns the server-assigned upload ID.
///
/// `POST /{identifier}/{key}?uploads`
///
/// `extra_headers` allows callers to attach metadata (`x-archive-meta*`),
/// `x-archive-auto-make-bucket`, `x-archive-queue-derive`, and other
/// item-creation headers to the initiate request. IA S3 only accepts
/// metadata at item creation time, so these headers must be sent here.
pub async fn initiate_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    extra_headers: &[(String, String)],
) -> Result<String> {
    let (access, secret) = client.require_auth()?;
    let url = format!("{}?uploads", build_s3_url(client, identifier, key));

    let mut req = client
        .upload_http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Length", "0");

    for (k, v) in extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await.map_err(|e| IaError::UploadFailed {
        identifier: identifier.into(),
        key: key.into(),
        message: format!("initiate multipart: {e}"),
        status: None,
    })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("initiate multipart failed: {msg}"),
            status: Some(status.as_u16()),
        });
    }

    parse_initiate_response(&body).ok_or_else(|| IaError::UploadFailed {
        identifier: identifier.into(),
        key: key.into(),
        message: "initiate response missing UploadId".into(),
        status: None,
    })
}

/// Upload a single part. Returns the ETag for the completion manifest.
///
/// `PUT /{identifier}/{key}?partNumber={N}&uploadId={ID}`
///
/// IA's S3 does not return an `ETag` header on part PUTs; its completion
/// check compares the manifest entry against the part's MD5. When the header
/// is absent, the quoted hex MD5 of the body is used instead.
pub async fn upload_part(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    part_number: u32,
    body: Vec<u8>,
) -> Result<String> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?partNumber={}&uploadId={}",
        build_s3_url(client, identifier, key),
        part_number,
        upload_id,
    );
    let content_length = body.len();
    let local_md5 = {
        use md5::{Digest, Md5};
        format!("\"{:x}\"", Md5::digest(&body))
    };

    let resp = client
        .upload_http()
        .put(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Length", content_length.to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("upload part {part_number}: {e}"),
            status: None,
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        let msg = parse_s3_error(&body_text)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body_text)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("upload part {part_number} failed: {msg}"),
            status: Some(status.as_u16()),
        });
    }

    // Prefer the server's ETag; IA omits it, so fall back to the local MD5.
    Ok(resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or(local_md5))
}

/// Complete a multipart upload by sending the manifest.
///
/// `POST /{identifier}/{key}?uploadId={ID}` with XML body
pub async fn complete_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    parts: &[(u32, String)],
    keep_old_version: bool,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let manifest = build_complete_manifest(parts);
    let mut req = client
        .upload_http()
        .post(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Type", "application/xml")
        .header("Content-Length", manifest.len().to_string());

    if keep_old_version {
        req = req.header("x-archive-keep-old-version", "1");
    }

    let resp = req
        .body(manifest)
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("complete multipart: {e}"),
            status: None,
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("complete multipart failed: {msg}"),
            status: Some(status.as_u16()),
        });
    }
    Ok(())
}

/// Abort a multipart upload.
///
/// `DELETE /{identifier}/{key}?uploadId={ID}`
pub async fn abort_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let resp = client
        .upload_http()
        .delete(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("abort multipart: {e}"),
            status: None,
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("abort multipart failed: {msg}"),
            status: Some(status.as_u16()),
        });
    }
    Ok(())
}

/// List all in-progress multipart uploads for an item.
///
/// `GET /{identifier}?uploads`
///
/// Returns an empty list when the item does not exist yet (`NoSuchBucket`),
/// so callers can fall through to a fresh initiate on a new item.
pub async fn list_uploads(client: &IaClient, identifier: &str) -> Result<Vec<MultipartUploadInfo>> {
    let (access, secret) = client.require_auth()?;
    let url = format!("{}?uploads", build_s3_item_url(client, identifier));

    let resp = client
        .upload_http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: String::new(),
            message: format!("list multipart uploads: {e}"),
            status: None,
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let s3_err = parse_s3_error(&body);
        // A brand-new item has no bucket yet, so there is nothing in progress
        // to list. Treat that as an empty result; the initiate POST that
        // follows carries x-archive-auto-make-bucket and creates the item.
        if s3_err.as_ref().is_some_and(|e| e.code == "NoSuchBucket") {
            tracing::debug!(
                identifier,
                "item does not exist yet; no multipart uploads to resume"
            );
            return Ok(Vec::new());
        }
        let msg = s3_err
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: String::new(),
            message: format!("list multipart uploads: {msg}"),
            status: Some(status.as_u16()),
        });
    }

    Ok(parse_list_uploads_response(&body))
}

/// List completed parts for a multipart upload.
///
/// `GET /{identifier}/{key}?uploadId={ID}`
pub async fn list_parts(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
) -> Result<Vec<PartInfo>> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let resp = client
        .upload_http()
        .get(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("list parts: {e}"),
            status: None,
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = parse_s3_error(&body)
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: format!("list parts: {msg}"),
            status: Some(status.as_u16()),
        });
    }

    Ok(parse_list_parts_response(&body))
}

// ── Full multipart upload ───────────────────────────────────────────────

use crate::upload::types::{
    UploadOpts, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Upload a file using the S3 multipart protocol.
///
/// Flow: check for resume → initiate (if fresh) → split into parts → upload
/// each part with retry → complete.
/// On permanent part failure, aborts the upload (best-effort cleanup).
/// Retries individual parts on transient errors.
///
/// `part_size` controls the split size. Use [`DEFAULT_PART_SIZE`] for production.
/// A smaller value can be passed for testing.
///
/// `is_first_file` / `is_last_file` / `size_hint` control the same IA S3
/// headers as the single-PUT path (`x-archive-auto-make-bucket`,
/// `x-archive-queue-derive`, `x-archive-size-hint`), plus metadata headers
/// on the initiate POST.
#[allow(clippy::too_many_arguments)]
pub async fn upload_file_multipart(
    client: &IaClient,
    identifier: &str,
    file: &Path,
    key: &str,
    opts: &UploadOpts,
    part_size: u64,
    is_first_file: bool,
    is_last_file: bool,
    size_hint: Option<u64>,
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
) -> Result<UploadResult> {
    if part_size == 0 {
        return Err(IaError::UploadFailed {
            identifier: identifier.into(),
            key: key.into(),
            message: "part_size must be greater than 0".into(),
            status: None,
        });
    }

    let start = Instant::now();
    let file_size = tokio::fs::metadata(file).await?.len();

    // Dry run: report what would happen without contacting the server
    if opts.dry_run {
        return Ok(UploadResult {
            identifier: identifier.into(),
            key: key.into(),
            status: UploadStatus::DryRun,
            bytes: file_size,
            md5: None,
            elapsed_ms: start.elapsed().as_millis() as u64,
            retries: 0,
        });
    }

    // Report verifying phase
    if let Some(ref cb) = progress {
        cb(UploadProgress {
            identifier: identifier.into(),
            key: key.into(),
            bytes_sent: 0,
            total_bytes: file_size,
            status: UploadProgressStatus::Verifying,
        });
    }

    // Try to resume an existing upload
    let (upload_id, existing_parts) = try_resume(client, identifier, key).await?;

    // Build extra headers for the initiate POST (metadata, auto-make-bucket, etc.)
    let extra_headers = {
        use crate::upload::headers::encode_metadata_headers;
        let mut hdrs = Vec::new();

        // Metadata headers (first file only, same as single-PUT path)
        if is_first_file && !opts.metadata.is_empty() {
            hdrs.extend(encode_metadata_headers(&opts.metadata)?);
        }

        // x-archive-auto-make-bucket (first file only)
        if is_first_file && !opts.no_auto_make_bucket {
            hdrs.push(("x-archive-auto-make-bucket".to_string(), "1".to_string()));
        }

        // x-archive-size-hint (first file only)
        if let Some(hint) = size_hint {
            hdrs.push(("x-archive-size-hint".to_string(), hint.to_string()));
        }

        // x-archive-queue-derive
        if opts.no_derive || !is_last_file {
            hdrs.push(("x-archive-queue-derive".to_string(), "0".to_string()));
        } else {
            hdrs.push(("x-archive-queue-derive".to_string(), "1".to_string()));
        }

        // Custom headers from opts
        hdrs.extend(opts.headers.iter().cloned());

        hdrs
    };

    let (upload_id, mut completed_parts) = match upload_id {
        Some(id) => {
            tracing::info!(
                identifier,
                key,
                upload_id = %id,
                existing_parts = existing_parts.len(),
                "resuming multipart upload"
            );
            let parts: Vec<(u32, String)> = existing_parts
                .into_iter()
                .map(|p| (p.part_number, p.etag))
                .collect();
            (id, parts)
        }
        None => {
            let id = initiate_upload(client, identifier, key, &extra_headers).await?;
            tracing::debug!(identifier, key, upload_id = %id, "initiated multipart upload");
            (id, Vec::new())
        }
    };

    // Compute part boundaries
    let part_count = file_size.div_ceil(part_size).max(1) as u32;
    let mut total_retries = 0u32;

    // Upload each part
    for part_num in 1..=part_count {
        // Skip already-completed parts (resume)
        if completed_parts.iter().any(|(n, _)| *n == part_num) {
            continue;
        }

        let offset = (part_num as u64 - 1) * part_size;
        let this_part_size = std::cmp::min(part_size, file_size - offset) as usize;

        // Report progress
        if let Some(ref cb) = progress {
            cb(UploadProgress {
                identifier: identifier.into(),
                key: key.into(),
                bytes_sent: offset,
                total_bytes: file_size,
                status: UploadProgressStatus::Uploading,
            });
        }

        // Per-part retry loop — re-read from file on each attempt to avoid
        // holding a 100 MiB clone in memory across retries.
        let mut part_retries = 0u32;
        let etag = loop {
            let data = read_file_range(file, offset, this_part_size).await?;
            tracing::debug!(
                identifier,
                key,
                part = part_num,
                of = part_count,
                bytes = this_part_size,
                "uploading part"
            );
            match upload_part(client, identifier, key, &upload_id, part_num, data).await {
                Ok(etag) => {
                    tracing::debug!(identifier, key, part = part_num, %etag, "part uploaded");
                    break etag;
                }
                Err(e) => {
                    // Check if retryable: 5xx status codes are transient
                    let is_retryable = matches!(
                        &e,
                        IaError::UploadFailed { status: Some(code), .. }
                            if *code >= 500
                    );

                    if is_retryable && part_retries < opts.retries {
                        part_retries += 1;
                        total_retries += 1;

                        if let Some(ref cb) = progress {
                            cb(UploadProgress {
                                identifier: identifier.into(),
                                key: key.into(),
                                bytes_sent: offset,
                                total_bytes: file_size,
                                status: UploadProgressStatus::WaitingRateLimit,
                            });
                        }

                        tokio::time::sleep(opts.retry_sleep).await;
                        continue;
                    }

                    // Non-retryable or retries exhausted: abort the upload
                    tracing::warn!(
                        identifier,
                        key,
                        part_num,
                        "multipart part failed, aborting upload"
                    );
                    if let Err(abort_err) = abort_upload(client, identifier, key, &upload_id).await
                    {
                        tracing::warn!(
                            identifier,
                            key,
                            %upload_id,
                            error = %abort_err,
                            "failed to abort multipart upload after part failure — \
                             run `ia upload cleanup` to clean up orphaned uploads"
                        );
                    }
                    return Err(e);
                }
            }
        };

        completed_parts.push((part_num, etag));
    }

    // Sort parts by number — required by S3 CompleteMultipartUpload.
    // When resuming, existing parts may be non-contiguous (e.g. parts 1,3
    // done, part 2 uploaded now) leaving completed_parts out of order.
    completed_parts.sort_by_key(|(num, _)| *num);

    // Complete the multipart upload
    let keep_old_version = !opts.no_backup;
    complete_upload(
        client,
        identifier,
        key,
        &upload_id,
        &completed_parts,
        keep_old_version,
    )
    .await?;

    // Report completion
    if let Some(ref cb) = progress {
        cb(UploadProgress {
            identifier: identifier.into(),
            key: key.into(),
            bytes_sent: file_size,
            total_bytes: file_size,
            status: UploadProgressStatus::Complete,
        });
    }

    // Delete local file if requested
    if opts.delete_after_upload {
        if let Err(e) = tokio::fs::remove_file(file).await {
            tracing::warn!("failed to delete {} after upload: {e}", file.display());
        }
    }

    Ok(UploadResult {
        identifier: identifier.into(),
        key: key.into(),
        status: UploadStatus::Uploaded,
        bytes: file_size,
        md5: None,
        elapsed_ms: start.elapsed().as_millis() as u64,
        retries: total_retries,
    })
}

/// Read a range of bytes from a file.
async fn read_file_range(file: &Path, offset: u64, len: usize) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(file).await?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Check for an existing in-progress upload for this key and return it.
///
/// If multiple uploads exist for the same key, returns the most recent one.
async fn try_resume(
    client: &IaClient,
    identifier: &str,
    key: &str,
) -> Result<(Option<String>, Vec<PartInfo>)> {
    let uploads = list_uploads(client, identifier).await?;

    // Find uploads matching this key, take the most recent
    // Take the last matching upload (most recent, S3 returns chronological order)
    let matching = uploads.iter().rfind(|u| u.key == key);

    match matching {
        Some(info) => {
            let parts = list_parts(client, identifier, key, &info.upload_id).await?;
            Ok((Some(info.upload_id.clone()), parts))
        }
        None => Ok((None, Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_xml_field --

    #[test]
    fn extract_field_basic() {
        let xml = "<Root><UploadId>abc123</UploadId></Root>";
        assert_eq!(extract_xml_field(xml, "UploadId").unwrap(), "abc123");
    }

    #[test]
    fn extract_field_with_whitespace() {
        let xml = "<Root>\n  <UploadId> abc123 </UploadId>\n</Root>";
        assert_eq!(extract_xml_field(xml, "UploadId").unwrap(), "abc123");
    }

    #[test]
    fn extract_field_missing() {
        let xml = "<Root><Other>value</Other></Root>";
        assert!(extract_xml_field(xml, "UploadId").is_none());
    }

    // -- extract_xml_blocks --

    #[test]
    fn extract_blocks_multiple() {
        let xml = "<Root><Item>a</Item><Item>b</Item><Item>c</Item></Root>";
        let blocks = extract_xml_blocks(xml, "Item");
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].contains("a"));
        assert!(blocks[2].contains("c"));
    }

    #[test]
    fn extract_blocks_none() {
        let xml = "<Root><Other>a</Other></Root>";
        let blocks = extract_xml_blocks(xml, "Item");
        assert!(blocks.is_empty());
    }

    // -- parse_initiate_response --

    #[test]
    fn parse_initiate_success() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult>
  <Bucket>my-item</Bucket>
  <Key>file.zip</Key>
  <UploadId>VXBsb2FkIElEIGZvciBlbG</UploadId>
</InitiateMultipartUploadResult>"#;
        assert_eq!(
            parse_initiate_response(xml).unwrap(),
            "VXBsb2FkIElEIGZvciBlbG"
        );
    }

    #[test]
    fn parse_initiate_not_xml() {
        assert!(parse_initiate_response("not xml").is_none());
    }

    // -- parse_list_uploads_response --

    #[test]
    fn parse_list_uploads_multiple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListMultipartUploadsResult>
  <Bucket>my-item</Bucket>
  <Upload>
    <Key>file1.zip</Key>
    <UploadId>upload-1</UploadId>
    <Initiated>2026-03-06T12:00:00.000Z</Initiated>
  </Upload>
  <Upload>
    <Key>file2.zip</Key>
    <UploadId>upload-2</UploadId>
    <Initiated>2026-03-06T13:00:00.000Z</Initiated>
  </Upload>
</ListMultipartUploadsResult>"#;
        let uploads = parse_list_uploads_response(xml);
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[0].key, "file1.zip");
        assert_eq!(uploads[0].upload_id, "upload-1");
        assert_eq!(uploads[1].key, "file2.zip");
    }

    #[test]
    fn parse_list_uploads_empty() {
        let xml = r#"<ListMultipartUploadsResult>
  <Bucket>my-item</Bucket>
</ListMultipartUploadsResult>"#;
        let uploads = parse_list_uploads_response(xml);
        assert!(uploads.is_empty());
    }

    // -- parse_list_parts_response --

    #[test]
    fn parse_list_parts_multiple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListPartsResult>
  <Bucket>my-item</Bucket>
  <Key>file.zip</Key>
  <UploadId>abc123</UploadId>
  <Part>
    <PartNumber>1</PartNumber>
    <ETag>"etag1"</ETag>
    <Size>104857600</Size>
  </Part>
  <Part>
    <PartNumber>2</PartNumber>
    <ETag>"etag2"</ETag>
    <Size>52428800</Size>
  </Part>
</ListPartsResult>"#;
        let parts = parse_list_parts_response(xml);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part_number, 1);
        assert_eq!(parts[0].etag, "\"etag1\"");
        assert_eq!(parts[0].size, 104857600);
        assert_eq!(parts[1].part_number, 2);
        assert_eq!(parts[1].size, 52428800);
    }

    #[test]
    fn parse_list_parts_empty() {
        let xml = "<ListPartsResult></ListPartsResult>";
        assert!(parse_list_parts_response(xml).is_empty());
    }

    // -- build_complete_manifest --

    #[test]
    fn build_manifest_single_part() {
        let parts = vec![(1, "\"etag1\"".to_string())];
        let xml = build_complete_manifest(&parts);
        assert_eq!(
            xml,
            "<CompleteMultipartUpload>\
             <Part><PartNumber>1</PartNumber><ETag>\"etag1\"</ETag></Part>\
             </CompleteMultipartUpload>"
        );
    }

    #[test]
    fn build_manifest_multiple_parts() {
        let parts = vec![
            (1, "\"etag1\"".to_string()),
            (2, "\"etag2\"".to_string()),
            (3, "\"etag3\"".to_string()),
        ];
        let xml = build_complete_manifest(&parts);
        assert!(xml.starts_with("<CompleteMultipartUpload>"));
        assert!(xml.ends_with("</CompleteMultipartUpload>"));
        assert!(xml.contains("<PartNumber>2</PartNumber>"));
        assert_eq!(xml.matches("<Part>").count(), 3);
    }

    // -- constants --

    #[test]
    fn default_part_size_is_100mib() {
        assert_eq!(DEFAULT_PART_SIZE, 100 * 1024 * 1024);
    }
}
