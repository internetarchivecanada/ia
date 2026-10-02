//! Multipart upload support for IA S3.
//!
//! Implements the S3 multipart upload protocol:
//! - Initiate: POST /{id}/{key}?uploads → UploadId
//! - Upload part: PUT /{id}/{key}?partNumber={N}&uploadId={ID} → ETag
//! - Complete: POST /{id}/{key}?uploadId={ID} with XML manifest
//! - Resume: GET /{id}?uploads → list, GET /{id}/{key}?uploadId={ID} → parts
//! - Abort: DELETE /{id}/{key}?uploadId={ID}
//! - Cleanup: GET /{id}?uploads (list all), then abort
//!
//! Both listings follow S3 pagination (`IsTruncated` and the next marker).

use super::retry::{send_with_retry, S3Failure, S3RetryCtx};
use super::stall_watch::bytes_chunks;
use crate::error::{IaError, Result};
use crate::upload::checksum::FileHashes;
use crate::upload::types::{MultipartUploadInfo, PartInfo};
use crate::IaClient;
use bytes::Bytes;

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
///
/// Retries per the shared IA-S3 policy with the default budget. Callers that
/// need a configurable budget go through `upload_file_multipart`.
pub async fn initiate_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    extra_headers: &[(String, String)],
) -> Result<String> {
    initiate_upload_with_retry(client, &default_ctx(identifier, key), extra_headers)
        .await
        .map(|(id, _attempts)| id)
}

/// Begin a multipart upload, retrying per the shared IA-S3 policy.
///
/// Retrying is correct here: IA answers throttling with 503 `SlowDown`, which
/// means the request was refused and no upload was created. The cost of not
/// retrying is that `--multipart` fails outright on IA's most common
/// response, while a plain upload survives it.
pub(crate) async fn initiate_upload_with_retry(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    extra_headers: &[(String, String)],
) -> Result<(String, u32)> {
    let (access, secret) = client.require_auth()?;
    let (identifier, key) = (ctx.identifier, ctx.key);
    let url = format!("{}?uploads", build_s3_url(client, identifier, key));

    let sent = send_with_retry(ctx, "initiate multipart", |_watch| {
        let mut req = client
            .upload_http()
            .post(&url)
            .header("Authorization", format!("LOW {access}:{secret}"))
            .header("Content-Length", "0");
        for (k, v) in extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req.send()
    })
    .await?;

    let attempts = sent.attempts;
    let body = sent.response.text().await.unwrap_or_default();

    let id = parse_initiate_response(&body).ok_or_else(|| IaError::UploadFailed {
        identifier: identifier.into(),
        key: key.into(),
        message: "initiate response missing UploadId".into(),
        status: None,
    })?;
    Ok((id, attempts))
}

/// Upload a single part. Returns the ETag for the completion manifest.
///
/// `PUT /{identifier}/{key}?partNumber={N}&uploadId={ID}`
///
/// IA's S3 does not return an `ETag` header on part PUTs; its completion
/// check compares the manifest entry against the part's MD5. When the header
/// is absent, the quoted hex MD5 of the body is used instead.
///
/// Retries per the shared IA-S3 policy with the default budget. Callers that
/// need a configurable budget go through `upload_file_multipart`.
pub async fn upload_part(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    part_number: u32,
    body: Vec<u8>,
) -> Result<String> {
    let body = Bytes::from(body);
    let ctx = S3RetryCtx {
        total_bytes: body.len() as u64,
        ..default_ctx(identifier, key)
    };
    upload_part_with_retry(client, &ctx, upload_id, part_number, body)
        .await
        .map(|(etag, _attempts)| etag)
        .map_err(IaError::from)
}

/// Upload one part, retrying per the shared IA-S3 policy.
///
/// The body is a `Bytes` so that rebuilding the request on each attempt is a
/// refcount bump, not a copy of the part. Holding the part across the backoff
/// is deliberate: re-reading it from disk would cost an I/O round trip on
/// every attempt, including the common single-attempt case, and the caller
/// already has the buffer in hand.
///
/// Returns the [`S3Failure`] rather than a flattened error so the part loop
/// can tell a permanent refusal (the S3 code) from a spent budget (the
/// attempt count) and word its message accordingly.
pub(crate) async fn upload_part_with_retry(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    upload_id: &str,
    part_number: u32,
    body: Bytes,
) -> std::result::Result<(String, u32), S3Failure> {
    let (access, secret) = client.require_auth().map_err(|e| S3Failure {
        error: Box::new(e),
        code: None,
        attempts: 0,
    })?;
    let url = format!(
        "{}?partNumber={}&uploadId={}",
        build_s3_url(client, ctx.identifier, ctx.key),
        part_number,
        upload_id,
    );
    let content_length = body.len();
    let local_md5 = {
        use md5::{Digest, Md5};
        format!("\"{:x}\"", Md5::digest(&body))
    };

    // The part goes out as a watched stream of 64 KiB slices (no copy), so a
    // server that stops reading is caught by the stall detector. The
    // explicit Content-Length keeps the transfer unchunked, as IA requires.
    let sent = send_with_retry(ctx, &format!("upload part {part_number}"), |watch| {
        let stream = watch.wrap(bytes_chunks(body.clone()), content_length as u64);
        client
            .upload_http()
            .put(&url)
            .header("Authorization", format!("LOW {access}:{secret}"))
            .header("Content-Length", content_length.to_string())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
    })
    .await?;

    // Prefer the server's ETag; IA omits it, so fall back to the local MD5.
    let etag = sent
        .response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or(local_md5);
    Ok((etag, sent.attempts))
}

