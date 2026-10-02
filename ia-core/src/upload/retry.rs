//! The retry policy for IA-S3 multipart requests.
//!
//! Every multipart request goes through [`send_with_retry`]: initiate, part
//! PUTs, complete, abort, and the two listings. They speak the same protocol
//! to the same endpoint, so they get one policy in one place rather than a
//! copy per call site that drifts. The single-file PUT in `upload::single`
//! keeps its own loop, because its body streams from disk and cannot be
//! rebuilt by a closure; it shares the classifier below.
//!
//! Retryability of a response is decided by [`should_retry_s3`], which reads
//! the S3 error `<Code>` rather than the HTTP status. IA returns 503 both for
//! `SlowDown` (throttled, never applied, retry is right) and for real faults,
//! so status alone cannot tell those apart. Transport failures are classified
//! the way the retry middleware classified them: connect errors, timeouts,
//! resets, and a connection closed before the response are transient.

use std::future::Future;
use std::sync::Arc;

use reqwest_retry::policies::ExponentialBackoff;
use reqwest_retry::{default_on_request_failure, Retryable};

pub(crate) use crate::retry::{backoff_policy, backoff_wait, retry_after_wait, wait_before_retry};

use super::check_limit::is_spam_response;
use super::s3_error::{describe_parsed, parse_s3_error, should_retry_s3};
use super::stall_watch::{watch_send, BodyWatch, SendEnd};
use super::types::{UploadProgress, UploadProgressStatus};
use crate::error::{format_error_chain, IaError};

/// What a retrying S3 call needs to know to report itself.
pub(crate) struct S3RetryCtx<'a> {
    pub identifier: &'a str,
    pub key: &'a str,
    /// Maximum retry attempts after the first try.
    pub retries: u32,
    /// The wait schedule between attempts (see [`backoff_policy`]).
    pub backoff: ExponentialBackoff,
    /// Floor for the body send in bytes per second; 0 disables stall
    /// detection (see [`BodyWatch`]).
    pub min_speed: u64,
    /// Bytes already sent, for the progress callback during a backoff.
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
}

impl std::fmt::Debug for S3RetryCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3RetryCtx")
            .field("identifier", &self.identifier)
            .field("key", &self.key)
            .field("retries", &self.retries)
            .field("backoff", &self.backoff)
            .field("min_speed", &self.min_speed)
            .field("bytes_sent", &self.bytes_sent)
            .field("total_bytes", &self.total_bytes)
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

/// Outcome of a retrying S3 request.
#[derive(Debug)]
pub(crate) struct S3Response {
    pub response: reqwest::Response,
    /// Attempts made, counting the first. 1 means it succeeded immediately.
    pub attempts: u32,
}

/// A request that ran out of attempts or hit a non-retryable condition.
///
/// Carries the S3 error `<Code>` and the attempt count so callers can act on
/// the specific failure. `complete_upload` needs both: `NoSuchUpload` after a
/// retry is worth checking against the item, on the first attempt it is not.
#[derive(Debug)]
pub(crate) struct S3Failure {
    /// Boxed so the `Err` side of `send_with_retry` stays small: `IaError`
    /// is over 100 bytes and clippy's `result_large_err` draws the line at
    /// 128 for the whole failure.
    pub error: Box<IaError>,
    /// S3 error `<Code>`, when the body was a parseable S3 error.
    pub code: Option<String>,
    /// Attempts made, counting the first.
    pub attempts: u32,
}

impl From<S3Failure> for IaError {
    fn from(f: S3Failure) -> Self {
        *f.error
    }
}

