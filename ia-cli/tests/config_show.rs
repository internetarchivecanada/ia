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
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "show"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("REDACTED"), "secrets should be redacted: {stdout}");
    assert!(stdout.contains("archive.org"), "should show host: {stdout}");
    assert!(!stdout.contains("test-access"), "should NOT show raw access key: {stdout}");
}

#[test]
fn config_show_json_mode() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "show", "--json"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("should be valid JSON: {stdout}"));
    assert!(parsed.get("general").is_some());
}
