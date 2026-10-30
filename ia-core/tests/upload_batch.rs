//! Integration tests for batch upload from spreadsheet records (`upload_batch`).

use ia_core::upload::{self, UploadOpts, UploadStatus};
use ia_core::{IaClient, IaConfig};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Create an `IaClient` pointed at a wiremock server with S3 credentials.
fn test_client(server: &MockServer) -> IaClient {
    let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
    let mut config = IaConfig::default();
    config.s3_access = Some("test-access".into());
    config.s3_secret = Some("test-secret".into());
    config.general.host = host_port;
    config.general.secure = false;
    IaClient::from_config_no_retry(config).unwrap()
}

/// Helper to build a spreadsheet record for upload.
fn record(
    id: &str,
    file: &str,
    mediatype: &str,
    collection: &str,
) -> (String, HashMap<String, String>) {
    let mut fields = HashMap::new();
    fields.insert("file".into(), file.into());
    fields.insert("mediatype".into(), mediatype.into());
    fields.insert("collection".into(), collection.into());
    (id.into(), fields)
}

#[tokio::test]
async fn batch_upload_single_item() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/item-1/file1.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("file1.txt");
    fs::write(&f1, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "item-1",
        f1.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn batch_upload_multiple_items() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("file1.txt");
    let f2 = dir.path().join("file2.txt");
    fs::write(&f1, "content1").unwrap();
    fs::write(&f2, "content2").unwrap();

    let client = test_client(&server);
    let records = vec![
        record("item-1", f1.to_str().unwrap(), "texts", "test_collection"),
        record("item-2", f2.to_str().unwrap(), "texts", "test_collection"),
    ];
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o
    };

    let results = upload::upload_batch(&client, records, &opts, 2, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn batch_upload_grouped_by_identifier() {
    let server = MockServer::start().await;

    // Same item, two files
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    fs::write(&f1, "aaa").unwrap();
    fs::write(&f2, "bbb").unwrap();

    let client = test_client(&server);
    let records = vec![
        record("item-1", f1.to_str().unwrap(), "texts", "test_collection"),
        record("item-1", f2.to_str().unwrap(), "texts", "test_collection"),
    ];
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2); // 2 files in 1 item
}

#[tokio::test]
async fn batch_upload_empty_records() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = UploadOpts::default();

    let err = upload::upload_batch(&client, vec![], &opts, 1, None, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ia_core::IaError::EmptyUpload));
}

#[tokio::test]
async fn batch_upload_missing_file_field() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let mut fields = HashMap::new();
    fields.insert("mediatype".into(), "texts".into());
    // No "file" field
    let records = vec![("item-1".into(), fields)];

    let err = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap_err();
    // Should error about missing file
    assert!(matches!(err, ia_core::IaError::Config(_)));
}

#[tokio::test]
async fn batch_upload_nonexistent_file() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let records = vec![record(
        "item-1",
        "/tmp/ia-test-nonexistent-file-abc123.txt",
        "texts",
        "test_collection",
    )];

    let err = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap_err();
    // File validation should catch nonexistent files
    assert!(matches!(err, ia_core::IaError::Config(_)));
}

#[tokio::test]
async fn batch_upload_invalid_identifier() {
    let server = MockServer::start().await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "!!",
        f.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o
    };

    let err = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ia_core::IaError::Config(_)));
    assert!(err.to_string().contains("batch validation failed"));
}

#[tokio::test]
async fn batch_upload_merges_metadata_with_opts() {
    let server = MockServer::start().await;

    // Expect the PUT with metadata from both opts and spreadsheet
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);

    let mut fields = HashMap::new();
    fields.insert("file".into(), f.to_str().unwrap().into());
    fields.insert("mediatype".into(), "texts".into());
    fields.insert("collection".into(), "test_collection".into());
    fields.insert("title".into(), "From Spreadsheet".into());
    let records = vec![("item-1".into(), fields)];

    // Base opts have some extra header metadata
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::Uploaded));
}

