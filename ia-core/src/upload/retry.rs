//! The retry policy for IA-S3 requests.
//!
//! Every upload request goes through [`send_with_retry`]: single-file PUTs,
//! multipart part PUTs, and the multipart control calls. They speak the same
//! protocol to the same endpoint, so they get one policy in one place rather
//! than a copy per call site that drifts.
//!
//! Retryability is decided by [`should_retry_s3`], which reads the S3 error
//! `<Code>` rather than the HTTP status. IA returns 503 both for `SlowDown`
//! (throttled, never applied, retry is right) and for real faults, so status
//! alone cannot tell those apart.

use std::future::Future;
use std::sync::Arc;

use super::s3_error::{parse_s3_error, should_retry_s3, strip_xml};
use super::types::{UploadProgress, UploadProgressStatus};
use crate::error::IaError;

/// What a retrying S3 call needs to know to report itself.
pub(crate) struct S3RetryCtx<'a> {
    pub identifier: &'a str,
    pub key: &'a str,
    /// Maximum retry attempts after the first try.
    pub retries: u32,
    pub retry_sleep: std::time::Duration,
    /// Bytes already sent, for the progress callback during a backoff.
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
}

/// Outcome of a retrying S3 request.
pub(crate) struct S3Response {
    pub response: reqwest::Response,
    /// Attempts made, counting the first. 1 means it succeeded immediately.
    pub attempts: u32,
}

/// A request that ran out of attempts or hit a non-retryable condition.
///
/// Carries the S3 error `<Code>` and the attempt count so callers can act on
/// the specific failure. `complete_upload` needs both: `NoSuchUpload` means
/// "already completed" only if we know a previous attempt was sent.
pub(crate) struct S3Failure {
    pub error: IaError,
    /// S3 error `<Code>`, when the body was a parseable S3 error.
    pub code: Option<String>,
    /// Attempts made, counting the first.
    pub attempts: u32,
}

impl From<S3Failure> for IaError {
    fn from(f: S3Failure) -> Self {
        f.error
    }
}

/// Send an IA-S3 request, retrying per [`should_retry_s3`].
///
/// `send` must build and send the request afresh on each call. Rebuilding is
/// what lets a part PUT re-read its slice from disk per attempt instead of
/// holding a 100 MiB buffer across the backoff.
///
/// Returns the first successful response, or the error from the final
/// attempt. `context` names the operation for the error message, e.g.
/// `"upload part 3"`.
pub(crate) async fn send_with_retry<F, Fut>(
    ctx: &S3RetryCtx<'_>,
    context: &str,
    mut send: F,
) -> std::result::Result<S3Response, S3Failure>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest_middleware::Error>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;

        let response = match send().await {
            Ok(r) => r,
            Err(e) => {
                // Transport-level failure. A connection that was never
                // established never reached the server, so it is safe to
                // replay; anything else surfaces.
                let is_connect = matches!(
                    &e,
                    reqwest_middleware::Error::Reqwest(re) if re.is_connect()
                );
                if is_connect && attempt <= ctx.retries {
                    report_backoff(ctx);
                    tokio::time::sleep(ctx.retry_sleep).await;
                    continue;
                }
                return Err(S3Failure {
                    error: IaError::UploadFailed {
                        identifier: ctx.identifier.into(),
                        key: ctx.key.into(),
                        message: format!("{context}: {e}"),
                        status: None,
                    },
                    code: None,
                    attempts: attempt,
                });
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(S3Response {
                response,
                attempts: attempt,
            });
        }

        // Consume the body once: it is needed both to classify and to report.
        let body = response.text().await.unwrap_or_default();

        if should_retry_s3(status, &body) && attempt <= ctx.retries {
            tracing::debug!(
                identifier = ctx.identifier,
                key = ctx.key,
                attempt,
                %status,
                "retrying {context}"
            );
            report_backoff(ctx);
            tokio::time::sleep(ctx.retry_sleep).await;
            continue;
        }

        let parsed = parse_s3_error(&body);
        let detail = parsed
            .as_ref()
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| format!("HTTP {status}: {}", strip_xml(&body)));

        return Err(S3Failure {
            error: IaError::UploadFailed {
                identifier: ctx.identifier.into(),
                key: ctx.key.into(),
                message: format!("{context} failed: {detail}"),
                status: Some(status.as_u16()),
            },
            code: parsed.map(|e| e.code),
            attempts: attempt,
        });
    }
}

/// Tell the caller's UI that the request is sleeping before another attempt.
fn report_backoff(ctx: &S3RetryCtx<'_>) {
    if let Some(ref cb) = ctx.progress {
        cb(UploadProgress {
            identifier: ctx.identifier.into(),
            key: ctx.key.into(),
            bytes_sent: ctx.bytes_sent,
            total_bytes: ctx.total_bytes,
            status: UploadProgressStatus::WaitingRateLimit,
        });
    }
}
