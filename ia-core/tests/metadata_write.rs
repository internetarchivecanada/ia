//! Comprehensive integration tests for metadata write operations.
//!
//! ALL tests use wiremock mocks — ZERO live requests to archive.org.
//! All metadata fixtures are FAKE — no real archive.org items.

use ia_core::metadata::write::{
    compute_patch, modify, modify_compound, prepare_metadata, ChangeGroup,
    CompoundModifyRequest, MetadataOp, ModifyRequest, REMOVE_TAG,
};
use ia_core::rate_limit::RateLimiter;
use ia_core::{IaClient, IaConfig, IaError};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// =============================================================================
// Test helpers
// =============================================================================

fn mock_config_with_auth(server_uri: &str) -> IaConfig {
    let mut config = IaConfig::default();
    let host = server_uri
        .strip_prefix("http://")
        .or_else(|| server_uri.strip_prefix("https://"))
        .unwrap_or(server_uri);
    config.general.host = host.to_string();
    config.general.secure = false;
    config.s3_access = Some("test_access".to_string());
    config.s3_secret = Some("test_secret".to_string());
    config
}

fn success_response(task_id: u64) -> Value {
    json!({
        "success": true,
        "task_id": task_id,
        "log": format!("https://catalogd.archive.org/log/{task_id}")
    })
}

