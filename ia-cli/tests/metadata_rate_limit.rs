//! `ia metadata modify` pauses and retries an item when archive.org answers
//! 429 Too Many Requests.
//!
//! All tests are hermetic: every request goes to a wiremock server.

use std::fs;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::NamedTempFile;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config_with_keys(host: &str) -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(
        f.path(),
        format!(
            "[general]\nhost = {host}\nsecure = false\n\n[s3]\naccess = test-access\nsecret = test-secret\n"
        ),
    )
    .unwrap();
    f
}

fn item_metadata() -> serde_json::Value {
    json!({
        "metadata": {"identifier": "test-item", "title": "Old"},
        "files": []
    })
}

/// A 429 on the metadata read that precedes the write pauses the batch for
/// the Retry-After interval and retries the item, the same as a 429 on the
/// write itself, instead of recording the item as failed.
#[tokio::test]
async fn modify_retries_after_429_on_metadata_read() {
    let server = MockServer::start().await;

    // First read: 429 with Retry-After. Mounted first, so it answers the
    // first request; later reads fall through to the 200 below.
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item_metadata()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"success": true, "task_id": 42})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let cfg = config_with_keys(&host);
    let start = Instant::now();
    let output = assert_cmd::cargo_bin_cmd!("ia-cli")
        .arg("--config-file")
        .arg(cfg.path())
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .args(["metadata", "modify", "test-item", "-m", "title:New"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "modify should succeed after the pause; stderr: {stderr}"
    );
    assert!(
        stderr.contains("Rate limited. Pausing all workers for 1s"),
        "expected the batch pause message; stderr: {stderr}"
    );
    assert!(
        elapsed >= Duration::from_secs(1),
        "the retry must wait the Retry-After interval; took {elapsed:?}"
    );
    // Dropping `server` verifies each mock's `expect` count.
}