/// Complete a multipart upload by sending the manifest.
///
/// `POST /{identifier}/{key}?uploadId={ID}` with XML body
///
/// Retries per the shared IA-S3 policy with the default budget. Callers that
/// need a configurable budget go through `upload_file_multipart`.
pub async fn complete_upload(
    client: &IaClient,
    identifier: &str,
    key: &str,
    upload_id: &str,
    parts: &[(u32, String)],
    keep_old_version: bool,
) -> Result<()> {
    complete_upload_with_retry(
        client,
        &default_ctx(identifier, key),
        upload_id,
        parts,
        keep_old_version,
        None,
    )
    .await
    .map(|_attempts| ())
}

/// Finish a multipart upload, retrying per the shared IA-S3 policy.
///
/// Handles the one case where a retry changes the meaning of the answer. If
/// the completion applied and only the response was lost, the next attempt
/// gets `NoSuchUpload`, because a completed upload is no longer in progress.
/// Treating that as failure is wrong twice over: the upload succeeded, and
/// the caller's recovery is to re-upload the whole file, since
/// `list_uploads` cannot see a completed upload either.
///
/// `NoSuchUpload` is only evidence of that on a later attempt. On the first
/// attempt it means what it says — unknown or already-aborted upload id — and
/// is surfaced.
pub(crate) async fn complete_upload_with_retry(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    upload_id: &str,
    parts: &[(u32, String)],
    keep_old_version: bool,
    expected_size: Option<u64>,
) -> Result<u32> {
    let (access, secret) = client.require_auth()?;
    let (identifier, key) = (ctx.identifier, ctx.key);
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, identifier, key),
        upload_id,
    );

    let manifest = build_complete_manifest(parts);
    let result = send_with_retry(ctx, "complete multipart", |_watch| {
        let mut req = client
            .upload_http()
            .post(&url)
            .header("Authorization", format!("LOW {access}:{secret}"))
            .header("Content-Type", "application/xml")
            .header("Content-Length", manifest.len().to_string());
        if keep_old_version {
            req = req.header("x-archive-keep-old-version", "1");
        }
        req.body(manifest.clone()).send()
    })
    .await;

    match result {
        Ok(sent) => Ok(sent.attempts),
        // NoSuchUpload after a retry may mean an earlier attempt applied and
        // only its response was lost: a completed upload is no longer in
        // progress. The retry alone does not prove that, because every
        // retry trigger (a dropped connection, a 503 SlowDown) means the
        // earlier attempt was not applied. So ask the item whether the
        // object is actually there before calling this a success.
        Err(f) if f.code.as_deref() == Some("NoSuchUpload") && f.attempts > 1 => {
            if object_landed(client, identifier, key, expected_size).await {
                tracing::debug!(
                    identifier,
                    key,
                    %upload_id,
                    attempts = f.attempts,
                    "completion applied on an earlier attempt; object is present"
                );
                Ok(f.attempts)
            } else {
                tracing::warn!(
                    identifier,
                    key,
                    %upload_id,
                    attempts = f.attempts,
                    "upload id is gone and the object is not in the item; the upload did not complete"
                );
                Err(*f.error)
            }
        }
        Err(f) => Err(*f.error),
    }
}

/// Whether the item's metadata lists `key`, at `expected_size` when known.
///
/// Used only to disambiguate a `NoSuchUpload` after a retried completion.
/// A metadata fetch failure counts as "not there": the caller then reports
/// the completion as failed, which is the conservative outcome.
async fn object_landed(
    client: &IaClient,
    identifier: &str,
    key: &str,
    expected_size: Option<u64>,
) -> bool {
    match client.get_item(identifier).await {
        Ok(item) => item
            .files
            .iter()
            .any(|f| f.name == key && expected_size.is_none_or(|want| f.size == Some(want))),
        Err(e) => {
            tracing::debug!(identifier, key, error = %e, "could not fetch item metadata to confirm completion");
            false
        }
    }
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
    abort_upload_with_ctx(client, &default_ctx(identifier, key), upload_id).await
}

