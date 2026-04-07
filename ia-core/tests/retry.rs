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
