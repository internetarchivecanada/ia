//! Retry diagnostics — all HTTP retry and latency data flows through [`RetryStats`].
//!
//! [`RetryStats`] is a shared, lock-free (except for latency storage) counter
//! collection that records per-request and per-retry events. Callers snapshot
//! the state with [`RetryStats::summary`] and compute latency percentiles with
//! [`RetryStats::percentiles`].

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;
use reqwest_retry::policies::ExponentialBackoff;
use reqwest_retry::{
    default_on_request_failure, Jitter, RetryDecision, RetryPolicy, Retryable, RetryableStrategy,
};

/// Bounds of the standard retry schedule: the first retry waits up to
/// `STANDARD_MIN_DELAY`, each later one up to twice the previous, never more
/// than `STANDARD_MAX_DELAY`. Every retry loop in the crate that has no
/// caller-supplied bounds uses these.
pub(crate) const STANDARD_MIN_DELAY: Duration = Duration::from_secs(1);
pub(crate) const STANDARD_MAX_DELAY: Duration = Duration::from_secs(60);

/// The standard retry schedule: truncated exponential backoff with full
/// jitter. Retry `n` (1-based) waits a uniform random time between zero and
/// `min × 2^(n-1)`, never more than `max`. Past `retries` the policy says do
/// not retry; the loops here enforce their own budget, so that case yields
/// no wait at all.
///
/// The library asserts `min <= max`. The bounds are public fields on
/// `UploadOpts`, so a caller can set them the wrong way round; rather than
/// panic, the larger value wins and `min` becomes the cap, with a warning.
/// Build the policy through `UploadOpts::backoff` when the bounds come
/// from upload options.
pub(crate) fn backoff_policy(min: Duration, max: Duration, retries: u32) -> ExponentialBackoff {
    let max = if max < min {
        tracing::warn!(
            min_ms = min.as_millis() as u64,
            max_ms = max.as_millis() as u64,
            "retry_max_delay is below retry_min_delay; using retry_min_delay as the cap"
        );
        min
    } else {
        max
    };
    ExponentialBackoff::builder()
        .retry_bounds(min, max)
        .jitter(Jitter::Full)
        .build_with_max_retries(retries)
}

/// How long to wait before the next attempt, given how many retries have
/// already been made. Zero when the policy's own budget is spent.
///
/// The policy returns an absolute instant; the wait is that instant minus
/// now. Two `SystemTime::now()` calls are involved, and the wall clock can
/// step between them, so the result is clipped to the policy's cap.
pub(crate) fn backoff_wait(policy: &ExponentialBackoff, n_past_retries: u32) -> Duration {
    match policy.should_retry(std::time::SystemTime::now(), n_past_retries) {
        RetryDecision::Retry { execute_after } => execute_after
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default()
            .min(policy.max_retry_interval),
        RetryDecision::DoNotRetry => Duration::ZERO,
    }
}

/// The server's own instruction on how long to wait, if the failed response
/// carried a `Retry-After` header (seconds, or an HTTP date). A pure read:
/// whether the wait is taken is the caller's decision, made after the
/// response is classified.
pub(crate) fn retry_after_wait(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    crate::retry::extract_retry_after(headers).map(Duration::from_secs)
}

/// The wait before a retry: the server's `Retry-After` when it gave one,
/// honored as given (`0` means re-send at once; a value above the
/// schedule's cap is still waited out in full, with a warning so it shows
/// up in `--log`), otherwise the schedule's draw for this retry.
pub(crate) fn wait_before_retry(
    retry_after: Option<Duration>,
    policy: &ExponentialBackoff,
    n_past_retries: u32,
) -> Duration {
    match retry_after {
        Some(wait) => {
            if wait > policy.max_retry_interval {
                tracing::warn!(
                    retry_after_s = wait.as_secs(),
                    cap_s = policy.max_retry_interval.as_secs(),
                    "server asked for a Retry-After above the backoff cap; honoring it as given"
                );
            }
            wait
        }
        None => backoff_wait(policy, n_past_retries),
    }
}