/// Abort a multipart upload with the caller's retry budget.
pub(crate) async fn abort_upload_with_ctx(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    upload_id: &str,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let url = format!(
        "{}?uploadId={}",
        build_s3_url(client, ctx.identifier, ctx.key),
        upload_id,
    );

    let result = send_with_retry(ctx, "abort multipart", |_watch| {
        client
            .upload_http()
            .delete(&url)
            .header("Authorization", format!("LOW {access}:{secret}"))
            .send()
    })
    .await;
    match result {
        Ok(_) => Ok(()),
        // Already gone (completed, expired, or aborted by another run). The
        // caller wanted it gone; `ia upload cleanup --abort-all` must not
        // stop at the first upload that no longer exists.
        Err(f) if f.code.as_deref() == Some("NoSuchUpload") => {
            tracing::debug!(
                identifier = ctx.identifier,
                key = ctx.key,
                %upload_id,
                "multipart upload already gone; nothing to abort"
            );
            Ok(())
        }
        Err(f) => Err(*f.error),
    }
}

/// Retry context for the public S3 wrappers, which take no caller-supplied
/// budget: `initiate_upload`, `upload_part`, `complete_upload`,
/// `abort_upload`, `list_uploads` and `list_parts`.
///
/// These carry no progress of their own, but they must not silently lose the
/// retries they had while the middleware was doing it for them. The wrappers
/// are consumed outside this workspace, where a signature that still
/// compiles would otherwise hide the loss. Inside `upload_file_multipart`
/// every call uses the caller's `--retries` and backoff bounds instead.
fn default_ctx<'a>(identifier: &'a str, key: &'a str) -> S3RetryCtx<'a> {
    S3RetryCtx {
        identifier,
        key,
        retries: DEFAULT_RETRIES,
        backoff: super::retry::backoff_policy(
            DEFAULT_RETRY_MIN_DELAY,
            DEFAULT_RETRY_MAX_DELAY,
            DEFAULT_RETRIES,
        ),
        bytes_sent: 0,
        total_bytes: 0,
        progress: None,
    }
}

/// Matches the attempt count the retry middleware used to give these calls.
const DEFAULT_RETRIES: u32 = 3;
const DEFAULT_RETRY_MIN_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
const DEFAULT_RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// List all in-progress multipart uploads for an item.
///
/// `GET /{identifier}?uploads`
///
/// Returns an empty list when the item does not exist yet (`NoSuchBucket`),
/// so callers can fall through to a fresh initiate on a new item.
pub async fn list_uploads(client: &IaClient, identifier: &str) -> Result<Vec<MultipartUploadInfo>> {
    list_uploads_with_ctx(client, &default_ctx(identifier, "")).await
}

/// Whether a listing page says more follows, and the marker for the next
/// request: `<IsTruncated>true</IsTruncated>` plus the named marker
/// element. S3 pages at 1000 entries; whether IA does is unknown, so the
/// protocol is followed either way. A page that is truncated but gives no
/// marker ends the walk with what was read rather than asking for the same
/// page again.
fn next_page_marker(body: &str, marker_tag: &str) -> Option<String> {
    let truncated =
        extract_xml_field(body, "IsTruncated").is_some_and(|t| t.eq_ignore_ascii_case("true"));
    if !truncated {
        return None;
    }
    extract_xml_field(body, marker_tag).filter(|m| !m.is_empty())
}

/// List in-progress uploads with the caller's retry budget, following
/// pagination (`key-marker` and `upload-id-marker`).
pub(crate) async fn list_uploads_with_ctx(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
) -> Result<Vec<MultipartUploadInfo>> {
    let identifier = ctx.identifier;
    let (access, secret) = client.require_auth()?;
    let base = format!("{}?uploads", build_s3_item_url(client, identifier));

    let mut uploads = Vec::new();
    let mut marker: Option<(String, String)> = None;
    loop {
        let url = match &marker {
            Some((key, id)) => format!(
                "{base}&key-marker={}&upload-id-marker={}",
                urlencoding::encode(key),
                urlencoding::encode(id)
            ),
            None => base.clone(),
        };
        let result = send_with_retry(ctx, "list multipart uploads", |_watch| {
            client
                .upload_http()
                .get(&url)
                .header("Authorization", format!("LOW {access}:{secret}"))
                .send()
        })
        .await;

        let body = match result {
            Ok(sent) => sent.response.text().await.unwrap_or_default(),
            // A brand-new item has no bucket yet, so there is nothing in
            // progress to list. Treat that as an empty result; the initiate
            // POST that follows carries x-archive-auto-make-bucket and
            // creates the item.
            Err(f) if f.code.as_deref() == Some("NoSuchBucket") => {
                tracing::debug!(
                    identifier,
                    "item does not exist yet; no multipart uploads to resume"
                );
                return Ok(Vec::new());
            }
            Err(f) => return Err(*f.error),
        };
        uploads.extend(parse_list_uploads_response(&body));
        let next_key = next_page_marker(&body, "NextKeyMarker");
        let next_id = next_page_marker(&body, "NextUploadIdMarker");
        match (next_key, next_id) {
            // A marker equal to the one just sent would fetch the same
            // page forever; stop with what was read.
            (Some(key), Some(id)) if marker.as_ref() != Some(&(key.clone(), id.clone())) => {
                marker = Some((key, id))
            }
            _ => return Ok(uploads),
        }
    }
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
    list_parts_with_ctx(client, &default_ctx(identifier, key), upload_id).await
}

