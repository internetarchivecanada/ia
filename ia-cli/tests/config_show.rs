use assert_cmd::Command;
use std::io::Write;

#[test]
fn config_show_displays_json() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = test-access").unwrap();
    writeln!(f, "secret = test-secret").unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args([
        "--config-file",
        ini_path.to_str().unwrap(),
        "config",
        "show",
    ]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Identifiers should be shown
    assert!(
        stdout.contains("test-access"),
        "should show access key (identifier): {stdout}"
    );
    assert!(stdout.contains("archive.org"), "should show host: {stdout}");
    // Secrets should be redacted
    assert!(
        stdout.contains("REDACTED"),
        "secrets should be redacted: {stdout}"
    );
    assert!(
        !stdout.contains("test-secret"),
        "should NOT show raw secret key: {stdout}"
    );
}

#[test]
fn config_show_json_mode() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args([
        "--config-file",
        ini_path.to_str().unwrap(),
        "config",
        "show",
        "--json",
    ]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("should be valid JSON: {stdout}"));
    assert!(parsed.get("general").is_some());
}

#[test]
fn config_show_secrets_flag() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = test-access").unwrap();
    writeln!(f, "secret = test-secret").unwrap();
    writeln!(f, "[cookies]").unwrap();
    writeln!(f, "logged-in-sig = secret-sig").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args([
        "--config-file",
        ini_path.to_str().unwrap(),
        "config",
        "show",
        "--show-secrets",
    ]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("test-secret"),
        "should show raw secret key: {stdout}"
    );
    assert!(
        stdout.contains("secret-sig"),
        "should show raw cookie sig: {stdout}"
    );
    assert!(
        !stdout.contains("REDACTED"),
        "should not contain REDACTED: {stdout}"
    );
}