#[tokio::test]
async fn batch_upload_dry_run() {
    let server = MockServer::start().await;
    // No mocks — dry run should not make HTTP requests

    let dir = TempDir::new().unwrap();
    let f = dir.path().join("file.txt");
    fs::write(&f, "content").unwrap();

    let client = test_client(&server);
    let records = vec![record(
        "item-1",
        f.to_str().unwrap(),
        "texts",
        "test_collection",
    )];
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.dry_run = true;
        o.no_collection_check = true;
        o
    };

    let results = upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, UploadStatus::DryRun));
}

/// Upload `records` against a fresh wiremock server that accepts every PUT,
/// and return the one PUT's path and its `x-archive-meta*` headers.
async fn single_put(
    records: Vec<(String, HashMap<String, String>)>,
    cli_metadata: &[(&str, &str)],
) -> (String, Vec<(String, String)>) {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.metadata = pairs(cli_metadata);
        o
    };
    upload::upload_batch(&client, records, &opts, 1, None, None, None)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let put = requests
        .iter()
        .find(|r| r.method.as_str() == "PUT")
        .expect("one PUT");
    let headers = put
        .headers
        .iter()
        .filter(|(name, _)| name.as_str().starts_with("x-archive-meta"))
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap().to_string(),
            )
        })
        .collect();
    (put.url.path().to_string(), headers)
}

/// The `x-archive-meta*` headers for one field, sorted by header name (that
/// is, by the two-digit index), as `(header, value)`.
fn field_headers(headers: &[(String, String)], field: &str) -> Vec<(String, String)> {
    let suffix = format!("-{field}");
    let mut out: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| name.ends_with(&suffix))
        .cloned()
        .collect();
    out.sort();
    out
}

/// A record for `item-1` with a real file `a.txt`, `mediatype` and
/// `collection`, plus `extra` columns as a reader produces them.
fn row_with(dir: &TempDir, extra: &[(&str, &str)]) -> (String, HashMap<String, String>) {
    let f = dir.path().join("a.txt");
    fs::write(&f, "content").unwrap();
    let (id, mut fields) = record("item-1", f.to_str().unwrap(), "texts", "test_collection");
    for (k, v) in extra {
        fields.insert((*k).into(), (*v).into());
    }
    (id, fields)
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[tokio::test]
async fn indexed_columns_reach_ia_as_one_header_each_in_index_order() {
    // #97: subject[0]/subject[1] used to collapse into one header.
    let dir = TempDir::new().unwrap();
    let rec = row_with(&dir, &[("subject[0]", "alpha"), ("subject[1]", "beta")]);
    let (_, headers) = single_put(vec![rec], &[]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[
            ("x-archive-meta00-subject", "alpha"),
            ("x-archive-meta01-subject", "beta"),
        ])
    );
}

#[tokio::test]
async fn a_row_field_replaces_the_command_line_values_for_that_field() {
    // As in the Python client: the row's values for a field replace the -m
    // values for it; -m fields the row does not name are kept.
    let dir = TempDir::new().unwrap();
    let rec = row_with(&dir, &[("subject[0]", "alpha"), ("subject[1]", "beta")]);
    let (_, headers) = single_put(
        vec![rec],
        &[
            ("subject", "from-cli"),
            ("subject", "also-from-cli"),
            ("creator", "cli-creator"),
        ],
    )
    .await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[
            ("x-archive-meta00-subject", "alpha"),
            ("x-archive-meta01-subject", "beta"),
        ])
    );
    assert_eq!(
        field_headers(&headers, "creator"),
        pairs(&[("x-archive-meta00-creator", "cli-creator")])
    );
}

#[tokio::test]
async fn indexed_columns_are_ordered_by_numeric_index_not_column_order() {
    // Column order [2], [10], [0]; string order would put [10] before [2].
    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "content").unwrap();
    let csv = dir.path().join("batch.csv");
    fs::write(
        &csv,
        format!(
            "identifier,file,mediatype,collection,subject[2],subject[10],subject[0]\n\
             item-1,{},texts,test_collection,two,ten,zero\n",
            f.display()
        ),
    )
    .unwrap();
    let records = ia_core::spreadsheet::read_spreadsheet(&csv).unwrap();
    let (_, headers) = single_put(records, &[]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[
            ("x-archive-meta00-subject", "zero"),
            ("x-archive-meta01-subject", "two"),
            ("x-archive-meta02-subject", "ten"),
        ])
    );
}

