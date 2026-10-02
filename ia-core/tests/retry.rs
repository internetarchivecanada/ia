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
    assert!(
        p.p50_ms < 5000,
        "latency should be reasonable for local mock"
    );
}

#[tokio::test]
async fn request_429_passes_through_without_retry() {
    let server = MockServer::start().await;

    // 429 is NOT retried by middleware — it passes through to the caller.
    Mock::given(method("GET"))
        .and(path("/metadata/rate-limited"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/rate-limited", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 429, "429 should pass through to caller");

    let stats = client.retry_stats();
    assert!(stats.had_retries());
    let s = stats.summary();
    assert_eq!(s.status_429_count, 1);
    assert_eq!(s.retries_total, 0, "429s are not middleware retries");
    assert_eq!(s.total_retry_wait, Duration::from_secs(30));
    assert_eq!(s.requests_total, 1);
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
async fn mixed_5xx_then_429_passes_429_through() {
    let server = MockServer::start().await;

    // Sequence: 503 -> 429. Middleware retries the 503 (transient),
    // then gets a 429 which passes through (not retried).
    Mock::given(method("GET"))
        .and(path("/metadata/mixed"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metadata/mixed"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "10"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/mixed", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        429,
        "429 should pass through after 503 retry"
    );

    let s = client.retry_stats().summary();
    assert_eq!(s.retries_total, 1, "only the 503 is a middleware retry");
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
    assert_eq!(client.retry_stats().summary().requests_total, 0);
}

// ── Retry-After on middleware retries ─────────────────────────────────────
//
// The transport middleware retries a 5xx. The wait before the retry is the
// server's Retry-After when the response carried one, otherwise the standard
// backoff. These run through `IaClient::from_config`, the production stack.

/// A 503 with `Retry-After: 1` is retried after at least one second, not
/// after the backoff's own draw.
#[tokio::test]
async fn middleware_503_with_retry_after_waits_the_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/slow"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/slow"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let started = std::time::Instant::now();
    let resp = client
        .http()
        .get(format!("{}/metadata/slow", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "Retry-After: 1 was not waited for ({:?})",
        started.elapsed()
    );
    server.verify().await;
}

/// The HTTP-date form is honored too. The date is fixed right before the
/// request, 3 s ahead, so the wait is at least 1 s even on a slow runner
/// (one-second granularity; see the PR #31 plan doc).
#[tokio::test]
async fn middleware_503_with_http_date_retry_after_waits() {
    let server = MockServer::start().await;
    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let when = std::time::SystemTime::now() + Duration::from_secs(3);
    Mock::given(method("GET"))
        .and(path("/metadata/dated"))
        .respond_with(
            ResponseTemplate::new(503).insert_header("retry-after", httpdate::fmt_http_date(when)),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/dated"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let resp = client
        .http()
        .get(format!("{}/metadata/dated", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "HTTP-date Retry-After was not waited for ({:?})",
        started.elapsed()
    );
    server.verify().await;
}

/// `Retry-After: 0` means re-send at once, and the budget is still three
/// retries: four requests in all, then the last response is handed back.
/// (Without the header the three waits would be a jittered 1 s + 2 s + 4 s.)
#[tokio::test]
async fn middleware_retry_after_zero_resends_at_once_within_the_budget() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/down"))
        .respond_with(ResponseTemplate::new(500).insert_header("retry-after", "0"))
        .expect(4)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let started = std::time::Instant::now();
    let resp = client
        .http()
        .get(format!("{}/metadata/down", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500, "the final response is handed back");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Retry-After: 0 was not honored as an immediate retry ({:?})",
        started.elapsed()
    );
    // The mock's expect(4) is the attempt count; `requests_total` counts
    // outer calls (the timing middleware wraps the retry middleware).
    assert_eq!(client.retry_stats().summary().status_5xx_count, 4);
    server.verify().await;
}

/// When the budget is spent, the error handed to the caller carries the
/// last response's Retry-After, so a caller retrying on the error object
/// can honor it. Here through `get_item`, whose error is built in
/// `metadata::read`. (`Retry-After: 0` keeps the three honored waits at
/// zero; `Some(0)` against `None` is what is being proved.)
#[tokio::test]
async fn http_error_after_the_budget_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/busy"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "0")
                .set_body_string("busy"),
        )
        .expect(4)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let err = client
        .get_item("busy")
        .await
        .expect_err("503 after the budget");
    assert_eq!(err.retry_after(), Some(0), "got {err:?}");
    server.verify().await;
}
