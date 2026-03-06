//! Test whether reqwest respects Expect: 100-continue.
//!
//! If the server rejects before sending 100 Continue,
//! does reqwest avoid sending the body?

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn expect_100_continue_server_rejects_before_body() {
    let server = MockServer::start().await;

    // Mock that returns 403 immediately (no 100 Continue)
    Mock::given(method("PUT"))
        .and(path("/test-item/file.bin"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Access Denied"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let body = vec![0u8; 1_000_000]; // 1 MB body

    let response = client
        .put(format!("{}/test-item/file.bin", server.uri()))
        .header("Expect", "100-continue")
        .header("Content-Length", body.len().to_string())
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 403);

    // Note: We cannot easily verify that the body was NOT sent with wiremock.
    // This test verifies the basic flow works. True 100-continue verification
    // requires monitoring bytes on the wire, which we'll do manually with curl.
    //
    // The real verification: run this against IA with x-archive-simulate-error:AccessDenied
    // and observe that curl with -v shows body is not sent after 403.
}

#[tokio::test]
async fn expect_100_continue_server_accepts() {
    let server = MockServer::start().await;

    // Mock that accepts the upload
    Mock::given(method("PUT"))
        .and(path("/test-item/file.bin"))
        .and(header("Expect", "100-continue"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let body = vec![42u8; 1024]; // 1 KB body

    let response = client
        .put(format!("{}/test-item/file.bin", server.uri()))
        .header("Expect", "100-continue")
        .header("Content-Length", body.len().to_string())
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}