#[tokio::test]
async fn an_indexed_column_overwrites_the_bare_value_at_its_position() {
    // The #65 rule (`apply_set`): the bare value is the list, and
    // `subject[0]` overwrites position 0.
    let dir = TempDir::new().unwrap();
    let rec = row_with(&dir, &[("subject[0]", "indexed"), ("subject", "bare")]);
    let (_, headers) = single_put(vec![rec], &[]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[("x-archive-meta00-subject", "indexed")])
    );
}

#[tokio::test]
async fn an_indexed_column_past_the_bare_value_is_appended() {
    // The #65 rule: `subject:a` then `subject[1]:b` gives [a, b].
    let dir = TempDir::new().unwrap();
    let rec = row_with(&dir, &[("subject[1]", "b"), ("subject", "a")]);
    let (_, headers) = single_put(vec![rec], &[]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[
            ("x-archive-meta00-subject", "a"),
            ("x-archive-meta01-subject", "b"),
        ])
    );
}

#[tokio::test]
async fn two_columns_for_one_position_keep_the_later_in_column_name_order() {
    // The #65 rule keeps the later value for a position named twice. The
    // readers do not keep column order, so "later" is column-name order:
    // "subject[00]" < "subject[0]".
    let dir = TempDir::new().unwrap();
    let rec = row_with(
        &dir,
        &[("subject[0]", "zero"), ("subject[00]", "zero-zero")],
    );
    let (_, headers) = single_put(vec![rec], &[]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[("x-archive-meta00-subject", "zero")])
    );
}

#[tokio::test]
async fn a_remove_tag_cell_is_sent_as_a_literal_value() {
    // An upload creates the item; a cell is a value, as `-m subject:REMOVE_TAG`
    // is, and replaces the -m values for its field like any other.
    let dir = TempDir::new().unwrap();
    let rec = row_with(
        &dir,
        &[("subject", "REMOVE_TAG"), ("creator[0]", "REMOVE_TAG")],
    );
    let (_, headers) = single_put(vec![rec], &[("subject", "cli")]).await;
    assert_eq!(
        field_headers(&headers, "subject"),
        pairs(&[("x-archive-meta00-subject", "REMOVE_TAG")])
    );
    assert_eq!(
        field_headers(&headers, "creator"),
        pairs(&[("x-archive-meta00-creator", "REMOVE_TAG")])
    );
}

/// A template row for `file` in `item-1` with `remote_name` filled in.
fn template_row(file: &std::path::Path, remote_name: &str) -> upload::TemplateRow {
    upload::TemplateRow {
        identifier: "item-1".into(),
        file: file.to_string_lossy().into_owned(),
        remote_name: remote_name.into(),
        mediatype: "texts".into(),
        collection: "test_collection".into(),
        title: String::new(),
        creator: String::new(),
        date: String::new(),
        description: String::new(),
        subject: String::new(),
        language: String::new(),
    }
}

/// Write `rows` as the template CSV and read them back as a batch would.
fn through_template_csv(
    dir: &TempDir,
    rows: &[upload::TemplateRow],
) -> Vec<(String, HashMap<String, String>)> {
    let csv = dir.path().join("batch.csv");
    let mut out = fs::File::create(&csv).unwrap();
    upload::write_template_csv(rows, &mut out).unwrap();
    drop(out);
    ia_core::spreadsheet::read_spreadsheet(&csv).unwrap()
}

#[tokio::test]
async fn the_template_remote_name_column_names_the_file_and_is_not_metadata() {
    // #98: the readers lowercase REMOTE_NAME, so it was sent as metadata
    // `remote_name` and the file kept its local name.
    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "content").unwrap();
    let records = through_template_csv(&dir, &[template_row(&f, "renamed.txt")]);

    let (path, headers) = single_put(records, &[]).await;
    assert_eq!(path, "/item-1/renamed.txt");
    assert!(
        headers.iter().all(|(name, _)| !name.contains("remote")),
        "remote name sent as metadata: {headers:?}"
    );
}