/// List completed parts with the caller's retry budget, following
/// pagination (`part-number-marker`).
pub(crate) async fn list_parts_with_ctx(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    upload_id: &str,
) -> Result<Vec<PartInfo>> {
    let (access, secret) = client.require_auth()?;
    let base = format!(
        "{}?uploadId={}",
        build_s3_url(client, ctx.identifier, ctx.key),
        upload_id,
    );

    let mut parts = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let url = match &marker {
            Some(m) => format!("{base}&part-number-marker={}", urlencoding::encode(m)),
            None => base.clone(),
        };
        let sent = send_with_retry(ctx, "list parts", |_watch| {
            client
                .upload_http()
                .get(&url)
                .header("Authorization", format!("LOW {access}:{secret}"))
                .send()
        })
        .await?;
        let body = sent.response.text().await.unwrap_or_default();
        parts.extend(parse_list_parts_response(&body));
        match next_page_marker(&body, "NextPartNumberMarker") {
            // A marker equal to the one just sent would fetch the same
            // page forever; stop with what was read.
            Some(m) if marker.as_deref() != Some(m.as_str()) => marker = Some(m),
            _ => return Ok(parts),
        }
    }
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
///
/// A part that fails for good, whether IA refused it or its retry budget
/// ran out, does not abort the upload: the parts IA already holds stay, the
/// error names the upload ID, a rerun resumes from those parts, and
/// `ia upload cleanup` discards them (#18). Killing the process never
/// aborted, so the two paths now behave alike.
///
/// `part_size` controls the split size. Use [`DEFAULT_PART_SIZE`] for production.
/// A smaller value can be passed for testing.
///
/// `is_first_file` / `is_last_file` / `size_hint` control the same IA S3
/// headers as the single-PUT path (`x-archive-auto-make-bucket`,
/// `x-archive-queue-derive`, `x-archive-size-hint`), plus metadata headers
/// on the initiate POST.
///
/// `hashes` is the file's md5 and per-part md5s at `part_size` when the
/// caller already read the file (`upload_file` does, for the skip check);
/// `None` when it did not (`--clobber --no-verify`). With hashes whose
/// `parts` is empty (a md5 from `--checksums`), a resume check hashes the
/// file itself. The whole-file md5, when known, becomes `UploadResult.md5`.
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
    hashes: Option<&FileHashes>,
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
    let control_ctx = S3RetryCtx {
        identifier,
        key,
        retries: opts.retries,
        backoff: opts.backoff(),
        bytes_sent: 0,
        total_bytes: file_size,
        progress: progress.clone(),
    };

    // Dry run: report what would happen without contacting the server
    if opts.dry_run {
        return Ok(UploadResult {
            identifier: identifier.into(),
            key: key.into(),
            status: UploadStatus::DryRun,
            bytes: file_size,
            md5: hashes.map(|h| h.md5.clone()),
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

    // Try to resume an existing upload whose parts match this file.
    let part_md5s = hashes
        .filter(|h| !h.parts.is_empty())
        .map(|h| h.parts.as_slice());
    let (upload_id, existing_parts) =
        try_resume(client, &control_ctx, file, file_size, part_size, part_md5s).await?;

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

    // Retries across every request of this upload, for UploadResult.
    let mut total_retries = 0u32;

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
            let (id, attempts) =
                initiate_upload_with_retry(client, &control_ctx, &extra_headers).await?;
            total_retries += attempts.saturating_sub(1);
            tracing::debug!(identifier, key, upload_id = %id, "initiated multipart upload");
            (id, Vec::new())
        }
    };

    // Compute part boundaries
    let part_count = file_size.div_ceil(part_size).max(1) as u32;

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

        // Retry lives in upload_part_with_retry, which uses the same policy
        // as every other IA-S3 request. This loop owns orchestration only:
        // progress, accounting, and the message when a part is lost.
        let data = read_file_range(file, offset, this_part_size).await?;
        tracing::debug!(
            identifier,
            key,
            part = part_num,
            of = part_count,
            bytes = this_part_size,
            "uploading part"
        );
        let ctx = S3RetryCtx {
            identifier,
            key,
            retries: opts.retries,
            backoff: opts.backoff(),
            bytes_sent: offset,
            total_bytes: file_size,
            progress: progress.clone(),
        };
        let etag =
            match upload_part_with_retry(client, &ctx, &upload_id, part_num, Bytes::from(data))
                .await
            {
                Ok((etag, attempts)) => {
                    // attempts counts the first try, so retries is one fewer.
                    total_retries += attempts.saturating_sub(1);
                    tracing::debug!(identifier, key, part = part_num, %etag, "part uploaded");
                    etag
                }
                Err(failure) => {
                    // Not aborted: an abort would delete every part IA holds,
                    // which is what multipart exists to avoid. The error says
                    // where the upload stands and both ways forward (#18).
                    let kept = KeptUpload {
                        identifier,
                        key,
                        upload_id: &upload_id,
                        part_num,
                        part_count,
                        parts_on_ia: completed_parts.len(),
                    };
                    let err = kept.describe(failure);
                    tracing::warn!(
                        identifier,
                        key,
                        %upload_id,
                        part = part_num,
                        parts_on_ia = completed_parts.len(),
                        "multipart part failed; upload kept on IA for resume or cleanup"
                    );
                    return Err(err);
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
    // Every byte has been sent by now; a backoff here must say so.
    let completion_ctx = S3RetryCtx {
        bytes_sent: file_size,
        ..control_ctx
    };
    let completion_attempts = complete_upload_with_retry(
        client,
        &completion_ctx,
        &upload_id,
        &completed_parts,
        keep_old_version,
        Some(file_size),
    )
    .await?;
    total_retries += completion_attempts.saturating_sub(1);

    // IA compared every part's md5 from the manifest with the part it holds
    // before answering 2xx, so the completion is the upload.
    if let Some(ref cb) = progress {
        cb(UploadProgress {
            identifier: identifier.into(),
            key: key.into(),
            bytes_sent: file_size,
            total_bytes: file_size,
            status: UploadProgressStatus::Complete,
        });
    }

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
        md5: hashes.map(|h| h.md5.clone()),
        elapsed_ms: start.elapsed().as_millis() as u64,
        retries: total_retries,
    })
}

