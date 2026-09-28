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

use reqwest_retry::{default_on_request_failure, Retryable};

use super::check_limit::is_spam_response;
use super::s3_error::{parse_s3_error, should_retry_s3, strip_xml};
use super::types::{UploadProgress, UploadProgressStatus};
use crate::error::{format_error_chain, IaError};

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
/// `send` must build and send the request afresh on each call, because a
/// `reqwest::Request` is consumed by sending. Bodies should be cheap to
/// clone: part PUTs pass a `Bytes`, so each attempt bumps a refcount rather
/// than copying the part.
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
                // Transport-level failure. Classified the way the retry
                // middleware classified it before this loop replaced it:
                // connect failures, timeouts, resets, and a connection
                // closed before the response (hyper IncompleteMessage) are
                // transient. Every request here is safe to replay: a part
                // PUT overwrites the same part number, the listings and the
                // abort are idempotent, and initiate/complete already accept
                // this exposure for 5xx responses.
                let transient =
                    matches!(default_on_request_failure(&e), Some(Retryable::Transient));
                let cause = format_error_chain(&e);
                if transient && attempt <= ctx.retries {
                    tracing::warn!(
                        identifier = ctx.identifier,
                        key = ctx.key,
                        attempt,
                        error = %cause,
                        "transport error, retrying {context}"
                    );
                    report_backoff(ctx, UploadProgressStatus::Retrying);
                    tokio::time::sleep(ctx.retry_sleep).await;
                    continue;
                }
                return Err(S3Failure {
                    error: IaError::UploadFailed {
                        identifier: ctx.identifier.into(),
                        key: ctx.key.into(),
                        message: describe_attempts(&format!("{context}: {cause}"), attempt),
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

        // IA's spam rejection is a plain-text 503 with no S3 <Code>. The
        // status fallback would retry it for the whole budget; it is
        // permanent, and the single-file path already treats it as such.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && is_spam_response(&body) {
            return Err(S3Failure {
                error: IaError::SpamDetected {
                    identifier: ctx.identifier.into(),
                },
                code: None,
                attempts: attempt,
            });
        }

        if should_retry_s3(status, &body) && attempt <= ctx.retries {
            tracing::debug!(
                identifier = ctx.identifier,
                key = ctx.key,
                attempt,
                %status,
                "retrying {context}"
            );
            // A throttle (429, or IA's 503 SlowDown) is a rate-limit wait;
            // anything else retryable is a plain retry. The progress bar
            // shows different text for the two.
            let throttled = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::SERVICE_UNAVAILABLE;
            let phase = if throttled {
                UploadProgressStatus::WaitingRateLimit
            } else {
                UploadProgressStatus::Retrying
            };
            report_backoff(ctx, phase);
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
                message: describe_attempts(&format!("{context} failed: {detail}"), attempt),
                status: Some(status.as_u16()),
            },
            code: parsed.map(|e| e.code),
            attempts: attempt,
        });
    }
}

/// Append the attempt count to a final error message when there was more
/// than one attempt, so the user can tell a first-try failure from an
/// exhausted budget.
fn describe_attempts(message: &str, attempts: u32) -> String {
    if attempts > 1 {
        format!("{message} (after {attempts} attempts)")
    } else {
        message.to_string()
    }
}

/// Tell the caller's UI that the request is sleeping before another attempt,
/// and why: `WaitingRateLimit` after a throttle, `Retrying` otherwise.
fn report_backoff(ctx: &S3RetryCtx<'_>, status: UploadProgressStatus) {
    if let Some(ref cb) = ctx.progress {
        cb(UploadProgress {
            identifier: ctx.identifier.into(),
            key: ctx.key.into(),
            bytes_sent: ctx.bytes_sent,
            total_bytes: ctx.total_bytes,
            status,
        });
    }
}