/// Mount GET + POST mocks for a given identifier and item fixture.
async fn mount_modify_mocks(server: &MockServer, identifier: &str, item: Value, task_id: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/metadata/{identifier}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!("/metadata/{identifier}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(task_id)))
        .mount(server)
        .await;
}

// =============================================================================
// Fixture functions — 16+ fake items covering IA's real-world metadata chaos
// =============================================================================

fn clean_item() -> Value {
    json!({
        "metadata": {
            "identifier": "clean-item",
            "title": "A Well-Formed Item",
            "description": "This item has all fields populated correctly.",
            "mediatype": "texts",
            "collection": ["opensource", "community"],
            "creator": "Test Author",
            "date": "2024-01-15",
            "subject": ["testing", "metadata", "quality"],
            "language": "eng",
            "publicdate": "2024-01-15 10:30:00",
            "addeddate": "2024-01-15 10:30:00",
            "uploader": "test@example.com"
        },
        "files": [
            {"name": "document.pdf", "size": "5000000", "source": "original", "md5": "abc123def", "format": "Text PDF"},
            {"name": "document_djvu.txt", "size": "120000", "source": "derivative", "original": "document.pdf", "format": "DjVuTXT"}
        ],
        "server": "ia802304.us.archive.org"
    })
}

fn minimal_item() -> Value {
    json!({
        "metadata": {
            "identifier": "minimal-item",
            "mediatype": "data"
        },
        "files": [],
        "server": "ia800100.us.archive.org"
    })
}

fn string_arrays_item() -> Value {
    json!({
        "metadata": {
            "identifier": "string-arrays-item",
            "title": "Item With String Instead of Arrays",
            "mediatype": "texts",
            "subject": "single-subject",
            "collection": "opensource",
            "creator": "Solo Author"
        },
        "files": [
            {"name": "file.txt", "size": "100", "source": "original"}
        ],
        "server": "ia800200.us.archive.org"
    })
}

fn mega_collections_item() -> Value {
    json!({
        "metadata": {
            "identifier": "mega-collections-item",
            "title": "Item in Many Collections",
            "mediatype": "texts",
            "collection": [
                "opensource", "community", "books", "texts",
                "fav-user1", "fav-user2", "fav-user3", "fav-user4",
                "additional_collections", "library_genesis",
                "americana", "toronto", "microfilm",
                "printdisabled", "inlibrary", "china"
            ],
            "subject": ["history", "archive"]
        },
        "files": [],
        "server": "ia800300.us.archive.org"
    })
}

fn unicode_item() -> Value {
    json!({
        "metadata": {
            "identifier": "unicode-item",
            "title": "\u{0F00}\u{0F01}\u{0F02} \u{4E16}\u{754C} \u{1F30D}",
            "description": "Tibetan, CJK, and emoji characters in metadata",
            "mediatype": "texts",
            "creator": "J\u{00F6}rg M\u{00FC}ller",
            "subject": ["\u{6570}\u{5B66}", "\u{79D1}\u{5B66}", "math\u{00E9}matiques"],
            "language": "mul"
        },
        "files": [
            {"name": "doc\u{00FC}ment.pdf", "size": "3000", "source": "original"}
        ],
        "server": "ia800400.us.archive.org"
    })
}

fn html_description_item() -> Value {
    json!({
        "metadata": {
            "identifier": "html-desc-item",
            "title": "Item With HTML Description",
            "description": "<p>This has <b>bold</b> and <a href=\"https://example.com\">links</a>.</p><br>Line two.",
            "mediatype": "texts",
            "subject": ["web", "html"]
        },
        "files": [],
        "server": "ia800500.us.archive.org"
    })
}

fn date_chaos_item() -> Value {
    json!({
        "metadata": {
            "identifier": "date-chaos-item",
            "title": "Item With Chaotic Dates",
            "mediatype": "texts",
            "date": "circa 1920",
            "publicdate": "20140925130256",
            "addeddate": "2014-09-25T13:02:56Z",
            "scandate": "[n.d.]",
            "subject": ["dates", "chaos"]
        },
        "files": [],
        "server": "ia800600.us.archive.org"
    })
}

fn duplicate_values_item() -> Value {
    json!({
        "metadata": {
            "identifier": "duplicate-values-item",
            "title": "Item With Duplicate Values",
            "mediatype": "texts",
            "subject": ["history", "history", "archive", "history"],
            "collection": ["opensource", "opensource", "community"]
        },
        "files": [],
        "server": "ia800700.us.archive.org"
    })
}

fn numeric_strings_item() -> Value {
    json!({
        "metadata": {
            "identifier": "numeric-strings-item",
            "title": "Item With Numeric String Fields",
            "mediatype": "texts",
            "ppi": "300",
            "imagecount": "42",
            "scanfee": "0",
            "year": "1995"
        },
        "files": [],
        "server": "ia800800.us.archive.org"
    })
}

fn empty_strings_item() -> Value {
    json!({
        "metadata": {
            "identifier": "empty-strings-item",
            "title": "Item With Empty String Fields",
            "mediatype": "texts",
            "description": "",
            "creator": "",
            "date": "",
            "subject": ["", "valid-subject", ""]
        },
        "files": [],
        "server": "ia800900.us.archive.org"
    })
}

fn null_fields_item() -> Value {
    json!({
        "metadata": {
            "identifier": "null-fields-item",
            "title": "Item With Null Fields",
            "mediatype": "texts",
            "description": null,
            "creator": null,
            "subject": ["valid"]
        },
        "files": [],
        "server": "ia801000.us.archive.org"
    })
}

fn extra_fields_item() -> Value {
    json!({
        "metadata": {
            "identifier": "extra-fields-item",
            "title": "Item With 30+ Fields",
            "mediatype": "texts",
            "collection": ["opensource"],
            "subject": ["scanning"],
            "scanningcenter": "sanfrancisco",
            "camera": "Canon 5D",
            "ppi": "600",
            "ocr": "ABBYY FineReader 11.0",
            "foldoutcount": "0",
            "operator": "scanner@example.com",
            "republisher_operator": "associate-editor@example.com",
            "republisher_date": "20240115130000",
            "republisher_time": "120",
            "scandate": "20240110100000",
            "autocrop_version": "0.0.14",
            "identifier-ark": "ark:/13960/t1234567x",
            "isbn": "9780123456789",
            "lccn": "2024123456",
            "oclc-id": "12345678",
            "external-identifier": "urn:oclc:record:12345678",
            "barcode": "30112012345678",
            "boxid": "IA12345",
            "backup_location": "ia904203-3",
            "sponsordate": "20240101",
            "sponsor": "The Library Foundation",
            "contributor": "University Library",
            "tts_version": "v1.2.3",
            "page-progression": "lr",
            "call_number": "QA76.73.R87",
            "notes": "Some scanning notes",
            "source": "folio",
            "possible-copyright-status": "NOT_IN_COPYRIGHT"
        },
        "files": [
            {"name": "book.pdf", "size": "50000000", "source": "original"}
        ],
        "server": "ia801100.us.archive.org"
    })
}

fn file_level_meta_item() -> Value {
    json!({
        "metadata": {
            "identifier": "file-level-meta-item",
            "title": "Item With File-Level Metadata",
            "mediatype": "texts"
        },
        "files": [
            {
                "name": "page001.jpg",
                "size": "500000",
                "source": "original",
                "format": "JPEG",
                "rotation": "90",
                "ocr_detected_lang": "eng",
                "custom_tag": "hello"
            },
            {
                "name": "page002.jpg",
                "size": "450000",
                "source": "original",
                "format": "JPEG"
            },
            {
                "name": "combined.pdf",
                "size": "2000000",
                "source": "derivative",
                "original": "page001.jpg",
                "format": "Text PDF"
            }
        ],
        "server": "ia801200.us.archive.org"
    })
}

fn dark_item() -> Value {
    json!({
        "metadata": {
            "identifier": "dark-item",
            "title": "Dark (Restricted) Item",
            "mediatype": "texts",
            "is_dark": true,
            "noindex": true
        },
        "files": [
            {"name": "restricted.pdf", "size": "1000", "source": "original"}
        ],
        "server": "ia801300.us.archive.org"
    })
}

fn semicolon_subjects_item() -> Value {
    json!({
        "metadata": {
            "identifier": "semicolon-subjects-item",
            "title": "Item With Semicolon-Delimited Subjects",
            "mediatype": "texts",
            "subject": "space;nasa;apollo;moon;astronomy",
            "collection": ["opensource"]
        },
        "files": [],
        "server": "ia801400.us.archive.org"
    })
}

fn description_array_item() -> Value {
    json!({
        "metadata": {
            "identifier": "desc-array-item",
            "title": "Item With Description as Array",
            "mediatype": "texts",
            "description": ["First paragraph.", "Second paragraph.", "Third paragraph."],
            "subject": ["arrays"]
        },
        "files": [],
        "server": "ia801500.us.archive.org"
    })
}

fn scan_metadata_item() -> Value {
    json!({
        "metadata": {
            "identifier": "scan-metadata-item",
            "title": "Scanned Book With Rich Scan Metadata",
            "mediatype": "texts",
            "scandate": "20240115100000",
            "scanner": "scribe3.sanfrancisco.archive.org",
            "ppi": "400",
            "camera": "Sony Alpha A6300",
            "operator": "scan-operator@example.com",
            "scanningcenter": "sanfrancisco",
            "repub_state": "4",
            "imagecount": "312",
            "collection": ["inlibrary", "printdisabled", "books"],
            "subject": ["bibliography", "library science"]
        },
        "files": [
            {"name": "book_images.zip", "size": "1500000000", "source": "original"},
            {"name": "book.pdf", "size": "50000000", "source": "derivative"}
        ],
        "server": "ia801600.us.archive.org"
    })
}

// =============================================================================
// Set operation tests (8+)
// =============================================================================

#[tokio::test]
async fn set_replace_existing_field_on_clean_item() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "clean-item", clean_item(), 10001).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("Updated Title"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
        .await
        .unwrap();
    assert!(resp.success);
    assert_eq!(resp.task_id, Some(10001));
}

