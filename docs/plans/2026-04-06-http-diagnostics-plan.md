# HTTP Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add global HTTP diagnostic infrastructure (retry stats, timing, verbosity) so users can see rate-limiting and latency data, starting with metadata export.

**Architecture:** New `retry.rs` module in ia-core owns all counters and tracing events. `LoggingRetryStrategy` replaces the default retry classifier. `TimingMiddleware` wraps the retry layer to capture wall-clock latency. CLI replaces `--debug` with `--verbose` (`-v`/`-vv`/`-vvv`) and prints a retry summary at end of metadata export.

**Tech Stack:** `reqwest-middleware` (Middleware trait), `reqwest-retry` (RetryableStrategy trait), `async-trait`, `tracing`, atomics for lock-free counters.

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `ia-core/src/retry.rs` | `RetryStats`, `RetrySummary`, `Percentiles`, `LoggingRetryStrategy`, `TimingMiddleware`, `extract_retry_after` |
| Modify | `ia-core/src/lib.rs` | Add `pub mod retry;` and re-export `RetryStats` |
| Modify | `ia-core/src/client.rs` | Wire middleware, add `retry_stats()` accessor, new `from_config_with_verbosity()` |
| Modify | `ia-core/Cargo.toml` | Add `async-trait = "0.1"` dependency |
| Modify | `ia-cli/src/main.rs` | Replace `--debug` with `--verbose`, update tracing subscriber, pass verbosity to client |
| Modify | `ia-cli/src/output.rs` | Add `print_retry_summary()` function |
| Modify | `ia-cli/src/commands/metadata.rs` | Call `print_retry_summary()` at end of export |
| Modify | `ia-cli/tests/cli.rs` | Update `--debug` references to `--verbose` |
| Create | `ia-core/tests/retry.rs` | Integration tests: wiremock 429/503/200 through full client |

---

### Task 1: RetryStats Core — Counters and Snapshot Types

**Files:**
- Create: `ia-core/src/retry.rs`
- Modify: `ia-core/src/lib.rs`

- [ ] **Step 1: Write failing tests for RetryStats counters**

Add to `ia-core/src/retry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
    fn record_retry_429_increments_counters() {
        let stats = RetryStats::new(0);
        stats.record_retry(429, Some(30), "/metadata/test");
        let s = stats.summary();
        assert_eq!(s.retries_total, 1);
        assert_eq!(s.status_429_count, 1);
        assert_eq!(s.status_5xx_count, 0);
        assert_eq!(s.total_retry_wait, Duration::from_secs(30));
        assert!(stats.had_retries());
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
    fn record_retry_429_without_retry_after() {
        let stats = RetryStats::new(0);
        stats.record_retry(429, None, "/metadata/test");
        let s = stats.summary();
        assert_eq!(s.retries_total, 1);
        assert_eq!(s.status_429_count, 1);
        assert_eq!(s.total_retry_wait, Duration::ZERO);
    }

    #[test]
    fn multiple_retries_accumulate() {
        let stats = RetryStats::new(0);
        stats.record_retry(429, Some(10), "/metadata/a");
        stats.record_retry(429, Some(20), "/metadata/b");
        stats.record_server_error(500, "/metadata/c");
        let s = stats.summary();
        assert_eq!(s.retries_total, 3);
        assert_eq!(s.status_429_count, 2);
        assert_eq!(s.status_5xx_count, 1);
        assert_eq!(s.total_retry_wait, Duration::from_secs(30));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core retry::tests -- --nocapture`
Expected: compile error — `retry` module doesn't exist yet.

- [ ] **Step 3: Write RetryStats implementation**

Create `ia-core/src/retry.rs`:

```rust
//! HTTP request diagnostics: retry statistics, timing middleware, and logging strategy.
//!
//! All diagnostic data flows through [`RetryStats`] methods, which own both the
//! counters and the tracing event emission. This ensures consistent output format
//! regardless of whether data comes from middleware (most commands) or manual
//! retry code (upload).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Shared, lock-free (mostly) HTTP request diagnostics.
///
/// Tracks retry counts, status code breakdown, latencies, and rate-limit state.
/// Accessible via `client.retry_stats()`.
#[derive(Debug)]
pub struct RetryStats {
    requests_total: AtomicU64,
    retries_total: AtomicU64,
    status_429_count: AtomicU64,
    status_5xx_count: AtomicU64,
    total_retry_wait_ms: AtomicU64,
    currently_rate_limited: AtomicBool,
    verbosity: AtomicU8,
    latencies_ms: Mutex<Vec<u64>>,
}

/// Snapshot of retry counters at a point in time.
#[derive(Debug, Clone)]
pub struct RetrySummary {
    pub requests_total: u64,
    pub retries_total: u64,
    pub status_429_count: u64,
    pub status_5xx_count: u64,
    pub total_retry_wait: Duration,
}

/// Latency percentiles (p50, p95, p99).
#[derive(Debug, Clone)]
pub struct Percentiles {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
}

impl RetryStats {
    /// Create a new stats tracker.
    ///
    /// `verbosity` controls tracing detail: 0 = dedup rate-limit warnings,
    /// 1+ = individual retry events.
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

    /// Record a completed logical request (after all retries).
    ///
    /// Called by `TimingMiddleware` for middleware-path requests, or directly
    /// by upload code for `raw_http()` requests.
    pub fn record_request(&self, latency: Duration) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.currently_rate_limited.store(false, Ordering::Relaxed);
        if let Ok(mut lat) = self.latencies_ms.lock() {
            lat.push(latency.as_millis() as u64);
        }
        tracing::debug!(latency_ms = latency.as_millis() as u64, "request completed");
    }

    /// Record a retry due to 429 rate limiting.
    ///
    /// Called by `LoggingRetryStrategy` on each 429 response, or directly
    /// by upload retry code.
    pub fn record_retry(&self, status: u16, retry_after: Option<u64>, url_path: &str) {
        self.retries_total.fetch_add(1, Ordering::Relaxed);
        self.status_429_count.fetch_add(1, Ordering::Relaxed);
        if let Some(secs) = retry_after {
            self.total_retry_wait_ms
                .fetch_add(secs * 1000, Ordering::Relaxed);
        }

        let verbosity = self.verbosity.load(Ordering::Relaxed);

        if verbosity >= 1 {
            // -v: log each retry individually
            tracing::info!(
                status,
                retry_after = retry_after.unwrap_or(0),
                path = url_path,
                "rate limited"
            );
        } else {
            // Default: dedup — only log on transition to rate-limited state
            let was_limited = self.currently_rate_limited.swap(true, Ordering::Relaxed);
            if !was_limited {
                tracing::warn!(
                    retry_after = retry_after.unwrap_or(0),
                    "rate limited by server"
                );
            }
        }
    }

    /// Record a retry due to server error (5xx).
    pub fn record_server_error(&self, status: u16, url_path: &str) {
        self.retries_total.fetch_add(1, Ordering::Relaxed);
        self.status_5xx_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(status, path = url_path, "server error, will retry");
    }

    /// Whether any retries occurred.
    pub fn had_retries(&self) -> bool {
        self.retries_total.load(Ordering::Relaxed) > 0
    }

    /// Snapshot current counters.
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

    /// Compute latency percentiles. Returns `None` if no requests recorded.
    pub fn percentiles(&self) -> Option<Percentiles> {
        let mut latencies = self.latencies_ms.lock().ok()?;
        if latencies.is_empty() {
            return None;
        }
        latencies.sort_unstable();
        let len = latencies.len();
        Some(Percentiles {
            p50_ms: latencies[len * 50 / 100],
            p95_ms: latencies[len * 95 / 100],
            p99_ms: latencies[(len * 99 / 100).min(len - 1)],
        })
    }
}
```

- [ ] **Step 4: Register the module in lib.rs**

Add `pub mod retry;` to `ia-core/src/lib.rs` (after `pub mod rate_limit;`).