/// Lock-free counters and latency samples for HTTP retry diagnostics.
///
/// Create one instance per [`crate::client::IaClient`] and pass it (via
/// `Arc`) to middleware so every request and retry is recorded.
#[derive(Debug)]
pub struct RetryStats {
    /// Total HTTP requests attempted (including retried ones).
    requests_total: AtomicU64,
    /// Total retry attempts across all requests.
    retries_total: AtomicU64,
    /// Number of 429 responses received.
    status_429_count: AtomicU64,
    /// Number of 5xx responses received.
    status_5xx_count: AtomicU64,
    /// Accumulated `Retry-After` wait time in milliseconds.
    total_retry_wait_ms: AtomicU64,
    /// True while at least one request is currently rate-limited.
    /// Used in verbosity-0 mode to deduplicate log emissions.
    currently_rate_limited: AtomicBool,
    /// Diagnostic verbosity level set once at startup.
    /// 0 = warn on first rate-limit only; 1+ = info per event.
    verbosity: AtomicU8,
    /// Per-request round-trip latencies in milliseconds, unsorted.
    /// Sorted on read to compute percentiles.
    latencies_ms: Mutex<Vec<u64>>,
}

/// Point-in-time snapshot of [`RetryStats`] counters.
#[derive(Debug, Clone)]
pub struct RetrySummary {
    pub requests_total: u64,
    pub retries_total: u64,
    pub status_429_count: u64,
    pub status_5xx_count: u64,
    pub total_retry_wait: Duration,
}

/// Latency percentiles computed from recorded request durations.
#[derive(Debug, Clone)]
pub struct Percentiles {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
}

