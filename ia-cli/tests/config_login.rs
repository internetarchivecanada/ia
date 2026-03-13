#[test]
fn config_login_missing_password_flag_without_tty() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");

    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.args([
        "--config-file",
        ini_path.to_str().unwrap(),
        "config",
        "login",
        "-u",
        "user@example.com",
    ]);
    cmd.assert().failure();
}