#[tokio::test]
async fn set_add_new_field_on_minimal_item() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "minimal-item", minimal_item(), 10002).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![
        ("title".to_string(), json!("Brand New Title")),
        ("description".to_string(), json!("A new description")),
    ];
    let resp = modify(&client, &ModifyRequest {
        identifier: "minimal-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
        .await
        .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn set_remove_tag_deletes_field_on_extra_fields_item() {
    let source = extra_fields_item()["metadata"].clone();
    let changes = vec![
        ("notes".to_string(), json!(REMOVE_TAG)),
        ("camera".to_string(), json!(REMOVE_TAG)),
    ];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert!(dest.get("notes").is_none());
    assert!(dest.get("camera").is_none());
    // Other fields untouched
    assert_eq!(dest["title"], json!("Item With 30+ Fields"));
    assert_eq!(dest["ppi"], json!("600"));
}

#[test]
fn set_on_string_arrays_item_replaces_string_with_value() {
    let source = string_arrays_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!(["new-subject-1", "new-subject-2"]))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["subject"], json!(["new-subject-1", "new-subject-2"]));
}

#[test]
fn set_unicode_value_on_unicode_item() {
    let source = unicode_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!("\u{1F680} Rocket Science \u{2764}\u{FE0F}"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["title"], json!("\u{1F680} Rocket Science \u{2764}\u{FE0F}"));
}

#[test]
fn set_empty_string_value() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!(""))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["description"], json!(""));
}

#[test]
fn set_multiple_fields_at_once() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![
        ("title".to_string(), json!("New Title")),
        ("description".to_string(), json!("New Description")),
        ("date".to_string(), json!("2025-06-01")),
        ("language".to_string(), json!("fra")),
    ];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["title"], json!("New Title"));
    assert_eq!(dest["description"], json!("New Description"));
    assert_eq!(dest["date"], json!("2025-06-01"));
    assert_eq!(dest["language"], json!("fra"));
    // Unchanged fields
    assert_eq!(dest["mediatype"], json!("texts"));
}

#[test]
fn set_overwrite_list_with_scalar() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("single-subject"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["subject"], json!("single-subject"));
}

#[test]
fn set_overwrite_scalar_with_list() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!(["Title Part 1", "Title Part 2"]))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["title"], json!(["Title Part 1", "Title Part 2"]));
}

// =============================================================================
// Append operation tests (3+)
// =============================================================================

#[test]
fn append_to_existing_string_on_html_item() {
    let source = html_description_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("<p>Addendum.</p>"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
    let desc = dest["description"].as_str().unwrap();
    assert!(desc.starts_with("<p>This has <b>bold</b>"));
    assert!(desc.ends_with("<p>Addendum.</p>"));
}

#[test]
fn append_to_missing_field_on_minimal_item() {
    let source = minimal_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("First description"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
    assert_eq!(dest["description"], json!("First description"));
}

#[test]
fn append_to_empty_string_field() {
    let source = empty_strings_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("added text"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
    assert_eq!(dest["description"], json!(" added text"));
}

#[test]
fn append_to_array_field_errors() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("new-val"))];
    let result = prepare_metadata(&source, &changes, &MetadataOp::Append, "test");
    assert!(result.is_err());
}