/// What the user needs to know when a part fails for good and the upload is
/// left on IA: which part, why, the upload ID, how many parts IA holds, and
/// the two ways forward.
struct KeptUpload<'a> {
    identifier: &'a str,
    key: &'a str,
    upload_id: &'a str,
    part_num: u32,
    part_count: u32,
    parts_on_ia: usize,
}

impl KeptUpload<'_> {
    /// The error for a part failure. Two wordings, chosen on the S3 code:
    /// a permanent refusal (`AccessDenied`, `InvalidAccessKeyId`,
    /// `BadDigest`, ...) says "refused by IA" and asks the user to fix the
    /// cause before rerunning; a spent budget says how many attempts were
    /// made. Both carry the upload ID and name `ia upload cleanup ... --abort`.
    ///
    /// An `UploadFailed` is reworded as above; an `UploadStalled` (#38) as
    /// "part N of M stalled K times (no bytes sent for W s)". Anything
    /// else the part request produced, in practice IA's spam rejection
    /// (`SpamDetected`), is fatal for the whole item and passes through
    /// unchanged so the item loop still stops on it.
    fn describe(&self, failure: S3Failure) -> IaError {
        let refused = failure
            .code
            .as_deref()
            .is_some_and(|code| !super::s3_error::is_retryable_code(code));
        let (status, detail) = match *failure.error {
            IaError::UploadFailed {
                status, message, ..
            } => (
                status,
                Self::detail(&message, self.part_num, failure.attempts),
            ),
            IaError::UploadStalled {
                window_secs,
                stalls,
                ..
            } => {
                let kept = self.kept_sentence();
                // Attempts can exceed stalls when an earlier attempt failed
                // some other way; say so, as the UploadFailed arm does.
                let attempts = if failure.attempts > 1 {
                    format!(", after {} attempts", failure.attempts)
                } else {
                    String::new()
                };
                return IaError::UploadFailed {
                    identifier: self.identifier.into(),
                    key: self.key.into(),
                    message: format!(
                        "part {} of {} stalled {stalls} {} (no bytes sent for {window_secs} s{attempts}): \
                         multipart upload {} {kept}; rerun the same command to resume, or discard it \
                         with: ia upload cleanup {} {} --abort",
                        self.part_num,
                        self.part_count,
                        if stalls == 1 { "time" } else { "times" },
                        self.upload_id,
                        self.identifier,
                        shell_word(self.key)
                    ),
                    status: None,
                };
            }
            other => return other,
        };
        let kept = self.kept_sentence();
        let what = if refused {
            format!(
                "part {} of {} refused by IA ({detail})",
                self.part_num, self.part_count
            )
        } else if failure.attempts > 1 {
            format!(
                "part {} of {} failed after {} attempts ({detail})",
                self.part_num, self.part_count, failure.attempts
            )
        } else {
            format!(
                "part {} of {} failed ({detail})",
                self.part_num, self.part_count
            )
        };
        let fix = if refused { "fix the cause and " } else { "" };
        IaError::UploadFailed {
            identifier: self.identifier.into(),
            key: self.key.into(),
            message: format!(
                "{what}: multipart upload {} {kept}; {fix}rerun the same command to resume, \
                 or discard it with: ia upload cleanup {} {} --abort",
                self.upload_id,
                self.identifier,
                shell_word(self.key)
            ),
            status,
        }
    }

    /// "is kept with N parts on IA", or "is kept on IA with no parts yet".
    fn kept_sentence(&self) -> String {
        match self.parts_on_ia {
            0 => "is kept on IA with no parts yet".to_string(),
            1 => "is kept with 1 part on IA".to_string(),
            n => format!("is kept with {n} parts on IA"),
        }
    }

    /// The failure's own text without the wrapping `send_with_retry` adds
    /// (the "upload part N failed: " or "upload part N: " prefix and the
    /// "(after N attempts)" suffix), since the message built here says
    /// both in its own words.
    fn detail(message: &str, part_num: u32, attempts: u32) -> String {
        let mut text = message;
        for prefix in [
            format!("upload part {part_num} failed: "),
            format!("upload part {part_num}: "),
        ] {
            if let Some(rest) = text.strip_prefix(prefix.as_str()) {
                text = rest;
            }
        }
        let suffix = format!(" (after {attempts} attempts)");
        text.strip_suffix(suffix.as_str())
            .unwrap_or(text)
            .to_string()
    }
}

