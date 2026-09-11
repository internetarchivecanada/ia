use assert_cmd::Command;
use predicates::prelude::*;
use serde_json;
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ia_cmd() -> Command {
    assert_cmd::cargo_bin_cmd!("ia-cli")
}

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia-cli");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn verify_no_args_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("verify")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_identifier_only_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["verify", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_alias_ve_works() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["ve", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_invalid_checksum_type_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["verify", "my-item", "file.txt", "--checksum-type", "sha512"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown algorithm"));
}

#[test]
fn verify_spreadsheet_conflicts_with_identifier() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::new().unwrap();
    writeln!(spreadsheet, "identifier,file").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "my-item",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn verify_checksum_file_satisfies_file_requirement() {
    // --checksum-file should satisfy the "files required" constraint
    // (will fail for other reasons like missing credentials, but NOT for missing files arg)
    let cfg = empty_config();
    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "d41d8cd98f00b204e9800998ecf8427e  file.txt").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "my-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        // Should fail because of credentials, not because of missing files
        .stderr(predicate::str::contains("required").not());
}

#[test]
fn verify_nonexistent_file_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "verify",
            "my-item",
            "/tmp/ia-test-nonexistent-file-12345.txt",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("No such file")));
}

// ─── Spreadsheet validation ──────────────────────────────────────────────────

#[test]
fn verify_spreadsheet_missing_identifier_column_errors() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "file,md5").unwrap();
    writeln!(spreadsheet, "test.txt,abc123").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn verify_spreadsheet_missing_file_column_errors() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "identifier,md5").unwrap();
    writeln!(spreadsheet, "test-item,abc123").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

// ─── Wiremock integration tests ─────────────────────────────────────────────

#[tokio::test]
async fn verify_all_files_verified_exit_0() {
    let server = MockServer::start().await;

    // Create temp file with known content
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "source": "original",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "sha1": "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                "crc32": "3610a686",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_directory_recursive() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(dir.path().join("a.txt"), b"hello").unwrap();
    fs::write(sub.join("b.txt"), b"hello").unwrap();

    // Both files have same content, same MD5
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "a.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }, {
                "name": "b.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    // Verify the entire directory (should recurse into sub/)
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(dir.path().to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_missing_file_exit_1() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "other.txt",
                "md5": "different_hash",
                "size": "100"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_json_output_format() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item", "--json"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\":\"verified\""));
    assert!(stdout.contains("\"identifier\":\"test-item\""));
    assert!(stdout.contains("\"local_file\":\"hello.txt\""));
}

#[tokio::test]
async fn verify_quiet_no_output() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args(["--insecure", "-H", &host, "-q", "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn verify_checksum_file_no_local_files() {
    let server = MockServer::start().await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
    writeln!(checksum_file, "5d41402abc4b2a76b9719d911017c592  hello.txt").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_sha1_checksum_type() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "sha1": "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-type",
            "sha1",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_match_names_mismatch_exit_1() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    // Remote has same hash but different name — --match-names should fail
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "renamed.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    // Without --match-names: should succeed (hash matches)
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();

    // With --match-names: should fail (name doesn't match)
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--match-names",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_glob_filters_remote_files() {
    let server = MockServer::start().await;

    let hash = "d41d8cd98f00b204e9800998ecf8427e";
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {
                    "name": "file.txt",
                    "md5": hash,
                    "size": "100"
                },
                {
                    "name": "file.pdf",
                    "md5": hash,
                    "size": "100"
                }
            ]
        })))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "{}  local.txt", hash).unwrap();

    let host = server.uri().replace("http://", "");

    // With --glob '*.pdf': should only match against file.pdf
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
            "--glob",
            "*.pdf",
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should match file.pdf (passes glob filter)
    assert!(stdout.contains("\"remote_key\":\"file.pdf\""));
}

#[tokio::test]
async fn verify_metadata_fetch_error_exit_1() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "d41d8cd98f00b204e9800998ecf8427e  file.txt").unwrap();

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_source_filter_original_only() {
    let server = MockServer::start().await;

    let hash = "d41d8cd98f00b204e9800998ecf8427e";
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {
                    "name": "file.txt",
                    "source": "original",
                    "md5": hash,
                    "size": "100"
                },
                {
                    "name": "file_thumb.txt",
                    "source": "derivative",
                    "md5": hash,
                    "size": "50"
                }
            ]
        })))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "{}  local.txt", hash).unwrap();

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
            "--source",
            "original",
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should match file.txt (original), not file_thumb.txt (derivative)
    assert!(stdout.contains("\"remote_key\":\"file.txt\""));
}

#[tokio::test]
async fn verify_spreadsheet_with_hash_column_e2e() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "file.txt",
                "md5": "abc123",
                "size": "100"
            }]
        })))
        .mount(&server)
        .await;

    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "identifier,file,md5").unwrap();
    writeln!(spreadsheet, "test-item,file.txt,abc123").unwrap();

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}