impl RetryStats {
    /// Create a new, zeroed [`RetryStats`] with the given verbosity level.
    pub fn new(verbosity: u8) -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            retries_total: AtomicU64::new(0),
            status_429_count: AtomicU64::new(0),
            status_5xx_count: AtomicU64::new(0),
            total_retry_wait_ms: AtomicU64::new(0),
            currently_rate_limited: AtomicBool::new(false),
            verbosity: AtomicU8::new(verbosity),
            latencies_ms: Mutex::new(Vec::new()),
        }
    }

    /// Record a completed request with its round-trip latency.
    ///
    /// Increments `requests_total` and appends the latency sample for
    /// percentile computation. Does **not** clear `currently_rate_limited` —
    /// in concurrent batches, an unrelated successful request must not reset
    /// the dedup flag while another task is still being rate-limited.
    pub fn record_request(&self, latency: Duration) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);

        let ms = latency.as_millis() as u64;
        if let Ok(mut guard) = self.latencies_ms.lock() {
            guard.push(ms);
        }

        tracing::debug!(latency_ms = ms, "request completed");
    }

    /// Record a 429 rate-limit event (NOT a middleware retry).
    ///
    /// Increments `status_429_count` and accumulates `Retry-After` wait time.
    /// Does NOT increment `retries_total` because 429s are handled by the
    /// application layer, not retried by middleware.
    ///
    /// Logging behaviour:
    /// - verbosity >= 1: emits `tracing::info!` for every event.
    /// - verbosity 0: emits `tracing::warn!` only on the first rate-limit
    ///   (`false -> true` transition of `currently_rate_limited`).
    pub fn record_rate_limit(&self, status: u16, retry_after: Option<u64>, url_path: &str) {
        self.status_429_count.fetch_add(1, Ordering::Relaxed);

        if let Some(secs) = retry_after {
            self.total_retry_wait_ms
                .fetch_add(secs.saturating_mul(1_000), Ordering::Relaxed);
        }

        let verbosity = self.verbosity.load(Ordering::Relaxed);
        if verbosity >= 1 {
            tracing::info!(
                status,
                retry_after_secs = retry_after,
                url_path,
                "rate-limited by server"
            );
        } else {
            // Deduplicate: only log on the false→true transition.
            let was_limited = self.currently_rate_limited.swap(true, Ordering::Relaxed);
            if !was_limited {
                tracing::warn!(
                    status,
                    retry_after_secs = retry_after,
                    url_path,
                    "rate-limited by server"
                );
            }
        }
    }

    /// Record a 5xx server error retry event.
    ///
    /// Increments `retries_total` and `status_5xx_count`, and emits
    /// `tracing::info!` regardless of verbosity level.
    pub fn record_server_error(&self, status: u16, url_path: &str) {
        self.retries_total.fetch_add(1, Ordering::Relaxed);
        self.status_5xx_count.fetch_add(1, Ordering::Relaxed);

        tracing::info!(status, url_path, "server error, retrying");
    }

    /// Returns `true` if any retry or rate-limit event has been recorded.
    pub fn had_retries(&self) -> bool {
        self.retries_total.load(Ordering::Relaxed) > 0
            || self.status_429_count.load(Ordering::Relaxed) > 0
    }

    /// Return a point-in-time snapshot of all counters.
    pub fn summary(&self) -> RetrySummary {
        RetrySummary {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            retries_total: self.retries_total.load(Ordering::Relaxed),
            status_429_count: self.status_429_count.load(Ordering::Relaxed),
            status_5xx_count: self.status_5xx_count.load(Ordering::Relaxed),
            total_retry_wait: Duration::from_millis(
                self.total_retry_wait_ms.load(Ordering::Relaxed),
            ),
        }
    }

    /// Compute latency percentiles from all recorded request durations.
    ///
    /// Returns `None` when no requests have been recorded. Sorts the
    /// latency samples in place (inside the mutex) on each call.
    pub fn percentiles(&self) -> Option<Percentiles> {
        let mut guard = self.latencies_ms.lock().ok()?;
        if guard.is_empty() {
            return None;
        }
        guard.sort_unstable();
        let n = guard.len();

        // Nearest-rank method: rank = ceil(pct/100 * n), 0-based idx = rank - 1.
        // Clamped to [0, n-1] to guard against floating-point rounding.
        let idx = |pct: f64| -> usize {
            let rank = (pct / 100.0 * n as f64).ceil() as usize;
            rank.saturating_sub(1).min(n - 1)
        };

        Some(Percentiles {
            p50_ms: guard[idx(50.0)],
            p95_ms: guard[idx(95.0)],
            p99_ms: guard[idx(99.0)],
        })
    }
}

/// Whether this `reqwest::Error` represents a transient body-read / decode
/// failure that can be retried once the response has been returned.
///
/// The retry middleware (`reqwest-retry`) only sees the initial response
/// status and send-level transport failures. Once it hands the response off
/// to caller code, any failure reading the body (`bytes_stream`, `.json`,
/// `.text`, `.bytes`) bypasses the middleware entirely — `is_body()` and
/// `is_decode()` cover those cases. Request-level timeouts that fire after
/// a successful response header are also worth retrying here.
///
/// Returns `false` for definite connection/transport errors that the
/// middleware should have already handled.
pub fn is_retryable_body_error(err: &reqwest::Error) -> bool {
    err.is_body() || err.is_decode() || err.is_timeout()
}

