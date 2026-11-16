//! Every `--spreadsheet` command refuses what the shared reader refuses
//! (#154): a JSONL value that is not a string, an array of strings or
//! `null`, and a repeated header. The host is a closed local port, so no
//! request can leave the machine even if a command got past the reader.

use std::fs;
use std::path::Path;
use tempfile::{NamedTempFile, TempDir};

fn closed_host_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "[general]\nhost = 127.0.0.1:1\n").unwrap();
    f
}

/// Run `ia <args>` with test credentials and return (success, stderr).
fn run(args: &[&str]) -> (bool, String) {
    let cfg = closed_host_config();
    let out = assert_cmd::cargo_bin_cmd!("ia-cli")
        .arg("--config-file")
        .arg(cfg.path())
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The arguments that make each command read `sheet`.
fn commands(sheet: &Path) -> Vec<Vec<String>> {
    let s = sheet.to_str().unwrap().to_string();
    vec![
        vec![
            "upload".into(),
            "--spreadsheet".into(),
            s.clone(),
            "--dry-run".into(),
            "--no-collection-check".into(),
        ],
        vec!["metadata".into(), "--spreadsheet".into(), s.clone()],
        vec![
            "tasks".into(),
            "submit".into(),
            "--spreadsheet".into(),
            s.clone(),
        ],
        vec!["verify".into(), "--spreadsheet".into(), s],
    ]
}

fn assert_every_command_refuses(sheet: &Path, want: &str) {
    for args in commands(sheet) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (ok, stderr) = run(&args);
        assert!(!ok, "ia {args:?} should fail");
        assert!(
            stderr.contains(want),
            "ia {args:?}: expected {want:?} in stderr: {stderr}"
        );
    }
}

#[test]
fn jsonl_number_value_is_refused_by_every_command() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("a.txt");
    fs::write(&data, "x").unwrap();
    let sheet = dir.path().join("sheet.jsonl");
    fs::write(
        &sheet,
        format!(
            "{{\"identifier\":\"item-a\",\"file\":{},\"cmd\":\"derive.php\",\"mediatype\":\"texts\",\"collection\":\"test_collection\",\"year\":1999}}\n",
            serde_json::to_string(data.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    assert_every_command_refuses(
        &sheet,
        "line 1: year must be a string, an array of strings, or null (got a number)",
    );
}

#[test]
fn csv_repeated_header_is_refused_by_every_command() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("a.txt");
    fs::write(&data, "x").unwrap();
    let sheet = dir.path().join("sheet.csv");
    fs::write(
        &sheet,
        format!(
            "identifier,file,cmd,mediatype,collection,title,Title\nitem-a,{},derive.php,texts,test_collection,x,y\n",
            data.display()
        ),
    )
    .unwrap();
    assert_every_command_refuses(&sheet, "header 'title' is given more than once");
}

#[test]
fn upload_dry_run_lists_each_value_of_a_jsonl_array() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("a.txt");
    fs::write(&data, "x").unwrap();
    let sheet = dir.path().join("sheet.jsonl");
    fs::write(
        &sheet,
        format!(
            "{{\"Identifier\":\"item-a\",\"file\":{},\"mediatype\":\"texts\",\"collection\":\"test_collection\",\"subject\":[\"alpha\",\"beta\"],\"creator\":null}}\n",
            serde_json::to_string(data.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let s = sheet.to_str().unwrap();
    let (ok, stderr) = run(&[
        "upload",
        "--spreadsheet",
        s,
        "-m",
        "creator:from-cli",
        "--dry-run",
        "--no-collection-check",
    ]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("item-a"), "{stderr}");
    assert!(
        stderr.contains("alpha") && stderr.contains("beta"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("[\"alpha\""),
        "array text sent as one value: {stderr}"
    );
    assert!(
        stderr.contains("from-cli"),
        "null replaced the -m value: {stderr}"
    );
    assert!(!stderr.contains("null"), "{stderr}");
}