/// Send an IA-S3 request, retrying per [`should_retry_s3`].
///
/// `send` must build and send the request afresh on each call, because a
/// `reqwest::Request` is consumed by sending. Bodies should be cheap to
/// clone: part PUTs pass a `Bytes`, so each attempt bumps a refcount rather
/// than copying the part. It receives the attempt's [`BodyWatch`]; a request
/// with a body worth watching wraps its stream with it, the others ignore
/// it.
///
/// A body send that stalls below `ctx.min_speed` (see [`BodyWatch`]) is
/// abandoned and re-sent at once while the budget lasts; past it the
/// failure is [`IaError::UploadStalled`].
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
    F: FnMut(&BodyWatch) -> Fut,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest_middleware::Error>>,
{
    let mut attempt: u32 = 0;
    let mut stalls: usize = 0;
    loop {
        attempt += 1;

        // Every attempt is judged on its own clock: a re-send is a new
        // connection and gets the full grace.
        let watch = BodyWatch::new(ctx.min_speed);
        let sent = match watch_send(&watch, send(&watch)).await {
            SendEnd::Done(result) => result,
            SendEnd::Stalled {
                observed,
                window_secs,
            } => {
                stalls += 1;
                tracing::warn!(
                    identifier = ctx.identifier,
                    key = ctx.key,
                    attempt,
                    observed_bytes_per_sec = observed,
                    min_bytes_per_sec = ctx.min_speed,
                    window_secs,
                    "body send stalled, {}",
                    if attempt <= ctx.retries {
                        "re-sending"
                    } else {
                        "giving up"
                    }
                );
                if attempt <= ctx.retries {
                    // No wait: the problem is the peer, not load.
                    report_backoff(ctx, UploadProgressStatus::Retrying);
                    continue;
                }
                return Err(S3Failure {
                    error: Box::new(IaError::UploadStalled {
                        identifier: ctx.identifier.into(),
                        key: ctx.key.into(),
                        observed_bytes_per_sec: observed,
                        min_bytes_per_sec: ctx.min_speed,
                        window_secs,
                        stalls,
                    }),
                    code: None,
                    attempts: attempt,
                });
            }
        };

        let response = match sent {
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
                    let wait = backoff_wait(&ctx.backoff, attempt - 1);
                    tracing::warn!(
                        identifier = ctx.identifier,
                        key = ctx.key,
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        error = %cause,
                        "transport error, retrying {context}"
                    );
                    report_backoff(ctx, UploadProgressStatus::Retrying);
                    tokio::time::sleep(wait).await;
                    continue;
                }
                return Err(S3Failure {
                    error: Box::new(IaError::UploadFailed {
                        identifier: ctx.identifier.into(),
                        key: ctx.key.into(),
                        message: describe_attempts(&format!("{context}: {cause}"), attempt),
                        status: None,
                    }),
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

        // The server's own instruction on how long to wait, if it gave one.
        // Read before the body is consumed; it overrides the schedule.
        let retry_after = retry_after_wait(response.headers());
        // Consume the body once: it is needed both to classify and to report.
        let body = response.text().await.unwrap_or_default();

        // IA's spam rejection is a plain-text 503 with no S3 <Code>. The
        // status fallback would retry it for the whole budget; it is
        // permanent, and the single-file path already treats it as such.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && is_spam_response(&body) {
            return Err(S3Failure {
                error: Box::new(IaError::SpamDetected {
                    identifier: ctx.identifier.into(),
                }),
                code: None,
                attempts: attempt,
            });
        }

        if should_retry_s3(status, &body) && attempt <= ctx.retries {
            let wait = wait_before_retry(retry_after, &ctx.backoff, attempt - 1);
            tracing::debug!(
                identifier = ctx.identifier,
                key = ctx.key,
                attempt,
                %status,
                wait_ms = wait.as_millis() as u64,
                retry_after = retry_after.is_some(),
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
            tokio::time::sleep(wait).await;
            continue;
        }

        let parsed = parse_s3_error(&body);
        let detail = describe_parsed(status, parsed.as_ref(), &body);

        return Err(S3Failure {
            error: Box::new(IaError::UploadFailed {
                identifier: ctx.identifier.into(),
                key: ctx.key.into(),
                message: describe_attempts(&format!("{context} failed: {detail}"), attempt),
                status: Some(status.as_u16()),
            }),
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

#[cfg(test)]
mod stall_tests {
    use super::*;
    use crate::upload::stall_watch::bytes_chunks;
    use crate::upload::stall_watch::test_support::{big_body, shrink_policy, stalling_listener};

    fn ctx<'a>(retries: u32) -> S3RetryCtx<'a> {
        S3RetryCtx {
            identifier: "item",
            key: "part.bin",
            retries,
            backoff: backoff_policy(
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(2),
                retries,
            ),
            min_speed: 10 * 1024,
            bytes_sent: 0,
            total_bytes: 16 * 1024 * 1024,
            progress: None,
        }
    }

    /// A part PUT whose body send stalls is re-sent at once; when the
    /// budget is spent the failure is UploadStalled with one stall per
    /// attempt, and the peer saw one connection per attempt.
    #[tokio::test]
    async fn stalled_part_send_is_retried_then_fails_as_stalled() {
        let _policy = shrink_policy();
        let (addr, connections) = stalling_listener().await;
        let client = crate::client::configure_transport(
            reqwest::Client::builder(),
            std::time::Duration::from_secs(5),
            None,
        )
        .build()
        .unwrap();
        let client = reqwest_middleware::ClientBuilder::new(client).build();
        let body = big_body();
        let url = format!("http://{addr}/item/part.bin?partNumber=1&uploadId=u1");
        let failure = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            send_with_retry(&ctx(1), "upload part 1", |watch| {
                client
                    .put(&url)
                    .header("Content-Length", body.len().to_string())
                    .body(reqwest::Body::wrap_stream(
                        watch.wrap(bytes_chunks(body.clone()), body.len() as u64),
                    ))
                    .send()
            }),
        )
        .await
        .expect("the stalled sends must be judged within a minute")
        .expect_err("a stalled send must fail once the retries are spent");
        assert_eq!(failure.attempts, 2);
        assert!(failure.code.is_none());
        assert!(
            matches!(
                *failure.error,
                IaError::UploadStalled {
                    stalls: 2,
                    window_secs: 2,
                    min_bytes_per_sec: 10240,
                    ..
                }
            ),
            "got {:?}",
            failure.error
        );
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