#[tokio::test]
async fn an_empty_remote_name_keeps_the_local_file_name() {
    let dir = TempDir::new().unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "content").unwrap();
    let records = through_template_csv(&dir, &[template_row(&f, "")]);

    let (path, _) = single_put(records, &[]).await;
    assert_eq!(path, "/item-1/a.txt");
}

/// Write files `names` in `dir`, one template row each for `item-1` with
/// the paired remote name, and read them back as a batch would.
fn rows_for(dir: &TempDir, rows: &[(&str, &str)]) -> Vec<(String, HashMap<String, String>)> {
    let template: Vec<upload::TemplateRow> = rows
        .iter()
        .map(|(name, remote)| {
            let f = dir.path().join(name);
            fs::write(&f, "content").unwrap();
            template_row(&f, remote)
        })
        .collect();
    through_template_csv(dir, &template)
}

/// Run a batch against a server that accepts every PUT; return the
/// result and the paths of every request it received (method and path),
/// sorted.
async fn batch_puts(
    records: Vec<(String, HashMap<String, String>)>,
    remote_dir: Option<&str>,
) -> (
    ia_core::error::Result<Vec<upload::UploadResult>>,
    Vec<String>,
) {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let client = test_client(&server);
    let opts = {
        let mut o = UploadOpts::default();
        o.verify = false;
        o.no_collection_check = true;
        o.remote_dir = remote_dir.map(String::from);
        o
    };
    let result = upload::upload_batch(&client, records, &opts, 1, None, None, None).await;
    let mut paths: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    paths.sort();
    (result, paths)
}

/// The PUT paths among `requests` from [`batch_puts`].
fn put_paths(requests: &[String]) -> Vec<&str> {
    requests
        .iter()
        .filter_map(|r| r.strip_prefix("PUT "))
        .collect()
}

#[tokio::test]
async fn each_row_of_an_item_is_stored_under_its_own_remote_name() {
    // #98: a remote name per row, and a row without one keeps its name.
    let dir = TempDir::new().unwrap();
    let records = rows_for(
        &dir,
        &[("a.txt", "one.txt"), ("b.txt", "two.txt"), ("c.txt", "")],
    );
    let (result, paths) = batch_puts(records, None).await;
    result.unwrap();
    assert_eq!(
        put_paths(&paths),
        vec!["/item-1/c.txt", "/item-1/one.txt", "/item-1/two.txt"]
    );
}

#[tokio::test]
async fn a_remote_name_is_placed_under_the_remote_dir() {
    let dir = TempDir::new().unwrap();
    let records = rows_for(&dir, &[("a.txt", "renamed.txt"), ("b.txt", "")]);
    let (result, paths) = batch_puts(records, Some("scans")).await;
    result.unwrap();
    assert_eq!(
        put_paths(&paths),
        vec!["/item-1/scans/b.txt", "/item-1/scans/renamed.txt"]
    );
}

#[tokio::test]
async fn a_remote_name_outside_the_item_is_refused_before_any_request() {
    // compute_keys applies validate_key to a remote name as to every key.
    let dir = TempDir::new().unwrap();
    let records = rows_for(&dir, &[("a.txt", "../x.txt"), ("b.txt", "")]);
    let (result, paths) = batch_puts(records, None).await;
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("../x.txt"), "{msg}");
    assert!(paths.is_empty(), "{paths:?}");
}

#[tokio::test]
async fn two_rows_given_one_remote_name_are_refused_before_any_request() {
    // The second PUT would replace the first.
    let dir = TempDir::new().unwrap();
    let records = rows_for(&dir, &[("a.txt", "same.txt"), ("b.txt", "same.txt")]);
    let (result, paths) = batch_puts(records, None).await;
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("same.txt"), "{msg}");
    assert!(paths.is_empty(), "{paths:?}");
}

#[tokio::test]
async fn a_remote_name_equal_to_another_rows_file_name_is_refused() {
    let dir = TempDir::new().unwrap();
    let records = rows_for(&dir, &[("a.txt", "b.txt"), ("b.txt", "")]);
    let (result, paths) = batch_puts(records, None).await;
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("b.txt"), "{msg}");
    assert!(paths.is_empty(), "{paths:?}");
}
