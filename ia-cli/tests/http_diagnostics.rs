//! The `HTTP:` diagnostics line that `ia download` prints to stderr.
//!
//! ALL tests use wiremock — ZERO live requests to archive.org.

use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FILES: [(&str, &[u8]); 3] = [
    ("a.txt", b"alpha"),
    ("b.txt", b"bravo!"),
    ("c.txt", b"charlie"),
];

/// An item of three small files. Each `/download/` request is redirected to
/// a data-node path on the same host, as archive.org redirects to
/// `ia8xxxxx.us.archive.org`, so each file costs two HTTP requests.
async fn mount_three_file_item(server: &MockServer) {
    let files: Vec<_> = FILES
        .iter()
        .map(|(name, body)| {
            json!({
                "name": name,
                "source": "original",
                "size": body.len().to_string(),
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/metadata/three-files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"identifier": "three-files"},
            "files": files,
        })))
        .expect(1)
        .mount(server)
        .await;

    for (name, body) in FILES {
        Mock::given(method("GET"))
            .and(path(format!("/download/three-files/{name}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("/node/items/three-files/{name}")),
            )
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/node/items/three-files/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(server)
            .await;
    }
}

fn download(server: &MockServer, dir: &TempDir, extra: &[&str]) -> std::process::Output {
    let host = server.uri().replace("http://", "");
    let mut args = vec![
        "--insecure",
        "-H",
        &host,
        "download",
        "three-files",
        "--destdir",
        dir.path().to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    assert_cmd::cargo_bin_cmd!("ia-cli")
        .args(&args)
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .env("IA_CONFIG_FILE", dir.path().join("no-such-ia.ini"))
        .output()
        .unwrap()
}

/// One metadata request, then a `/download/` request and its redirect for
/// each of the three files: seven requests.
#[tokio::test]
async fn download_http_line_counts_every_request() {
    let server = MockServer::start().await;
    mount_three_file_item(&server).await;
    let dir = TempDir::new().unwrap();

    let output = download(&server, &dir, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "download failed: {stderr}");
    let line = stderr
        .lines()
        .find(|l| l.starts_with("HTTP:"))
        .unwrap_or_else(|| panic!("no HTTP: line in stderr: {stderr}"));
    assert!(
        line.starts_with("HTTP: 7 requests"),
        "expected 7 requests, got: {line}"
    );
    server.verify().await;
}

/// `--json` prints one JSON object per file on stdout and no `HTTP:` line.
#[tokio::test]
async fn download_json_has_no_http_line() {
    let server = MockServer::start().await;
    mount_three_file_item(&server).await;
    let dir = TempDir::new().unwrap();

    let output = download(&server, &dir, &["--json"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "download failed: {stderr}");
    assert!(
        !stderr.lines().any(|l| l.starts_with("HTTP:")),
        "--json must not print the HTTP: line: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("not JSON: {l}")))
        .collect();
    assert_eq!(lines.len(), 3, "one JSON line per file: {stdout}");
}