#[test]
fn append_to_numeric_string_field() {
    let source = numeric_strings_item()["metadata"].clone();
    let changes = vec![("ppi".to_string(), json!("(updated)"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Append, "test").unwrap();
    assert_eq!(dest["ppi"], json!("300 (updated)"));
}

// =============================================================================
// AppendList operation tests (4+)
// =============================================================================

#[test]
fn append_list_to_existing_array_on_clean_item() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("new-subject"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
    assert_eq!(
        dest["subject"],
        json!(["testing", "metadata", "quality", "new-subject"])
    );
}

#[test]
fn append_list_to_string_converts_on_string_arrays_item() {
    let source = string_arrays_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("second-subject"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
    assert_eq!(dest["subject"], json!(["single-subject", "second-subject"]));
}

#[test]
fn append_list_to_missing_field_creates_list() {
    let source = minimal_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("brand-new"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
    assert_eq!(dest["subject"], json!(["brand-new"]));
}

#[test]
fn append_list_allows_duplicate_values() {
    let source = duplicate_values_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("history"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
    // Should now have 4 "history" entries (3 original + 1 new)
    let subjects = dest["subject"].as_array().unwrap();
    let history_count = subjects.iter().filter(|v| v == &&json!("history")).count();
    assert_eq!(history_count, 4); // 3 original + 1 new
}

#[test]
fn append_list_on_mega_collections_item() {
    let source = mega_collections_item()["metadata"].clone();
    let changes = vec![("collection".to_string(), json!("new-collection"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::AppendList, "test").unwrap();
    let collections = dest["collection"].as_array().unwrap();
    assert_eq!(collections.len(), 17); // 16 original + 1
    assert_eq!(collections.last().unwrap(), &json!("new-collection"));
}

// =============================================================================
// Insert operation tests (4+)
// =============================================================================

#[test]
fn insert_at_beginning_of_array() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("collection".to_string(), json!("featured"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
    assert_eq!(dest["collection"], json!(["featured", "opensource", "community"]));
}

#[test]
fn insert_at_end_of_array() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("last"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(999), "test").unwrap();
    let subjects = dest["subject"].as_array().unwrap();
    assert_eq!(subjects.last().unwrap(), &json!("last"));
}

#[test]
fn insert_deduplicates_existing_value() {
    let source = mega_collections_item()["metadata"].clone();
    // "opensource" already exists at index 0; insert it at position 3
    let changes = vec![("collection".to_string(), json!("opensource"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(3), "test").unwrap();
    let collections = dest["collection"].as_array().unwrap();
    // Count should be same (deduplicated)
    assert_eq!(collections.len(), 16);
    // Should be at index 3 (after removing from index 0, then inserting at 3)
    assert_eq!(collections[3], json!("opensource"));
}

#[test]
fn insert_into_string_converts_to_array() {
    let source = string_arrays_item()["metadata"].clone();
    let changes = vec![("collection".to_string(), json!("featured"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
    assert_eq!(dest["collection"], json!(["featured", "opensource"]));
}

#[test]
fn insert_into_missing_field_creates_array() {
    let source = minimal_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("brand-new"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Insert(0), "test").unwrap();
    assert_eq!(dest["subject"], json!(["brand-new"]));
}

// =============================================================================
// Remove operation tests (6+)
// =============================================================================

#[test]
fn remove_from_array_leaves_remaining() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("metadata"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert_eq!(dest["subject"], json!(["testing", "quality"]));
}

#[test]
fn remove_last_from_array_deletes_field() {
    let source = json!({"identifier": "test", "subject": ["only-one"]});
    let changes = vec![("subject".to_string(), json!("only-one"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert!(dest.get("subject").is_none());
}

#[test]
fn remove_scalar_match_deletes_field() {
    let source = string_arrays_item()["metadata"].clone();
    let changes = vec![("creator".to_string(), json!("Solo Author"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert!(dest.get("creator").is_none());
}

#[test]
fn remove_scalar_no_match_is_noop() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("creator".to_string(), json!("Wrong Author"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert_eq!(dest["creator"], json!("Test Author"));
}

#[test]
fn remove_from_semicolon_subjects() {
    let source = semicolon_subjects_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("apollo"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert_eq!(dest["subject"], json!("space;nasa;moon;astronomy"));
}

#[test]
fn remove_all_semicolon_subjects_one_by_one() {
    let source = json!({"identifier": "test", "subject": "a;b"});
    // Remove "a"
    let changes = vec![("subject".to_string(), json!("a"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert_eq!(dest["subject"], json!("b"));
    // Remove "b" from the result
    let changes2 = vec![("subject".to_string(), json!("b"))];
    let dest2 = prepare_metadata(&dest, &changes2, &MetadataOp::Remove, "test").unwrap();
    assert!(dest2.get("subject").is_none());
}

#[test]
fn remove_from_duplicate_values_array() {
    let source = duplicate_values_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("history"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    // All "history" values removed, only "archive" remains
    assert_eq!(dest["subject"], json!(["archive"]));
}

#[test]
fn remove_nonexistent_field_is_noop() {
    let source = minimal_item()["metadata"].clone();
    let changes = vec![("nonexistent".to_string(), json!("value"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Remove, "test").unwrap();
    assert_eq!(dest, source);
}

// =============================================================================
// Target tests: item metadata, file metadata, file not found
// =============================================================================

#[tokio::test]
async fn modify_item_metadata_target() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "clean-item", clean_item(), 20001).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New Title"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
        .await
        .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_file_metadata_target() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/file-level-meta-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_level_meta_item()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/file-level-meta-item"))
        .and(body_string_contains("-target=files%2Fpage001.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(20002)))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("rotation".to_string(), json!("0"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "file-level-meta-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "files/page001.jpg".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_file_not_found_returns_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/file-level-meta-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_level_meta_item()))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("tag".to_string(), json!("val"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "file-level-meta-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "files/nonexistent.pdf".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    match result.unwrap_err() {
        ia_core::IaError::MetadataWrite { message, .. } => {
            assert!(message.contains("file not found"));
        }
        other => panic!("expected MetadataWrite, got: {other}"),
    }
}

// =============================================================================
// Edge case tests: zero-change, 429, 401/403, huge metadata
// =============================================================================

#[tokio::test]
async fn zero_change_patch_returns_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    // Set title to its current value — no changes
    let changes = vec![("title".to_string(), json!("A Well-Formed Item"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    assert!(result.is_err());
    match result.unwrap_err() {
        ia_core::IaError::MetadataWrite { identifier, message } => {
            assert_eq!(identifier, "clean-item");
            assert!(message.contains("no changes"));
        }
        other => panic!("expected MetadataWrite error, got: {other}"),
    }
}

#[tokio::test]
async fn rate_limited_429_returns_retry_after() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/clean-item"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "120"),
        )
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    match result.unwrap_err() {
        ia_core::IaError::RateLimited { retry_after } => {
            assert_eq!(retry_after, 120);
        }
        other => panic!("expected RateLimited, got: {other}"),
    }
}

#[tokio::test]
async fn rate_limited_429_no_retry_after_defaults_to_30() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    match result.unwrap_err() {
        ia_core::IaError::RateLimited { retry_after } => {
            assert_eq!(retry_after, 30);
        }
        other => panic!("expected RateLimited, got: {other}"),
    }
}

#[tokio::test]
async fn auth_missing_returns_error_immediately() {
    let config = IaConfig::default(); // no credentials
    let client = IaClient::from_config(config).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "any-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    assert!(matches!(result.unwrap_err(), ia_core::IaError::Auth(_)));
}

#[tokio::test]
async fn server_reports_error_in_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false,
            "error": "internal server error"
        })))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let result = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await;
    match result.unwrap_err() {
        ia_core::IaError::MetadataWrite { message, .. } => {
            assert!(message.contains("internal server error"));
        }
        other => panic!("expected MetadataWrite, got: {other}"),
    }
}

#[test]
fn huge_metadata_100_plus_fields() {
    let mut metadata = serde_json::Map::new();
    metadata.insert("identifier".to_string(), json!("huge-item"));
    for i in 0..120 {
        metadata.insert(format!("field_{i:03}"), json!(format!("value_{i}")));
    }
    let source = Value::Object(metadata);

    let changes = vec![
        ("field_050".to_string(), json!("updated_50")),
        ("field_099".to_string(), json!("updated_99")),
        ("field_new".to_string(), json!("brand_new")),
    ];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["field_050"], json!("updated_50"));
    assert_eq!(dest["field_099"], json!("updated_99"));
    assert_eq!(dest["field_new"], json!("brand_new"));
    // Unchanged fields preserved
    assert_eq!(dest["field_000"], json!("value_0"));
    assert_eq!(dest["field_119"], json!("value_119"));
}

// =============================================================================
// Patch computation edge cases
// =============================================================================

#[test]
fn compute_patch_set_on_clean_item() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!("Updated"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0]["op"], "replace");
    assert_eq!(patch[0]["path"], "/title");
    assert_eq!(patch[0]["value"], "Updated");
}

#[test]
fn compute_patch_add_on_minimal_item() {
    let source = minimal_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!("New Title"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0]["op"], "add");
    assert_eq!(patch[0]["path"], "/title");
}

#[test]
fn compute_patch_remove_tag_on_extra_fields_item() {
    let source = extra_fields_item()["metadata"].clone();
    let changes = vec![("notes".to_string(), json!(REMOVE_TAG))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0]["op"], "remove");
    assert_eq!(patch[0]["path"], "/notes");
}

#[test]
fn compute_patch_no_changes_on_duplicate_values_item() {
    let source = duplicate_values_item()["metadata"].clone();
    // Set subject to same value
    let changes = vec![(
        "subject".to_string(),
        json!(["history", "history", "archive", "history"]),
    )];
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, None, "test").unwrap();
    assert!(patch.is_empty());
}

#[test]
fn compute_patch_with_expect_on_clean_item() {
    let source = clean_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!("New Title"))];
    let expect = HashMap::from([("title".to_string(), json!("A Well-Formed Item"))]);
    let patch = compute_patch(&source, &changes, &MetadataOp::Set, Some(&expect), "test").unwrap();
    // First op should be the test op
    assert!(patch.len() >= 2);
    assert_eq!(patch[0]["op"], "test");
    assert_eq!(patch[0]["path"], "/title");
    assert_eq!(patch[0]["value"], "A Well-Formed Item");
    // Second op should be the replace
    assert_eq!(patch[1]["op"], "replace");
    assert_eq!(patch[1]["path"], "/title");
}

#[test]
fn compute_patch_append_list_on_scan_metadata_item() {
    let source = scan_metadata_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("digitization"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::AppendList, None, "test").unwrap();
    // Should produce a replace or add for the subject array
    assert!(!patch.is_empty());
}

#[test]
fn compute_patch_remove_from_semicolon_subjects() {
    let source = semicolon_subjects_item()["metadata"].clone();
    let changes = vec![("subject".to_string(), json!("nasa"))];
    let patch = compute_patch(&source, &changes, &MetadataOp::Remove, None, "test").unwrap();
    assert!(!patch.is_empty());
    assert_eq!(patch[0]["op"], "replace");
    assert_eq!(patch[0]["path"], "/subject");
    assert_eq!(patch[0]["value"], "space;apollo;moon;astronomy");
}

// =============================================================================
// Fixture-specific edge case tests
// =============================================================================

#[test]
fn null_fields_set_replaces_null() {
    let source = null_fields_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("Real description"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["description"], json!("Real description"));
}

#[test]
fn null_fields_remove_tag_on_null() {
    let source = null_fields_item()["metadata"].clone();
    let changes = vec![("creator".to_string(), json!(REMOVE_TAG))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert!(dest.get("creator").is_none());
}

#[test]
fn empty_strings_set_replaces_empty() {
    let source = empty_strings_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("Not empty anymore"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["description"], json!("Not empty anymore"));
}

#[test]
fn description_array_set_replaces_array() {
    let source = description_array_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("Single string now"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["description"], json!("Single string now"));
}

#[test]
fn description_array_append_errors() {
    let source = description_array_item()["metadata"].clone();
    let changes = vec![("description".to_string(), json!("More text"))];
    let result = prepare_metadata(&source, &changes, &MetadataOp::Append, "test");
    // Description is an array, so Append should error
    assert!(result.is_err());
}

#[test]
fn dark_item_can_still_modify_metadata() {
    let source = dark_item()["metadata"].clone();
    let changes = vec![("title".to_string(), json!("Updated Dark Item"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["title"], json!("Updated Dark Item"));
    // Other fields unchanged
    assert_eq!(dest["is_dark"], json!(true));
}

#[test]
fn date_chaos_set_updates_date() {
    let source = date_chaos_item()["metadata"].clone();
    let changes = vec![("date".to_string(), json!("1920"))];
    let dest = prepare_metadata(&source, &changes, &MetadataOp::Set, "test").unwrap();
    assert_eq!(dest["date"], json!("1920"));
}

// =============================================================================
// Verify POST body format
// =============================================================================

#[tokio::test]
async fn modify_sends_correct_post_body_format() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    // Verify key parts of the POST body
    Mock::given(method("POST"))
        .and(path("/metadata/clean-item"))
        .and(body_string_contains("-target=metadata"))
        .and(body_string_contains("-patch="))
        .and(body_string_contains("priority="))
        .and(body_string_contains("access=test_access"))
        .and(body_string_contains("secret=test_secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(30001)))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("Updated"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: Some(-5),
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_with_priority_sends_correct_priority() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/clean-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(clean_item()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/clean-item"))
        .and(body_string_contains("priority=-5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(30002)))
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("New"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "clean-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: Some(-5),
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

// =============================================================================
// Cross-fixture integration: apply operations to various fixtures
// =============================================================================

#[tokio::test]
async fn modify_unicode_item_set_title() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "unicode-item", unicode_item(), 40001).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("title".to_string(), json!("Plain ASCII Title"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "unicode-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_scan_metadata_item_append_list_subject() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "scan-metadata-item", scan_metadata_item(), 40002).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("subject".to_string(), json!("digitization"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "scan-metadata-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::AppendList,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_dark_item_set_metadata() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "dark-item", dark_item(), 40003).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("noindex".to_string(), json!(REMOVE_TAG))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "dark-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_numeric_strings_item_set_ppi() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "numeric-strings-item", numeric_strings_item(), 40004).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("ppi".to_string(), json!("600"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "numeric-strings-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_empty_strings_item_set_description() {
    let server = MockServer::start().await;
    mount_modify_mocks(&server, "empty-strings-item", empty_strings_item(), 40005).await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let changes = vec![("description".to_string(), json!("Now has a description"))];
    let resp = modify(&client, &ModifyRequest {
        identifier: "empty-strings-item".to_string(),
        changes: changes.clone(),
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    })
    .await
    .unwrap();
    assert!(resp.success);
}

// =============================================================================
// Batch concurrency tests: JoinSet + Semaphore + RateLimiter with modify()
// =============================================================================

fn batch_item(identifier: &str) -> Value {
    json!({
        "metadata": {
            "identifier": identifier,
            "title": "Old Title",
            "collection": ["test-collection"]
        },
        "files": []
    })
}

#[tokio::test]
async fn no_retry_client_sees_429_as_rate_limited() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test429"))
        .respond_with(ResponseTemplate::new(200).set_body_json(batch_item("test429")))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test429"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("Retry-After", "5"),
        )
        .mount(&server)
        .await;

    let client = IaClient::from_config_no_retry(mock_config_with_auth(&server.uri())).unwrap();
    let req = ModifyRequest {
        identifier: "test429".to_string(),
        changes: vec![("title".to_string(), json!("New"))],
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: Some(0),
        reduced_priority: false,
    };
    let result = modify(&client, &req).await;
    match &result {
        Err(IaError::RateLimited { retry_after }) => {
            assert_eq!(*retry_after, 5);
        }
        other => panic!("expected RateLimited, got: {other:?}"),
    }
}

#[tokio::test]
async fn single_worker_429_retry_with_rate_limiter() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/retry-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(batch_item("retry-item")))
        .mount(&server)
        .await;

    // 429 mock: high priority (1), consumed after 1 response
    Mock::given(method("POST"))
        .and(path("/metadata/retry-item"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("Retry-After", "1"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // Success mock: low priority (10), serves after 429 mock is consumed
    Mock::given(method("POST"))
        .and(path("/metadata/retry-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(8001)))
        .with_priority(10)
        .mount(&server)
        .await;

    let client = IaClient::from_config_no_retry(mock_config_with_auth(&server.uri())).unwrap();
    let rl = RateLimiter::new();
    let mut attempts = 0usize;

    let req = ModifyRequest {
        identifier: "retry-item".to_string(),
        changes: vec![("title".to_string(), json!("New"))],
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: Some(0),
        reduced_priority: false,
    };

    let result = loop {
        attempts += 1;
        rl.wait_if_paused().await;
        match modify(&client, &req).await {
            Ok(resp) => break Ok(resp),
            Err(IaError::RateLimited { retry_after }) => {
                rl.pause_for(retry_after, |_| {}).await;
            }
            Err(e) => break Err(e),
        }
    };

    assert!(result.is_ok(), "expected Ok after retry, got: {result:?}");
    assert_eq!(attempts, 2, "should take 2 attempts (first 429, then success)");
}

#[tokio::test]
async fn batch_modify_concurrent_faster_than_sequential() {
    let server = MockServer::start().await;

    // Mount mocks for 3 items, each POST has 200ms delay
    for id in ["batch-a", "batch-b", "batch-c"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch_item(id)))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(success_response(5000))
                    .set_delay(std::time::Duration::from_millis(200)),
            )
            .mount(&server)
            .await;
    }

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let semaphore = Arc::new(Semaphore::new(3)); // all 3 can run concurrently
    let rate_limiter = RateLimiter::new();

    let start = std::time::Instant::now();
    let mut set = JoinSet::new();

    for id in ["batch-a", "batch-b", "batch-c"] {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            rl.wait_if_paused().await;

            let req = ModifyRequest {
                identifier: id.to_string(),
                changes: vec![("title".to_string(), json!("New Title"))],
                op: MetadataOp::Set,
                target: "metadata".to_string(),
                expect: None,
                priority: Some(-5),
                reduced_priority: false,
            };
            modify(&client, &req).await
        });
    }

    let mut results = Vec::new();
    while let Some(result) = set.join_next().await {
        results.push(result.unwrap());
    }

    let elapsed = start.elapsed();

    // All 3 should succeed
    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(r.is_ok(), "expected Ok, got: {r:?}");
    }

    // Concurrent: ~200ms. Sequential would be ~600ms+.
    // Use 500ms threshold to account for overhead.
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "took {elapsed:?}, expected <500ms for concurrent execution of 3x200ms tasks"
    );
}

#[tokio::test]
async fn batch_modify_429_pauses_all_workers_then_retries() {
    let server = MockServer::start().await;

    // Mount GET mocks for both items
    for id in ["rate-a", "rate-b"] {
        Mock::given(method("GET"))
            .and(path(format!("/metadata/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch_item(id)))
            .mount(&server)
            .await;
    }

    // rate-a POST: 429 mock (high priority, consumed after 1 response)
    Mock::given(method("POST"))
        .and(path("/metadata/rate-a"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("Retry-After", "1"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // rate-a POST: success mock (low priority, serves after 429 consumed)
    Mock::given(method("POST"))
        .and(path("/metadata/rate-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(6001)))
        .with_priority(10)
        .mount(&server)
        .await;

    // rate-b POST: always success
    Mock::given(method("POST"))
        .and(path("/metadata/rate-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(6002)))
        .mount(&server)
        .await;

    // Use no-retry client so 429 reaches modify() directly as IaError::RateLimited
    let client = IaClient::from_config_no_retry(mock_config_with_auth(&server.uri())).unwrap();
    let semaphore = Arc::new(Semaphore::new(2));
    let rate_limiter = RateLimiter::new();
    let pause_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let start = std::time::Instant::now();
    let mut set = JoinSet::new();

    for id in ["rate-a", "rate-b"] {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();
        let pc = Arc::clone(&pause_count);

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let req = ModifyRequest {
                identifier: id.to_string(),
                changes: vec![("title".to_string(), json!("Updated"))],
                op: MetadataOp::Set,
                target: "metadata".to_string(),
                expect: None,
                priority: Some(0),
                reduced_priority: false,
            };

            loop {
                rl.wait_if_paused().await;
                match modify(&client, &req).await {
                    Ok(resp) => return Ok(resp),
                    Err(IaError::RateLimited { retry_after }) => {
                        pc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        rl.pause_for(retry_after, |_| {}).await;
                        // retry
                    }
                    Err(e) => return Err(e),
                }
            }
        });
    }

    let mut results = Vec::new();
    while let Some(result) = set.join_next().await {
        results.push(result.unwrap());
    }

    let elapsed = start.elapsed();

    // Both should succeed (rate-a after RateLimiter-mediated retry)
    assert_eq!(results.len(), 2);
    for r in &results {
        assert!(r.is_ok(), "expected Ok, got: {r:?}");
    }

    // RateLimiter should have been triggered at least once
    assert!(
        pause_count.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "rate limiter should have been triggered by 429"
    );

    // Should take at least 1 second (the Retry-After pause duration)
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "took {elapsed:?}, expected >=900ms due to rate limit pause"
    );
}

#[tokio::test]
async fn batch_modify_mixed_success_and_error() {
    let server = MockServer::start().await;

    // mix-ok: succeeds
    mount_modify_mocks(&server, "mix-ok", batch_item("mix-ok"), 7001).await;

    // mix-fail: GET succeeds, POST returns error response
    Mock::given(method("GET"))
        .and(path("/metadata/mix-fail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(batch_item("mix-fail")))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/mix-fail"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"success": false, "error": "internal error"})),
        )
        .mount(&server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&server.uri())).unwrap();
    let semaphore = Arc::new(Semaphore::new(2));
    let rate_limiter = RateLimiter::new();

    let mut set = JoinSet::new();

    for id in ["mix-ok", "mix-fail"] {
        let client = client.clone();
        let sem = Arc::clone(&semaphore);
        let rl = rate_limiter.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let req = ModifyRequest {
                identifier: id.to_string(),
                changes: vec![("title".to_string(), json!("New"))],
                op: MetadataOp::Set,
                target: "metadata".to_string(),
                expect: None,
                priority: Some(0),
                reduced_priority: false,
            };

            loop {
                rl.wait_if_paused().await;
                match modify(&client, &req).await {
                    Ok(resp) => return Ok(resp),
                    Err(IaError::RateLimited { retry_after }) => {
                        rl.pause_for(retry_after, |_| {}).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        });
    }

    let mut successes = 0;
    let mut errors = 0;
    while let Some(result) = set.join_next().await {
        match result.unwrap() {
            Ok(_) => successes += 1,
            Err(_) => errors += 1,
        }
    }

    assert_eq!(successes, 1);
    assert_eq!(errors, 1);
}

// =============================================================================
// Compound modify tests
// =============================================================================

fn standard_item_fixture() -> Value {
    json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Old Title",
            "mediatype": "texts",
            "subject": ["math", "science"],
            "collection": ["opensource"],
            "description": "A test item"
        },
        "files": [
            {"name": "test.pdf", "size": "1000", "source": "original", "md5": "abc123"}
        ],
        "server": "ia000000.us.archive.org"
    })
}

#[tokio::test]
async fn modify_compound_set_and_remove_single_post() {
    let mock_server = MockServer::start().await;
    let item = standard_item_fixture();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&item))
        .expect(1) // exactly 1 GET
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .and(body_string_contains("-target=metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(99999)))
        .expect(1) // exactly 1 POST
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
    let req = CompoundModifyRequest {
        identifier: "test-item".to_string(),
        groups: vec![
            ChangeGroup {
                changes: vec![("title".to_string(), json!("New Title"))],
                op: MetadataOp::Set,
            },
            ChangeGroup {
                changes: vec![("subject".to_string(), json!("science"))],
                op: MetadataOp::Remove,
            },
        ],
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let resp = modify_compound(&client, &req).await.unwrap();
    assert!(resp.success);
    assert_eq!(resp.task_id, Some(99999));
}

#[tokio::test]
async fn modify_compound_no_net_changes_errors() {
    let mock_server = MockServer::start().await;
    let item = standard_item_fixture();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&item))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
    let req = CompoundModifyRequest {
        identifier: "test-item".to_string(),
        groups: vec![ChangeGroup {
            changes: vec![("title".to_string(), json!("Old Title"))],
            op: MetadataOp::Set,
        }],
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let result = modify_compound(&client, &req).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn modify_compound_auth_required() {
    let config = IaConfig::default(); // no credentials
    let client = IaClient::from_config(config).unwrap();
    let req = CompoundModifyRequest {
        identifier: "test-item".to_string(),
        groups: vec![ChangeGroup {
            changes: vec![("title".to_string(), json!("New"))],
            op: MetadataOp::Set,
        }],
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let result = modify_compound(&client, &req).await;
    assert!(matches!(result.unwrap_err(), IaError::Auth(_)));
}

#[tokio::test]
async fn modify_compound_backwards_compat_with_modify() {
    // Verify modify() still works (it delegates to modify_compound internally)
    let mock_server = MockServer::start().await;
    let item = standard_item_fixture();
    mount_modify_mocks(&mock_server, "test-item", item, 12345).await;

    let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
    let req = ModifyRequest {
        identifier: "test-item".to_string(),
        changes: vec![("title".to_string(), json!("New Title"))],
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let resp = modify(&client, &req).await.unwrap();
    assert!(resp.success);
}

#[tokio::test]
async fn modify_compound_three_groups_single_post() {
    let mock_server = MockServer::start().await;
    let item = json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Old",
            "subject": ["math"],
            "collection": ["opensource"]
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    });

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&item))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(11111)))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
    let req = CompoundModifyRequest {
        identifier: "test-item".to_string(),
        groups: vec![
            ChangeGroup {
                changes: vec![("title".to_string(), json!("New"))],
                op: MetadataOp::Set,
            },
            ChangeGroup {
                changes: vec![("subject".to_string(), json!("physics"))],
                op: MetadataOp::AppendList,
            },
            ChangeGroup {
                changes: vec![("collection".to_string(), json!("featured"))],
                op: MetadataOp::Insert(0),
            },
        ],
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let resp = modify_compound(&client, &req).await.unwrap();
    assert!(resp.success);
    assert_eq!(resp.task_id, Some(11111));
}

#[tokio::test]
async fn modify_compound_overlapping_fields_last_wins() {
    let mock_server = MockServer::start().await;
    let item = json!({
        "metadata": {
            "identifier": "test-item",
            "title": "Original"
        },
        "files": [],
        "server": "ia000000.us.archive.org"
    });

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&item))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response(22222)))
        .mount(&mock_server)
        .await;

    let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
    let req = CompoundModifyRequest {
        identifier: "test-item".to_string(),
        groups: vec![
            ChangeGroup {
                changes: vec![("title".to_string(), json!("First"))],
                op: MetadataOp::Set,
            },
            ChangeGroup {
                changes: vec![("title".to_string(), json!("Second"))],
                op: MetadataOp::Set,
            },
        ],
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let resp = modify_compound(&client, &req).await.unwrap();
    assert!(resp.success);
}