Add to the re-exports: `pub use retry::RetryStats;`

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-core retry::tests`
Expected: all 7 tests pass.

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/retry.rs ia-core/src/lib.rs
git commit -m "feat: add RetryStats counters and snapshot types

Core diagnostic infrastructure for tracking HTTP retry behavior.
Lock-free atomic counters for request/retry totals, 429/5xx
breakdown, and server-requested wait time. Mutex-guarded latency
vector for percentile computation.

Part of #309"
```

---

### Task 2: Percentile Edge Cases

**Files:**
- Modify: `ia-core/src/retry.rs`

- [ ] **Step 1: Write failing tests for percentile edge cases**

Add to the `tests` module in `ia-core/src/retry.rs`:

```rust
    #[test]
    fn percentiles_none_when_no_requests() {
        let stats = RetryStats::new(0);
        assert!(stats.percentiles().is_none());
    }

    #[test]
    fn percentiles_single_request() {
        let stats = RetryStats::new(0);
        stats.record_request(Duration::from_millis(42));
        let p = stats.percentiles().unwrap();
        assert_eq!(p.p50_ms, 42);
        assert_eq!(p.p95_ms, 42);
        assert_eq!(p.p99_ms, 42);
    }

    #[test]
    fn percentiles_multiple_requests() {
        let stats = RetryStats::new(0);
        // 100 requests: 1ms, 2ms, ..., 100ms
        for i in 1..=100 {
            stats.record_request(Duration::from_millis(i));
        }
        let p = stats.percentiles().unwrap();
        assert_eq!(p.p50_ms, 50);
        assert_eq!(p.p95_ms, 95);
        assert_eq!(p.p99_ms, 99);
    }

    #[test]
    fn percentiles_unsorted_input() {
        let stats = RetryStats::new(0);
        // Insert out of order — percentiles() must sort internally
        stats.record_request(Duration::from_millis(500));
        stats.record_request(Duration::from_millis(10));
        stats.record_request(Duration::from_millis(200));
        stats.record_request(Duration::from_millis(1));
        let p = stats.percentiles().unwrap();
        // Sorted: [1, 10, 200, 500], len=4
        // p50 = index 2 = 200, p95 = index 3 = 500, p99 = index 3 = 500
        assert_eq!(p.p50_ms, 200);
        assert_eq!(p.p95_ms, 500);
        assert_eq!(p.p99_ms, 500);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ia-core retry::tests`
Expected: all 11 tests pass (7 from Task 1 + 4 new).

- [ ] **Step 3: Commit**

```bash
git add ia-core/src/retry.rs
git commit -m "test: add percentile edge case tests for RetryStats

Covers empty stats, single request, sequential input, and
unsorted input to verify sort-before-index behavior.

Part of #309"
```

---

### Task 3: extract_retry_after Helper

**Files:**
- Modify: `ia-core/src/retry.rs`

- [ ] **Step 1: Write failing tests for Retry-After extraction**

Add to the `tests` module in `ia-core/src/retry.rs`:

```rust
    use reqwest::header::HeaderMap;

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

    #[test]
    fn extract_retry_after_non_numeric() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "Fri, 31 Dec 2026 23:59:59 GMT".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_retry_after_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "0".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(0));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core retry::tests::extract`
Expected: compile error — `extract_retry_after` not defined yet.

- [ ] **Step 3: Implement extract_retry_after**

Add to `ia-core/src/retry.rs` (above the `impl RetryStats` block):

```rust
use reqwest::header::HeaderMap;

/// Parse the `Retry-After` header as a number of seconds.
///
/// Returns `None` if the header is missing or not a valid integer
/// (e.g., HTTP-date format is not supported).
pub fn extract_retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core retry::tests`
Expected: all 15 tests pass.

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/retry.rs
git commit -m "feat: add extract_retry_after header parser

Parses Retry-After as integer seconds. Returns None for missing
headers or HTTP-date format (not needed for archive.org which
always sends integer values).

