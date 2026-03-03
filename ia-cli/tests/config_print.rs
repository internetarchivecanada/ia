use assert_cmd::Command;
use std::io::Write;

#[test]
fn print_auth_outputs_header() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = myaccess").unwrap();
    writeln!(f, "secret = mysecret").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "Authorization: LOW myaccess:mysecret");
}

#[test]
fn print_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = myaccess").unwrap();
    writeln!(f, "secret = mysecret").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth", "--json"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["header"], "Authorization: LOW myaccess:mysecret");
}

#[test]
fn print_cookies_outputs_netscape_format() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[cookies]").unwrap();
    writeln!(f, "logged-in-user = user%40example.com").unwrap();
    writeln!(f, "logged-in-sig = test-sig").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-cookies"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("logged-in-user"), "should contain cookie name: {stdout}");
    assert!(stdout.contains("user%40example.com"), "should contain cookie value: {stdout}");
}

#[test]
fn print_auth_no_credentials_fails() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth"]);
    cmd.assert().failure();
}