/// Parse the `Retry-After` header as a number of seconds.
///
/// Accepts both forms the header allows: a non-negative integer number of
/// seconds, or an HTTP date, which becomes the whole seconds from now until
/// that date (zero when the date has passed). Returns `None` if the header
/// is missing or is neither form.
pub fn extract_retry_after(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(secs);
    }
    // The other form the header allows: an HTTP date. A date already past
    // means "now", that is, zero seconds.
    let when = httpdate::parse_http_date(value).ok()?;
    Some(
        when.duration_since(std::time::SystemTime::now())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// Retry strategy that records diagnostics before delegating retry decisions.
///
/// Implements [`RetryableStrategy`] from `reqwest-retry`. Classifies responses
/// and records retry events to the shared [`RetryStats`]. The actual
/// retry/don't-retry decision follows the same logic as the default strategy.
pub struct LoggingRetryStrategy {
    stats: Arc<RetryStats>,
}

impl LoggingRetryStrategy {
    /// Create a new strategy backed by the given stats.
    pub fn new(stats: Arc<RetryStats>) -> Self {
        Self { stats }
    }
}

impl RetryableStrategy for LoggingRetryStrategy {
    fn handle(
        &self,
        res: &Result<reqwest::Response, reqwest_middleware::Error>,
    ) -> Option<Retryable> {
        match res {
            Ok(response) => {
                let status = response.status().as_u16();
                let retry_after = extract_retry_after(response.headers());
                let url_path = response.url().path();

                if status == 429 {
                    self.stats.record_rate_limit(status, retry_after, url_path);
                    None // Don't retry — caller handles via IaError::RateLimited
                } else if status >= 500 {
                    self.stats.record_server_error(status, url_path);
                    Some(Retryable::Transient)
                } else if status >= 400 {
                    Some(Retryable::Fatal)
                } else {
                    None // success
                }
            }
            Err(error) => default_on_request_failure(error),
        }
    }
}

/// Retry strategy for non-idempotent requests: connect failures only.
///
/// Metadata writes and task submissions must never be replayed on a 5xx,
/// because the server can apply the change and then fail the response. A
/// connection that was never established is different: the request did not
/// reach the server, so there is nothing to apply twice and retrying is
/// safe.
///
/// Anything that got as far as a response, or failed after the connection
/// was up, returns `None` and surfaces to the caller.
pub struct ConnectOnlyRetryStrategy {
    stats: Arc<RetryStats>,
}

impl ConnectOnlyRetryStrategy {
    /// Create a new strategy backed by the given stats.
    pub fn new(stats: Arc<RetryStats>) -> Self {
        Self { stats }
    }
}

impl RetryableStrategy for ConnectOnlyRetryStrategy {
    fn handle(
        &self,
        res: &Result<reqwest::Response, reqwest_middleware::Error>,
    ) -> Option<Retryable> {
        match res {
            // A response arrived. Whatever its status, the server saw the
            // request, so replaying it risks applying the write twice.
            Ok(response) => {
                let status = response.status().as_u16();
                if status >= 500 {
                    self.stats
                        .record_server_error(status, response.url().path());
                }
                None
            }
            Err(reqwest_middleware::Error::Reqwest(e)) if e.is_connect() => {
                Some(Retryable::Transient)
            }
            Err(_) => None,
        }
    }
}

/// Middleware that measures wall-clock time per logical request (including retries).
///
/// Sits outside the retry middleware in the stack, so it captures the total
/// time from request start to final response (including any retry waits).
pub struct TimingMiddleware {
    stats: Arc<RetryStats>,
}

impl TimingMiddleware {
    /// Create a new timing middleware backed by the given stats.
    pub fn new(stats: Arc<RetryStats>) -> Self {
        Self { stats }
    }
}

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for TimingMiddleware {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let start = Instant::now();
        let result = next.run(req, extensions).await;
        self.stats.record_request(start.elapsed());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── ConnectOnlyRetryStrategy ─────────────────────────────────────────────

    /// A connection that was never established is safe to replay: the request
    /// did not reach the server, so there is nothing to apply twice.
    #[tokio::test]
    async fn connect_only_retries_a_connect_failure() {
        // Port 1 on loopback: nothing listens, so this is a genuine connect
        // error rather than a synthesised one.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connecting to a dead port should fail");
        assert!(err.is_connect(), "expected a connect error, got: {err:?}");

        let strategy = ConnectOnlyRetryStrategy::new(Arc::new(RetryStats::new(0)));
        let decision = strategy.handle(&Err(reqwest_middleware::Error::Reqwest(err)));

        assert!(
            matches!(decision, Some(Retryable::Transient)),
            "connect failures must be retried"
        );
    }

    /// A 5xx means the server saw the request. Replaying it could apply a
    /// metadata patch or queue a task a second time.
    #[tokio::test]
    async fn connect_only_does_not_retry_a_server_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let response = reqwest::Client::new()
            .post(server.uri())
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status().as_u16(), 503);

        let strategy = ConnectOnlyRetryStrategy::new(Arc::new(RetryStats::new(0)));
        let decision = strategy.handle(&Ok(response));

        assert!(
            decision.is_none(),
            "a 5xx must not be retried on a write path"
        );
    }

    // ── Task 1: RetryStats core ──────────────────────────────────────────────

    #[test]
    fn new_stats_are_zero() {
        let stats = RetryStats::new(0);
        let s = stats.summary();
        assert_eq!(s.requests_total, 0);
        assert_eq!(s.retries_total, 0);
        assert_eq!(s.status_429_count, 0);
        assert_eq!(s.status_5xx_count, 0);
        assert_eq!(s.total_retry_wait.as_millis(), 0);
        assert!(!stats.had_retries());
    }

    #[test]
    fn record_request_increments_total_and_tracks_latency() {
        let stats = RetryStats::new(0);
        stats.record_request(Duration::from_millis(50));
        stats.record_request(Duration::from_millis(150));
        let s = stats.summary();
        assert_eq!(s.requests_total, 2);
    }

    #[test]
    fn record_rate_limit_429_increments_counters() {
        let stats = RetryStats::new(0);
        stats.record_rate_limit(429, Some(30), "/metadata/test");
        let s = stats.summary();
        assert_eq!(s.retries_total, 0, "429s are not middleware retries");
        assert_eq!(s.status_429_count, 1);
        assert_eq!(s.status_5xx_count, 0);
        assert_eq!(s.total_retry_wait, Duration::from_secs(30));
        assert!(stats.had_retries(), "had_retries includes rate limits");
    }

    #[test]
    fn record_server_error_increments_counters() {
        let stats = RetryStats::new(0);
        stats.record_server_error(503, "/metadata/test");
        let s = stats.summary();
        assert_eq!(s.retries_total, 1);
        assert_eq!(s.status_429_count, 0);
        assert_eq!(s.status_5xx_count, 1);
    }

    #[test]
    fn record_rate_limit_429_without_retry_after() {
        let stats = RetryStats::new(0);
        stats.record_rate_limit(429, None, "/metadata/test");
        let s = stats.summary();
        assert_eq!(s.retries_total, 0);
        assert_eq!(s.status_429_count, 1);
        assert_eq!(s.total_retry_wait, Duration::ZERO);
    }

    #[test]
    fn multiple_events_accumulate() {
        let stats = RetryStats::new(0);
        stats.record_rate_limit(429, Some(10), "/metadata/a");
        stats.record_rate_limit(429, Some(20), "/metadata/b");
        stats.record_server_error(500, "/metadata/c");
        let s = stats.summary();
        assert_eq!(s.retries_total, 1, "only 5xx counts as retry");
        assert_eq!(s.status_429_count, 2);
        assert_eq!(s.status_5xx_count, 1);
        assert_eq!(s.total_retry_wait, Duration::from_secs(30));
    }

    // ── Task 2: Percentile edge cases ────────────────────────────────────────

    #[test]
    fn percentiles_none_when_no_requests() {
        let stats = RetryStats::new(0);
        assert!(stats.percentiles().is_none());
    }

    #[test]
    fn percentiles_single_request() {
        let stats = RetryStats::new(0);
        stats.record_request(Duration::from_millis(42));
        let p = stats.percentiles().expect("should have percentiles");
        assert_eq!(p.p50_ms, 42);
        assert_eq!(p.p95_ms, 42);
        assert_eq!(p.p99_ms, 42);
    }

    #[test]
    fn percentiles_multiple_requests() {
        let stats = RetryStats::new(0);
        for ms in 1u64..=100 {
            stats.record_request(Duration::from_millis(ms));
        }
        let p = stats.percentiles().expect("should have percentiles");
        assert_eq!(p.p50_ms, 50);
        assert_eq!(p.p95_ms, 95);
        assert_eq!(p.p99_ms, 99);
    }

    #[test]
    fn percentiles_unsorted_input() {
        let stats = RetryStats::new(0);
        for ms in [500u64, 10, 200, 1] {
            stats.record_request(Duration::from_millis(ms));
        }
        // sorted: [1, 10, 200, 500]  n=4
        // Nearest-rank: rank = ceil(pct/100 * n), 0-based idx = rank - 1
        // p50 → ceil(0.50 * 4) = 2 → idx 1 → 10
        // p95 → ceil(0.95 * 4) = ceil(3.8) = 4 → idx 3 → 500
        // p99 → ceil(0.99 * 4) = ceil(3.96) = 4 → idx 3 → 500
        let p = stats.percentiles().expect("should have percentiles");
        assert_eq!(p.p50_ms, 10, "p50 should be 10");
        assert_eq!(p.p95_ms, 500, "p95 should be 500");
        assert_eq!(p.p99_ms, 500, "p99 should be 500");
    }

    // ── Task 3: extract_retry_after ──────────────────────────────────────────

    #[test]
    fn extract_retry_after_numeric() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "120".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_retry_after(&headers), None);
    }

    /// The header's other form is an HTTP date. One already in the past
    /// means "now": zero seconds, not an unparseable value.
    #[test]
    fn extract_retry_after_past_http_date_is_zero() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    #[test]
    fn extract_retry_after_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "0".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    // ── Task 4: LoggingRetryStrategy ─────────────────────────────────────────

    fn make_strategy() -> (Arc<RetryStats>, LoggingRetryStrategy) {
        let stats = Arc::new(RetryStats::new(0));
        let strategy = LoggingRetryStrategy::new(stats.clone());
        (stats, strategy)
    }

    #[test]
    fn strategy_200_returns_none() {
        use reqwest_retry::RetryableStrategy;
        let (_stats, strategy) = make_strategy();
        let response = http::Response::builder().status(200).body("").unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(strategy.handle(&result).is_none());
    }

    #[test]
    fn strategy_429_returns_none() {
        use reqwest_retry::RetryableStrategy;
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(429)
            .header("retry-after", "60")
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(
            strategy.handle(&result).is_none(),
            "429 should not be retried by middleware"
        );
        assert_eq!(stats.summary().status_429_count, 1);
        assert_eq!(stats.summary().total_retry_wait, Duration::from_secs(60));
    }

    #[test]
    fn strategy_503_returns_transient() {
        use reqwest_retry::{Retryable, RetryableStrategy};
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder().status(503).body("").unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(matches!(
            strategy.handle(&result),
            Some(Retryable::Transient)
        ));
        assert_eq!(stats.summary().status_5xx_count, 1);
    }

    #[test]
    fn strategy_404_returns_fatal() {
        use reqwest_retry::{Retryable, RetryableStrategy};
        let (_stats, strategy) = make_strategy();
        let response = http::Response::builder().status(404).body("").unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(matches!(strategy.handle(&result), Some(Retryable::Fatal)));
    }

    #[test]
    fn strategy_429_no_retry_after_header() {
        use reqwest_retry::RetryableStrategy;
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder().status(429).body("").unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(
            strategy.handle(&result).is_none(),
            "429 without retry-after should still not retry"
        );
        assert_eq!(stats.summary().total_retry_wait, Duration::ZERO);
    }
    // -- Retry-After forms --

    fn headers_with_retry_after(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("retry-after", value.parse().unwrap());
        h
    }

    #[test]
    fn retry_after_seconds() {
        assert_eq!(
            extract_retry_after(&headers_with_retry_after("120")),
            Some(120)
        );
        assert_eq!(
            extract_retry_after(&headers_with_retry_after(" 7 ")),
            Some(7)
        );
    }

    #[test]
    fn retry_after_http_date_in_the_future() {
        let when = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        let value = httpdate::fmt_http_date(when);
        let secs = extract_retry_after(&headers_with_retry_after(&value)).unwrap();
        assert!((29..=31).contains(&secs), "{secs}");
    }

    #[test]
    fn retry_after_http_date_in_the_past_is_zero() {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(300);
        let value = httpdate::fmt_http_date(when);
        assert_eq!(
            extract_retry_after(&headers_with_retry_after(&value)),
            Some(0)
        );
    }

    #[test]
    fn retry_after_garbage_is_none() {
        assert_eq!(extract_retry_after(&headers_with_retry_after("soon")), None);
        assert_eq!(extract_retry_after(&HeaderMap::new()), None);
    }

    // ── The standard schedule ──────────────────────────────────────────────

    const MIN: Duration = STANDARD_MIN_DELAY;
    const MAX: Duration = STANDARD_MAX_DELAY;

    /// The upper bound the schedule allows for retry number `n` (1-based):
    /// min × 2^(n-1), no more than max.
    fn upper_bound(n: u32) -> Duration {
        std::cmp::min(MAX, MIN * 2u32.saturating_pow(n - 1))
    }

    #[test]
    fn waits_never_exceed_the_schedule() {
        let policy = backoff_policy(MIN, MAX, 100);
        for retry in 1..=20u32 {
            for _ in 0..50 {
                let wait = backoff_wait(&policy, retry - 1);
                assert!(
                    wait <= upper_bound(retry),
                    "retry {retry}: {wait:?} > {:?}",
                    upper_bound(retry)
                );
            }
        }
    }

    #[test]
    fn waits_are_jittered() {
        let policy = backoff_policy(MIN, MAX, 100);
        let draws: Vec<Duration> = (0..1000).map(|_| backoff_wait(&policy, 6)).collect();
        let first = draws[0];
        assert!(
            draws.iter().any(|d| *d != first),
            "1000 draws were all {first:?}"
        );
        assert!(draws.iter().all(|d| *d <= MAX));
    }

    #[test]
    fn past_the_budget_the_wait_is_zero() {
        // The loop's own budget check ends the loop; the policy just says
        // "do not retry", which must not turn into a sleep.
        let policy = backoff_policy(MIN, MAX, 3);
        assert_eq!(backoff_wait(&policy, 3), Duration::ZERO);
        assert_eq!(backoff_wait(&policy, 50), Duration::ZERO);
    }

    /// The library asserts min <= max. Those bounds are public fields on
    /// `UploadOpts`, so a caller can set them the wrong way round; that
    /// must not panic. The larger value wins: min becomes the cap.
    #[test]
    fn min_above_max_does_not_panic_and_caps_at_min() {
        let policy = backoff_policy(Duration::from_secs(60), Duration::from_secs(1), 10);
        assert_eq!(policy.min_retry_interval, Duration::from_secs(60));
        assert_eq!(policy.max_retry_interval, Duration::from_secs(60));
        for n in 0..10 {
            assert!(backoff_wait(&policy, n) <= Duration::from_secs(60));
        }
    }

    #[test]
    fn millisecond_bounds_keep_tests_fast() {
        let policy = backoff_policy(Duration::from_millis(1), Duration::from_millis(2), 10);
        for n in 0..10 {
            assert!(backoff_wait(&policy, n) <= Duration::from_millis(2));
        }
    }
}
