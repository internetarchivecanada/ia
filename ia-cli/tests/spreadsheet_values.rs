//! Every `--spreadsheet` command reads a file the same way (#154): JSONL
//! numbers and booleans are sent as written, and a repeated header is
//! refused. The host is a closed local port, so no
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

/// A JSONL file whose values are numbers and booleans.
fn sheet_with_numbers(dir: &TempDir) -> std::path::PathBuf {
    let data = dir.path().join("a.txt");
    fs::write(&data, "x").unwrap();
    let sheet = dir.path().join("sheet.jsonl");
    fs::write(
        &sheet,
        format!(
            "{{\"identifier\":\"item-a\",\"file\":{},\"cmd\":\"derive.php\",\"mediatype\":\"texts\",\"collection\":\"test_collection\",\"year\":1999,\"price\":1.50,\"count\":1e3,\"public\":true}}\n",
            serde_json::to_string(data.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    sheet
}

#[test]
fn jsonl_numbers_and_booleans_pass_the_reader_of_every_command() {
    let dir = TempDir::new().unwrap();
    let sheet = sheet_with_numbers(&dir);
    for args in commands(&sheet) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (_, stderr) = run(&args);
        assert!(
            !stderr.contains("spreadsheet has problems"),
            "ia {args:?} refused the file: {stderr}"
        );
    }
}

#[test]
fn upload_dry_run_sends_jsonl_numbers_and_booleans_as_written() {
    let dir = TempDir::new().unwrap();
    let sheet = sheet_with_numbers(&dir);
    let (ok, stderr) = run(&[
        "upload",
        "--spreadsheet",
        sheet.to_str().unwrap(),
        "--dry-run",
        "--no-collection-check",
    ]);
    assert!(ok, "{stderr}");
    for want in ["1999", "1.50", "1e3", "true"] {
        assert!(stderr.contains(want), "missing {want:?}: {stderr}");
    }
    assert!(!stderr.contains("1000"), "1e3 was re-rendered: {stderr}");
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

/// A resumed CSV export reads its own file back to merge the earlier rows.
/// When that file cannot be read (here, a header given twice in the same
/// case, as a hand edit can leave it), the export must stop and leave the
/// file as it was, not start fresh and overwrite the earlier rows.
#[tokio::test]
async fn resumed_export_stops_when_its_file_cannot_be_read() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata/item-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "item-b", "title": "B"},
            "files": []
        })))
        // The file is read before any request, so none is made.
        .expect(0)
        .mount(&server)
        .await;
    let host = server.uri().replace("http://", "");

    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.csv");
    let earlier = "identifier,title,title\nitem-a,one,two\n";
    fs::write(&out, earlier).unwrap();
    let joblog = dir.path().join("export.jsonl");
    fs::write(
        &joblog,
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"op\":\"export\",\"item\":\"item-a\",\"file\":\"\",\"status\":\"ok\"}\n",
    )
    .unwrap();
    let ids = dir.path().join("ids.txt");
    fs::write(&ids, "item-a\nitem-b\n").unwrap();

    let (ok, stderr) = run(&[
        "--insecure",
        "-H",
        &host,
        "--joblog",
        joblog.to_str().unwrap(),
        "metadata",
        "export",
        ids.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        !ok,
        "a resumed export with an unreadable file must fail: {stderr}"
    );
    assert!(
        stderr.contains("out.csv"),
        "error should name the file: {stderr}"
    );
    assert!(
        stderr.contains("header 'title' is given more than once"),
        "error should give the reason: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        earlier,
        "file was changed"
    );
}

/// Two resumes in a row keep every row and every key in the case IA
/// returned: the export reads its own file back without lowercasing the
/// headers, so `Title` stays `Title` and `Title` with `title` stays two
/// columns.
#[tokio::test]
async fn export_resumed_twice_keeps_rows_and_key_case() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    for (id, metadata) in [
        (
            "item-a",
            serde_json::json!({"identifier": "item-a", "Title": "A"}),
        ),
        (
            "item-b",
            serde_json::json!({"identifier": "item-b", "Title": "B upper", "title": "b lower"}),
        ),
        (
            "item-c",
            serde_json::json!({"identifier": "item-c", "Title": "C"}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"metadata": metadata, "files": []})),
            )
            .mount(&server)
            .await;
    }
    let host = server.uri().replace("http://", "");

    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.csv");
    let joblog = dir.path().join("export.jsonl");
    // Each run adds one item; the joblog skips the earlier ones, so runs 2
    // and 3 are resumes that read the file back.
    for ids in ["item-a\n", "item-a\nitem-b\n", "item-a\nitem-b\nitem-c\n"] {
        let list = dir.path().join("ids.txt");
        fs::write(&list, ids).unwrap();
        let (ok, stderr) = run(&[
            "--insecure",
            "-H",
            &host,
            "--joblog",
            joblog.to_str().unwrap(),
            "metadata",
            "export",
            list.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ]);
        assert!(ok, "export of {ids:?} failed: {stderr}");
    }

    // The values hold no delimiter or quote, so each line splits plainly.
    let content = fs::read_to_string(&out).unwrap();
    let mut lines = content.lines().map(|l| l.split(',').collect::<Vec<_>>());
    assert_eq!(
        lines.next().unwrap(),
        vec!["identifier", "Title", "title"],
        "headers"
    );
    let rows: Vec<Vec<&str>> = lines.collect();
    assert_eq!(
        rows,
        vec![
            vec!["item-a", "A", ""],
            vec!["item-b", "B upper", "b lower"],
            vec!["item-c", "C", ""],
        ],
        "rows"
    );
}

/// A JSONL `"priority": -5` reaches the task submission as -5.
#[tokio::test]
async fn tasks_submit_sends_a_jsonl_number_priority() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .and(body_partial_json(
            serde_json::json!({"identifier": "item-a", "priority": -5}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "value": {"task_id": 1234, "log": "https://catalogd.archive.org/log/1234"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let host = server.uri().replace("http://", "");

    let dir = TempDir::new().unwrap();
    let sheet = dir.path().join("jobs.jsonl");
    fs::write(
        &sheet,
        "{\"identifier\":\"item-a\",\"cmd\":\"derive.php\",\"priority\":-5}\n",
    )
    .unwrap();
    let (ok, stderr) = run(&[
        "--insecure",
        "-H",
        &host,
        "tasks",
        "submit",
        "--spreadsheet",
        sheet.to_str().unwrap(),
    ]);
    assert!(ok, "{stderr}");
}
