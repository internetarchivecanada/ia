//! A control character in the User-Agent suffix is reported as an error, from
//! both the `--user-agent-suffix` flag and the config file. The client is
//! built before any request, so nothing is sent.

fn run(ini: &str, extra: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    std::fs::write(&ini_path, ini).unwrap();
    let mut args = vec!["--config-file", ini_path.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["metadata", "nasa"]);
    assert_cmd::cargo_bin_cmd!("ia-cli")
        .env_remove("IA_ACCESS_KEY_ID")
        .env_remove("IA_SECRET_ACCESS_KEY")
        .args(&args)
        .output()
        .unwrap()
}

fn assert_clean_error(output: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        stderr.contains("invalid user_agent_suffix \"app\\n1.0\""),
        "{stderr}"
    );
}

#[test]
fn control_character_in_flag_is_an_error() {
    let output = run(
        "[general]\nhost = 127.0.0.1:1\nsecure = false\n",
        &["--user-agent-suffix", "app\n1.0"],
    );
    assert_clean_error(&output);
}

#[test]
fn control_character_in_config_file_is_an_error() {
    let output = run(
        "[general]\nhost = 127.0.0.1:1\nsecure = false\nuser_agent_suffix = app\u{b}1.0\n",
        &[],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(stderr.contains("invalid user_agent_suffix"), "{stderr}");
}