/// `word` as it can be pasted into a shell: as is when it is a plain word,
/// in single quotes when it holds whitespace or quote characters (a remote
/// key may, with `--remote-dir` or `--keep-directories`).
fn shell_word(word: &str) -> String {
    if word
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\' | '$' | '`'))
    {
        format!("'{}'", word.replace('\'', "'\\''"))
    } else {
        word.to_string()
    }
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

/// Find an in-progress upload for this key whose parts match the local file,
/// and return it with those parts.
///
/// Candidates are the item's unfinished uploads for `ctx.key`, newest first
/// by their `Initiated` time (ties and missing times keep the listing's
/// reverse order, S3 listing chronologically). Each candidate's parts are
/// checked with [`validate_parts`] against the local file's size and the
/// md5 of each local range; the first candidate that validates is resumed.
/// One that does not is left in place (an abort would be #18's mistake
/// again) and reported at warn with its upload ID, so `ia upload cleanup`
/// can discard it. With no valid candidate, `None`: the caller initiates a
/// fresh upload.
///
/// The local part md5s are `part_md5s` when the caller already read the
/// file; otherwise they come from one read here
/// ([`super::checksum::hash_file_and_parts`]), done only when there is a
/// candidate to check.
async fn try_resume(
    client: &IaClient,
    ctx: &S3RetryCtx<'_>,
    file: &Path,
    file_size: u64,
    part_size: u64,
    part_md5s: Option<&[String]>,
) -> Result<(Option<String>, Vec<PartInfo>)> {
    let uploads = list_uploads_with_ctx(client, ctx).await?;
    let mut candidates: Vec<&MultipartUploadInfo> =
        uploads.iter().rev().filter(|u| u.key == ctx.key).collect();
    // ISO 8601 timestamps sort as strings; the sort is stable.
    candidates.sort_by(|a, b| b.initiated.cmp(&a.initiated));
    if candidates.is_empty() {
        return Ok((None, Vec::new()));
    }

    let hashed;
    let local: &[String] = match part_md5s {
        Some(md5s) => md5s,
        None => {
            hashed = super::checksum::hash_file_and_parts_async(file, part_size).await?;
            &hashed.parts
        }
    };
    for info in candidates {
        let parts = list_parts_with_ctx(client, ctx, &info.upload_id).await?;
        match validate_parts(&parts, file_size, part_size, local) {
            Ok(()) => return Ok((Some(info.upload_id.clone()), parts)),
            Err(reason) => tracing::warn!(
                identifier = ctx.identifier,
                key = ctx.key,
                upload_id = %info.upload_id,
                "not resuming multipart upload {}: {reason}; it is left on IA, discard it \
                 with: ia upload cleanup {} {} --abort",
                info.upload_id,
                ctx.identifier,
                shell_word(ctx.key)
            ),
        }
    }
    Ok((None, Vec::new()))
}

/// Whether every part IA lists for an upload matches the local file.
///
/// For each part: its number must be within the file's part count at
/// `part_size`; its size, when the listing gave one (0 means it did not),
/// must be the expected size of that part (`part_size`, or what is left of
/// the file for the last part); and its ETag, quotes stripped and case
/// ignored, must equal the md5 of the local range (`local[n - 1]`). A part
/// number listed twice is a mismatch. `Err` names the first offending part
/// and why.
fn validate_parts(
    parts: &[PartInfo],
    file_size: u64,
    part_size: u64,
    local: &[String],
) -> std::result::Result<(), String> {
    let part_count = file_size.div_ceil(part_size).max(1);
    let mut seen = std::collections::HashSet::new();
    for part in parts {
        let n = part.part_number;
        if n == 0 || u64::from(n) > part_count {
            return Err(format!(
                "part {n} is outside this file's {part_count} parts of {part_size} bytes"
            ));
        }
        if !seen.insert(n) {
            return Err(format!("part {n} is listed twice"));
        }
        let offset = u64::from(n - 1) * part_size;
        let expected = part_size.min(file_size - offset);
        if part.size != 0 && part.size != expected {
            return Err(format!(
                "part {n} is {} bytes on IA but {expected} bytes locally",
                part.size
            ));
        }
        let etag = part.etag.trim().trim_matches('"').to_ascii_lowercase();
        match local.get((n - 1) as usize) {
            Some(md5) if *md5 == etag => {}
            _ => {
                return Err(format!(
                    "part {n} has ETag {etag} on IA but the local range's md5 differs"
                ))
            }
        }
    }
    Ok(())
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

    // -- KeptUpload::describe: the message for a part that failed for good --

    fn kept<'a>(
        part_num: u32,
        part_count: u32,
        parts_on_ia: usize,
        key: &'a str,
    ) -> KeptUpload<'a> {
        KeptUpload {
            identifier: "item",
            key,
            upload_id: "mp-1",
            part_num,
            part_count,
            parts_on_ia,
        }
    }

    fn failure(message: &str, status: Option<u16>, code: Option<&str>, attempts: u32) -> S3Failure {
        S3Failure {
            error: Box::new(IaError::UploadFailed {
                identifier: "item".into(),
                key: "f.bin".into(),
                message: message.into(),
                status,
            }),
            code: code.map(str::to_string),
            attempts,
        }
    }

    #[test]
    fn describe_transport_failure_past_the_budget() {
        let err = kept(2, 3, 1, "f.bin").describe(failure(
            "upload part 2: connection reset by peer (after 3 attempts)",
            None,
            None,
            3,
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("part 2 of 3 failed after 3 attempts (connection reset by peer): "),
            "{msg}"
        );
        assert!(msg.contains("multipart upload mp-1 is kept with 1 part on IA; rerun the same command to resume, or discard it with: ia upload cleanup item f.bin --abort"), "{msg}");
        assert!(!msg.contains("fix the cause"), "{msg}");
    }

    #[test]
    fn describe_single_attempt_has_no_attempt_count() {
        let err = kept(1, 1, 0, "f.bin").describe(failure(
            "upload part 1 failed: SlowDown: Please reduce your request rate.",
            Some(503),
            Some("SlowDown"),
            1,
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("part 1 of 1 failed (SlowDown: Please reduce your request rate.): "),
            "{msg}"
        );
        assert!(!msg.contains("after 1 attempts"), "{msg}");
    }

    #[test]
    fn describe_with_no_parts_on_ia_says_so() {
        let err = kept(1, 4, 0, "f.bin").describe(failure(
            "upload part 1 failed: InternalError: boom (after 11 attempts)",
            Some(500),
            Some("InternalError"),
            11,
        ));
        let msg = err.to_string();
        assert!(msg.contains("is kept on IA with no parts yet;"), "{msg}");
        assert!(!msg.contains("0 parts"), "{msg}");
    }

    #[test]
    fn describe_refusal_asks_to_fix_the_cause_and_quotes_an_awkward_key() {
        let err = kept(3, 7, 2, "dir/my file.bin").describe(failure(
            "upload part 3 failed: AccessDenied: Access Denied",
            Some(403),
            Some("AccessDenied"),
            1,
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("part 3 of 7 refused by IA (AccessDenied: Access Denied): "),
            "{msg}"
        );
        assert!(
            msg.contains("kept with 2 parts on IA; fix the cause and rerun"),
            "{msg}"
        );
        assert!(
            msg.ends_with("ia upload cleanup item 'dir/my file.bin' --abort"),
            "{msg}"
        );
        assert!(matches!(
            err,
            IaError::UploadFailed {
                status: Some(403),
                ..
            }
        ));
    }

    /// A stalled part is a failed part: the upload is kept on IA and the
    /// message carries the stall detail plus both ways forward.
    #[test]
    fn describe_stalled_part_is_a_kept_upload() {
        let stalled = S3Failure {
            error: Box::new(IaError::UploadStalled {
                identifier: "item".into(),
                key: "f.bin".into(),
                window_secs: 60,
                stalls: 2,
            }),
            code: None,
            attempts: 3,
        };
        let err = kept(2, 3, 1, "f.bin").describe(stalled);
        let msg = err.to_string();
        assert!(
            msg.contains(
                "part 2 of 3 stalled 2 times (no bytes sent for 60 s, after 3 attempts): "
            ),
            "{msg}"
        );
        assert!(msg.contains("multipart upload mp-1 is kept with 1 part on IA; rerun the same command to resume, or discard it with: ia upload cleanup item f.bin --abort"), "{msg}");
        assert!(
            matches!(err, IaError::UploadFailed { status: None, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn describe_passes_other_errors_through_unchanged() {
        let spam = S3Failure {
            error: Box::new(IaError::SpamDetected {
                identifier: "item".into(),
            }),
            code: None,
            attempts: 1,
        };
        assert!(matches!(
            kept(2, 2, 1, "f.bin").describe(spam),
            IaError::SpamDetected { .. }
        ));
    }

    // -- validate_parts: a listed part is reused only when it matches --

    fn part(n: u32, etag: &str, size: u64) -> PartInfo {
        PartInfo {
            part_number: n,
            etag: etag.to_string(),
            size,
        }
    }

    fn local() -> Vec<String> {
        vec!["aa".repeat(16), "bb".repeat(16), "cc".repeat(16)]
    }

    #[test]
    fn validate_parts_accepts_matching_parts() {
        let parts = [
            part(1, &format!("\"{}\"", "aa".repeat(16)), 10),
            part(3, &"cc".repeat(16), 5),
        ];
        assert_eq!(validate_parts(&parts, 25, 10, &local()), Ok(()));
    }

    #[test]
    fn validate_parts_ignores_etag_quotes_and_case() {
        let parts = [part(2, &format!("\"{}\"", "BB".repeat(16)), 10)];
        assert_eq!(validate_parts(&parts, 25, 10, &local()), Ok(()));
    }

    #[test]
    fn validate_parts_rejects_a_wrong_md5() {
        let parts = [part(1, &"dd".repeat(16), 10)];
        let reason = validate_parts(&parts, 25, 10, &local()).unwrap_err();
        assert!(
            reason.contains("part 1") && reason.contains("md5"),
            "{reason}"
        );
    }

    #[test]
    fn validate_parts_rejects_a_wrong_size() {
        let parts = [part(1, &"aa".repeat(16), 9)];
        let reason = validate_parts(&parts, 25, 10, &local()).unwrap_err();
        assert!(
            reason.contains("part 1") && reason.contains("9 bytes"),
            "{reason}"
        );
        // The last part is shorter; its expected size is what is left.
        let parts = [part(3, &"cc".repeat(16), 10)];
        assert!(validate_parts(&parts, 25, 10, &local()).is_err());
    }

    #[test]
    fn validate_parts_rejects_an_out_of_range_part_number() {
        assert!(validate_parts(&[part(0, &"aa".repeat(16), 10)], 25, 10, &local()).is_err());
        assert!(validate_parts(&[part(4, &"aa".repeat(16), 10)], 25, 10, &local()).is_err());
    }

    #[test]
    fn validate_parts_rejects_a_duplicate_part_number() {
        let parts = [part(1, &"aa".repeat(16), 10), part(1, &"aa".repeat(16), 10)];
        let reason = validate_parts(&parts, 25, 10, &local()).unwrap_err();
        assert!(reason.contains("twice"), "{reason}");
    }

    #[test]
    fn validate_parts_rejects_a_part_from_a_different_part_size() {
        // A 15-byte part can only come from an upload made with another part
        // size; at 10 bytes per part it is wrong by size.
        let parts = [part(1, &"aa".repeat(16), 15)];
        assert!(validate_parts(&parts, 25, 10, &local()).is_err());
    }

    #[test]
    fn validate_parts_handles_an_empty_file() {
        const EMPTY: &str = "d41d8cd98f00b204e9800998ecf8427e";
        let local = vec![EMPTY.to_string()];
        assert_eq!(validate_parts(&[part(1, EMPTY, 0)], 0, 10, &local), Ok(()));
        // With an expected size of 0, a listed size of 5 is a real mismatch,
        // not "size not given".
        assert!(validate_parts(&[part(1, EMPTY, 5)], 0, 10, &local).is_err());
    }

    #[test]
    fn validate_parts_rejects_a_composite_etag() {
        let parts = [part(1, "\"abc-3\"", 10)];
        let reason = validate_parts(&parts, 25, 10, &local()).unwrap_err();
        assert!(reason.contains("ETag abc-3"), "{reason}");
    }

    #[test]
    fn validate_parts_treats_a_missing_size_as_not_given() {
        let parts = [part(1, &"aa".repeat(16), 0)];
        assert_eq!(validate_parts(&parts, 25, 10, &local()), Ok(()));
    }
}