Part of #309"
```

---

### Task 4: LoggingRetryStrategy

**Files:**
- Modify: `ia-core/src/retry.rs`

- [ ] **Step 1: Write failing tests for LoggingRetryStrategy**

Add to the `tests` module in `ia-core/src/retry.rs`:

```rust
    use std::sync::Arc;

    fn make_strategy() -> (Arc<RetryStats>, LoggingRetryStrategy) {
        let stats = Arc::new(RetryStats::new(0));
        let strategy = LoggingRetryStrategy::new(stats.clone());
        (stats, strategy)
    }

    #[test]
    fn strategy_200_returns_none() {
        use reqwest_retry::RetryableStrategy;
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(200)
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert!(strategy.handle(&result).is_none());
        assert_eq!(stats.summary().retries_total, 0);
    }

    #[test]
    fn strategy_429_returns_transient() {
        use reqwest_retry::{Retryable, RetryableStrategy};
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(429)
            .header("retry-after", "60")
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert_eq!(strategy.handle(&result), Some(Retryable::Transient));
        let s = stats.summary();
        assert_eq!(s.status_429_count, 1);
        assert_eq!(s.total_retry_wait, Duration::from_secs(60));
    }

    #[test]
    fn strategy_503_returns_transient() {
        use reqwest_retry::{Retryable, RetryableStrategy};
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(503)
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert_eq!(strategy.handle(&result), Some(Retryable::Transient));
        assert_eq!(stats.summary().status_5xx_count, 1);
    }

    #[test]
    fn strategy_404_returns_fatal() {
        use reqwest_retry::{Retryable, RetryableStrategy};
        let (_stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(404)
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        assert_eq!(strategy.handle(&result), Some(Retryable::Fatal));
    }

    #[test]
    fn strategy_429_extracts_retry_after() {
        use reqwest_retry::RetryableStrategy;
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(429)
            .header("retry-after", "45")
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        strategy.handle(&result);
        assert_eq!(stats.summary().total_retry_wait, Duration::from_secs(45));
    }

    #[test]
    fn strategy_429_no_retry_after_header() {
        use reqwest_retry::RetryableStrategy;
        let (stats, strategy) = make_strategy();
        let response = http::Response::builder()
            .status(429)
            .body("")
            .unwrap();
        let reqwest_resp = reqwest::Response::from(response);
        let result: Result<reqwest::Response, reqwest_middleware::Error> = Ok(reqwest_resp);
        strategy.handle(&result);
        assert_eq!(stats.summary().total_retry_wait, Duration::ZERO);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core retry::tests::strategy`
Expected: compile error — `LoggingRetryStrategy` not defined.

- [ ] **Step 3: Implement LoggingRetryStrategy**

Add to `ia-core/src/retry.rs`:

```rust
use std::sync::Arc;
use reqwest_retry::{Retryable, RetryableStrategy, default_on_request_failure};

/// Retry strategy that records diagnostics before delegating retry decisions.
///
/// Implements `RetryableStrategy` from `reqwest-retry`. Classifies responses
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
    fn handle(&self, res: &Result<reqwest::Response, reqwest_middleware::Error>) -> Option<Retryable> {
        match res {
            Ok(response) => {
                let status = response.status().as_u16();
                let retry_after = extract_retry_after(response.headers());
                let url_path = response.url().path();

                if status == 429 {
                    self.stats.record_retry(status, retry_after, url_path);
                    Some(Retryable::Transient)
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core retry::tests`
Expected: all 21 tests pass.

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/retry.rs
git commit -m "feat: add LoggingRetryStrategy for diagnostic retry classification

Implements reqwest-retry's RetryableStrategy trait. Records 429 and
5xx events to RetryStats before returning the retry decision.
Extracts Retry-After header from 429 responses.

Part of #309"
```

---

### Task 5: TimingMiddleware

**Files:**
- Modify: `ia-core/src/retry.rs`
- Modify: `ia-core/Cargo.toml`

- [ ] **Step 1: Add async-trait dependency**

Add to `ia-core/Cargo.toml` dependencies (alphabetical, after `async-stream`):

```toml
async-trait = "0.1"
```

- [ ] **Step 2: Write TimingMiddleware**

Add to `ia-core/src/retry.rs`:

```rust
use std::time::Instant;

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
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ia-core`
Expected: success (no errors).

- [ ] **Step 4: Commit**

```bash
git add ia-core/src/retry.rs ia-core/Cargo.toml
git commit -m "feat: add TimingMiddleware for per-request latency tracking

Wraps the retry middleware to capture wall-clock time including
retry waits. Records latency to RetryStats for percentile
computation. Requires async-trait for reqwest-middleware Middleware
trait implementation.

Part of #309"
```

---

### Task 6: Wire Middleware into IaClient

**Files:**
- Modify: `ia-core/src/client.rs`

- [ ] **Step 1: Write failing test for retry_stats accessor**

Add to the `tests` module in `ia-core/src/client.rs`:

```rust
    #[test]
    fn client_exposes_retry_stats() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        let stats = client.retry_stats();
        assert_eq!(stats.summary().requests_total, 0);
        assert!(!stats.had_retries());
    }

    #[test]
    fn client_with_verbosity() {
        let client =
            IaClient::from_config_with_verbosity(IaConfig::default(), 2).unwrap();
        // Just verify it builds and returns stats
        assert_eq!(client.retry_stats().summary().requests_total, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core client::tests::client_exposes`
Expected: compile error — `retry_stats()` method doesn't exist.

- [ ] **Step 3: Implement client changes**

Modify `ia-core/src/client.rs`:

Add import at top:
```rust
use crate::retry::{LoggingRetryStrategy, RetryStats, TimingMiddleware};
use std::sync::Arc;
```

Add field to `IaClient`:
```rust
pub struct IaClient {
    http: ClientWithMiddleware,
    raw_http: reqwest::Client,
    no_redirect_http: reqwest::Client,
    config: IaConfig,
    user_agent: String,
    retry_stats: Arc<RetryStats>,
}
```

Add `from_config_with_verbosity`:
```rust
    /// Create a new client with the provided config and verbosity level.
    ///
    /// `verbosity` controls diagnostic output detail (0 = dedup warnings,
    /// 1+ = individual events). See [`RetryStats`] for details.
    pub fn from_config_with_verbosity(config: IaConfig, verbosity: u8) -> Result<Self> {
        let (raw_client, no_redirect_client, user_agent) = Self::build_raw_client(&config)?;

        let retry_policy = ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(60),
            )
            .build_with_max_retries(3);

        let stats = Arc::new(RetryStats::new(verbosity));
        let strategy = LoggingRetryStrategy::new(stats.clone());

        let raw_http = raw_client.clone();

        let http = ClientBuilder::new(raw_client)
            .with(TimingMiddleware::new(stats.clone()))
            .with(RetryTransientMiddleware::new_with_policy_and_strategy(
                retry_policy, strategy,
            ))
            .build();

        Ok(Self {
            http,
            raw_http,
            no_redirect_http: no_redirect_client,
            config,
            user_agent,
            retry_stats: stats,
        })
    }
```

Update existing `from_config` to delegate:
```rust
    pub fn from_config(config: IaConfig) -> Result<Self> {
        Self::from_config_with_verbosity(config, 0)
    }
```

Update `from_config_no_retry` to include stats:
```rust
    pub fn from_config_no_retry(config: IaConfig) -> Result<Self> {
        let (raw_client, no_redirect_client, user_agent) = Self::build_raw_client(&config)?;
        let stats = Arc::new(RetryStats::new(0));
        let raw_http = raw_client.clone();
        let http = ClientBuilder::new(raw_client).build();

        Ok(Self {
            http,
            raw_http,
            no_redirect_http: no_redirect_client,
            config,
            user_agent,
            retry_stats: stats,
        })
    }
```

Add accessor:
```rust
    /// Shared HTTP diagnostic counters.
    pub fn retry_stats(&self) -> &RetryStats {
        &self.retry_stats
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core client::tests`
Expected: all client tests pass (existing + 2 new).

- [ ] **Step 5: Run full ia-core test suite**

Run: `cargo test -p ia-core`
Expected: all tests pass — no existing tests broken.

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/client.rs
git commit -m "feat: wire RetryStats and TimingMiddleware into IaClient

Add retry_stats field to IaClient. TimingMiddleware (outer) wraps
RetryTransientMiddleware with LoggingRetryStrategy (inner).
from_config() delegates to from_config_with_verbosity(config, 0)
so all existing call sites are unchanged.

Part of #309"
```

---

### Task 7: Integration Tests — Wiremock 429/503/200

**Files:**
- Create: `ia-core/tests/retry.rs`

- [ ] **Step 1: Write integration tests**

Create `ia-core/tests/retry.rs`:

```rust
//! Integration tests for HTTP diagnostics middleware.
//!
//! ALL tests use wiremock — ZERO live requests to archive.org.

use ia_core::{IaClient, IaConfig};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_config(server_uri: &str) -> IaConfig {
    let mut config = IaConfig::default();
    let host = server_uri
        .strip_prefix("http://")
        .or_else(|| server_uri.strip_prefix("https://"))
        .unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.secure = false;
    config
}

#[tokio::test]
async fn successful_request_records_latency() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": []
        })))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let _resp = client
        .http()
        .get(format!("{}/metadata/test-item", server.uri()))
        .send()
        .await
        .unwrap();

    let stats = client.retry_stats();
    assert_eq!(stats.summary().requests_total, 1);
    assert!(!stats.had_retries());

    let p = stats.percentiles().unwrap();
    assert!(p.p50_ms < 5000, "latency should be reasonable for local mock");
}

#[tokio::test]
async fn request_429_records_retry_stats() {
    let server = MockServer::start().await;

    // First call: 429, second call: 429, third call: 200
    Mock::given(method("GET"))
        .and(path("/metadata/rate-limited"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/rate-limited"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/rate-limited", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let stats = client.retry_stats();
    assert!(stats.had_retries());
    let s = stats.summary();
    assert_eq!(s.status_429_count, 2);
    assert_eq!(s.total_retry_wait, Duration::from_secs(60)); // 30 * 2
    assert_eq!(s.requests_total, 1); // one logical request
}

#[tokio::test]
async fn request_503_records_server_error_stats() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/server-error"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/server-error"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/server-error", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let stats = client.retry_stats();
    assert!(stats.had_retries());
    let s = stats.summary();
    assert_eq!(s.status_5xx_count, 1);
    assert_eq!(s.status_429_count, 0);
}

#[tokio::test]
async fn mixed_429_and_5xx_retries() {
    let server = MockServer::start().await;

    // Sequence: 429 → 503 → 200
    Mock::given(method("GET"))
        .and(path("/metadata/mixed"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "10"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/mixed"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/mixed"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/mixed", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let s = client.retry_stats().summary();
    assert_eq!(s.retries_total, 2);
    assert_eq!(s.status_429_count, 1);
    assert_eq!(s.status_5xx_count, 1);
    assert_eq!(s.total_retry_wait, Duration::from_secs(10));
}

#[tokio::test]
async fn multiple_requests_accumulate_stats() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/item1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/item2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    client
        .http()
        .get(format!("{}/metadata/item1", server.uri()))
        .send()
        .await
        .unwrap();
    client
        .http()
        .get(format!("{}/metadata/item2", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(client.retry_stats().summary().requests_total, 2);
    assert!(client.retry_stats().percentiles().is_some());
}

#[tokio::test]
async fn verbosity_propagated_through_client() {
    let client = IaClient::from_config_with_verbosity(IaConfig::default(), 2).unwrap();
    // Stats are created with the requested verbosity — verify through
    // successful construction (verbosity is internal to RetryStats).
    assert_eq!(client.retry_stats().summary().requests_total, 0);
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p ia-core --test retry`
Expected: all 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add ia-core/tests/retry.rs
git commit -m "test: add wiremock integration tests for HTTP diagnostics

Tests 429/503/200 retry recording through full client middleware
stack, mixed retry scenarios, multi-request accumulation, and
verbosity propagation.

Part of #309"
```

---

### Task 8: Replace --debug with --verbose in CLI

**Files:**
- Modify: `ia-cli/src/main.rs`
- Modify: `ia-cli/tests/cli.rs`

- [ ] **Step 1: Update CLI test expectation first**

In `ia-cli/tests/cli.rs`, change the `help_shows_global_options` test:

Replace:
```rust
        .stdout(predicate::str::contains("--debug"))
```
With:
```rust
        .stdout(predicate::str::contains("--verbose"))
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ia-cli --test cli help_shows_global_options`
Expected: FAIL — help still shows `--debug`.

- [ ] **Step 3: Update CLI flag in main.rs**

In `ia-cli/src/main.rs`, replace the debug field in the `Cli` struct:

Replace:
```rust
    /// Enable debug output
    #[arg(short = 'd', long, global = true, help_heading = "Global Options")]
    debug: bool,
```

With:
```rust
    /// Increase output verbosity (-v, -vv, -vvv)
    #[arg(
        short = 'v',
        long,
        global = true,
        action = clap::ArgAction::Count,
        help_heading = "Global Options",
    )]
    verbose: u8,
```

- [ ] **Step 4: Update tracing subscriber setup in main.rs**

Replace this block:
```rust
    let log_level = if dashboard_active {
        "off"
    } else if cli.debug {
        "debug"
    } else if cli.log {
        "info"
    } else {
        "warn"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_writer(std::io::stderr)
        .init();
```

With:
```rust
    let log_level = if dashboard_active {
        "off"
    } else {
        match cli.verbose {
            0 if cli.log => "info",
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
```

- [ ] **Step 5: Update client construction to pass verbosity**

In `ia-cli/src/main.rs`, replace:
```rust
    let client = ia_core::IaClient::from_config(config)?;
```

With:
```rust
    let client = ia_core::IaClient::from_config_with_verbosity(config, cli.verbose)?;
```

- [ ] **Step 6: Run CLI test to verify it passes**

Run: `cargo test -p ia-cli --test cli help_shows_global_options`
Expected: PASS.

- [ ] **Step 7: Run full CLI test suite**

Run: `cargo test -p ia-cli`
Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add ia-cli/src/main.rs ia-cli/tests/cli.rs
git commit -m "feat: replace --debug with --verbose (-v/-vv/-vvv)

Graduated verbosity levels:
  (none) = WARN — rate-limit state transitions + end summary
  -v     = INFO — each retry event with details
  -vv    = DEBUG — per-request timing
  -vvv   = TRACE — full HTTP details

Passes verbosity to IaClient so RetryStats can control dedup
behavior. Removes -d/--debug flag. Updates CLI integration test.

Closes #309"
```

---

### Task 9: print_retry_summary in output.rs

**Files:**
- Modify: `ia-cli/src/output.rs`

- [ ] **Step 1: Add print_retry_summary function**

Add to end of `ia-cli/src/output.rs` (before any test modules, or at the very end if no tests exist in this file):

```rust
/// Print HTTP retry summary to stderr if any retries occurred.
///
/// Shows retry count, 429/5xx breakdown, total server-requested wait time,
/// and latency percentiles. Called at end of batch operations.
pub fn print_retry_summary(stats: &ia_core::RetryStats) {
    if !stats.had_retries() {
        return;
    }
    let s = stats.summary();
    eprintln!(
        "  {} {} requests retried ({} rate-limited, {} server errors), {:.0}s total wait",
        style("\u{26a0}").yellow(),
        s.retries_total,
        s.status_429_count,
        s.status_5xx_count,
        s.total_retry_wait.as_secs_f64(),
    );
    if let Some(p) = stats.percentiles() {
        eprintln!(
            "  {} p50={}ms p95={}ms p99={}ms",
            style("latency:").dim(),
            p.p50_ms,
            p.p95_ms,
            p.p99_ms,
        );
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add ia-cli/src/output.rs
git commit -m "feat: add print_retry_summary helper for end-of-command diagnostics

Prints retry count, 429/5xx breakdown, server-requested wait time,
and latency percentiles to stderr. Only prints if retries occurred.

Part of #309"
```

---

### Task 10: Integrate Retry Summary into Metadata Export

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

- [ ] **Step 1: Add import for print_retry_summary**

In `ia-cli/src/commands/metadata.rs`, update the crate output import line:

Replace:
```rust
use crate::output::{format_bytes, BAR_WIDTH, ICON_ERROR, ICON_SUCCESS, PROGRESS_CHARS};
```

With:
```rust
use crate::output::{
    format_bytes, print_retry_summary, BAR_WIDTH, ICON_ERROR, ICON_SUCCESS, PROGRESS_CHARS,
};
```

- [ ] **Step 2: Add retry summary call to export (file output path)**

In the `run_export` function, after the `print_export_summary(...)` call for the file output path (around line 1491-1500), add a retry summary call.

After this existing block:
```rust
        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            skipped,
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            Some(path.as_path()),
            quiet,
        );
```

Add:
```rust
        if quiet < 2 {
            print_retry_summary(client.retry_stats());
        }
```

- [ ] **Step 3: Add retry summary call to export (stdout path)**

After the second `print_export_summary(...)` call for the stdout path (around line 1566-1575), add the same retry summary call.

After this existing block:
```rust
        print_export_summary(
            succeeded.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
            skipped,
            bytes_total.load(Ordering::Relaxed),
            start.elapsed().as_secs_f64(),
            errors_shown.load(Ordering::Relaxed),
            None,
            quiet,
        );
```

Add:
```rust
        if quiet < 2 {
            print_retry_summary(client.retry_stats());
        }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: success.

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p ia-core`

Then: `cargo test -p ia-cli`

Expected: all tests pass in both crates.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Run fmt check**

Run: `cargo fmt -p ia-core -p ia-cli -- --check`
Expected: no formatting issues.

- [ ] **Step 8: Commit**

```bash
git add ia-cli/src/commands/metadata.rs
git commit -m "feat: integrate retry summary into metadata export

Prints HTTP retry diagnostics (429/5xx counts, wait time, latency
percentiles) at end of metadata export when retries occurred.
Suppressed at -qq quiet level.

Part of #309"
```

---

### Task 11: Update AGENTS.md Reserved Flags

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update reserved flags list**

In `AGENTS.md`, find the reserved flags line:

Replace:
```
`-c` (config-file), `-l` (log), `-d` (debug), `-i` (insecure), `-H` (host), `-j` (jobs), `-q` (quiet)
```

With:
```
`-c` (config-file), `-l` (log), `-v` (verbose), `-i` (insecure), `-H` (host), `-j` (jobs), `-q` (quiet)
```

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update reserved CLI flags (-d/debug -> -v/verbose)

Part of #309"
```

---

### Task 12: Update docs/usage.md

**Files:**
- Modify: `docs/usage.md`

- [ ] **Step 1: Check for --debug references**

Search `docs/usage.md` for `--debug` or `-d` flag references and update to `--verbose`/`-v`.

- [ ] **Step 2: Commit if changes needed**

```bash
git add docs/usage.md
git commit -m "docs: update usage.md for --verbose flag

Part of #309"
```

---

### Task 13: Final Verification

- [ ] **Step 1: Run full test suite (ia-core)**

Run: `cargo test -p ia-core`
Expected: all tests pass.

- [ ] **Step 2: Run full test suite (ia-cli)**

Run: `cargo test -p ia-cli`
Expected: all tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Run fmt check**

Run: `cargo fmt -p ia-core -p ia-cli -- --check`
Expected: clean.

- [ ] **Step 5: Run doc check**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p ia-core -p ia-cli --no-deps`
Expected: no warnings.
