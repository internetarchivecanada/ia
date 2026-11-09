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
    // The mock's expect(4) is the attempt count; each attempt is a request.
    let s = client.retry_stats().summary();
    assert_eq!(s.status_5xx_count, 4);
    assert_eq!(s.requests_total, 4);
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

// ── retries_total counts retries made, not 5xx answers (#132) ──────────────

/// A read whose every attempt answers 5xx is retried three times (the
/// standard budget) and then handed back: four attempts, three retries.
#[tokio::test]
async fn exhausted_read_counts_only_the_retries_made() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/down"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .expect(4)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/down", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);

    let s = client.retry_stats().summary();
    assert_eq!(s.status_5xx_count, 4, "every 5xx answer is counted");
    assert_eq!(s.retries_total, 3, "the last 5xx was not retried");
    server.verify().await;
}

/// A read that gets one 5xx and then succeeds made exactly one retry.
#[tokio::test]
async fn successful_retry_counts_one_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/flaky"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/flaky"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let resp = client
        .http()
        .get(format!("{}/metadata/flaky", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let s = client.retry_stats().summary();
    assert_eq!(s.status_5xx_count, 1);
    assert_eq!(s.retries_total, 1);
    server.verify().await;
}

/// A metadata write is never replayed on a 5xx, so a 5xx answer to it is a
/// server error and not a retry.
#[tokio::test]
async fn unretried_write_5xx_is_not_counted_as_a_retry() {
    use ia_core::metadata::write::{modify, MetadataOp, ModifyRequest};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/write-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "write-item", "title": "old"},
            "files": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/metadata/write-item"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = mock_config(&server.uri());
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    let client = IaClient::from_config(config).unwrap();
    let req = ModifyRequest {
        identifier: "write-item".to_string(),
        changes: vec![("title".to_string(), serde_json::json!("new"))],
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    assert!(modify(&client, &req).await.is_err(), "the 503 surfaces");

    let s = client.retry_stats().summary();
    assert_eq!(s.status_5xx_count, 1);
    assert_eq!(s.retries_total, 0, "a write is not retried on a 5xx");
    assert!(
        client.retry_stats().had_retries(),
        "a server error is still reported in the batch summary"
    );
    server.verify().await;
}

// ── /download/ requests reach the same stats (#122) ────────────────────────

/// The download transport bypasses the client's middleware; its requests,
/// their 429 and 5xx answers and the download loop's resends are recorded
/// in the same `RetryStats` as the metadata request.
#[tokio::test]
async fn download_requests_and_retries_are_recorded() {
    use ia_core::download::{download_item, DownloadOpts};
    use std::sync::Arc;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/dl-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "dl-item"},
            "files": [{"name": "f.txt", "source": "original", "size": "5"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/dl-item/f.txt"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/dl-item/f.txt"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/dl-item/f.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let mut opts = DownloadOpts::default();
    opts.destdir = dir.path().to_path_buf();
    let result = download_item(
        &client,
        "dl-item",
        &opts,
        Arc::new(tokio::sync::Semaphore::new(1)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.files_downloaded, 1);

    let s = client.retry_stats().summary();
    assert_eq!(
        s.requests_total, 4,
        "1 metadata request + 3 download attempts"
    );
    assert_eq!(s.status_5xx_count, 1);
    assert_eq!(s.status_429_count, 1);
    assert_eq!(s.retries_total, 2, "the 503 and the 429 were each resent");
    server.verify().await;
}

/// Each attempt the client's retry middleware sends is an HTTP request:
/// a read answered 503 and then 200 is two requests, each with its own
/// latency sample, as a `/download/` request and its resend are.
#[tokio::test]
async fn middleware_resend_is_a_second_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/twice"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/twice"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
    client
        .http()
        .get(format!("{}/metadata/twice", server.uri()))
        .send()
        .await
        .unwrap();

    let s = client.retry_stats().summary();
    assert_eq!(s.requests_total, 2);
    assert_eq!(s.retries_total, 1);
    server.verify().await;
}

/// A body cut off mid-stream is re-requested with `Range` inside
/// `download_file`; that re-request is a request and a retry.
///
/// A raw loopback listener, because wiremock cannot send headers that
/// promise more body than it then sends.
#[tokio::test]
async fn download_range_rerequest_is_counted_as_a_retry() {
    use ia_core::download::{download_file, DownloadOpts};
    use ia_core::types::FileMetadata;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = vec![0u8; 4096];
        let mut acc = Vec::new();
        while !acc.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&acc).to_ascii_lowercase()
    }

    let body = b"0123456789abcdef";
    let cut = 6;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        // First request: headers promise the whole body; close after `cut`.
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(&body[..cut]).await.unwrap();
        stream.flush().await.unwrap();
        drop(stream);

        // Second request: the Range re-request; answer the rest.
        let (mut stream, _) = listener.accept().await.unwrap();
        let req = read_request(&mut stream).await;
        assert!(req.contains(&format!("range: bytes={cut}-")), "{req}");
        let head = format!(
            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\n\
             content-range: bytes {cut}-{}/{}\r\nconnection: close\r\n\r\n",
            body.len() - cut,
            body.len() - 1,
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(&body[cut..]).await.unwrap();
        stream.flush().await.unwrap();
    });

    let mut config = IaConfig::default();
    config.general.host = format!("127.0.0.1:{port}");
    config.general.secure = false;
    let client = IaClient::from_config(config).unwrap();
    let file: FileMetadata = serde_json::from_value(serde_json::json!({
        "name": "cut.bin",
        "size": body.len().to_string(),
    }))
    .unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let result = download_file(
        &client,
        "cut-item",
        &file,
        dir.path(),
        &DownloadOpts::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.bytes, body.len() as u64);
    server.await.unwrap();

    let s = client.retry_stats().summary();
    assert_eq!(s.requests_total, 2);
    assert_eq!(s.retries_total, 1, "the Range re-request is a retry");
}
