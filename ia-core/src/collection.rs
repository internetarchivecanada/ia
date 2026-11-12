use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{IaError, Result};
use crate::identifier::validate_identifier;
use crate::spreadsheet::SpreadsheetRecord;
use crate::upload::batch::row_metadata;
use crate::upload::headers::encode_metadata_headers;
use crate::upload::s3_error::describe_s3_failure;
use crate::upload::{build_s3_item_url, upload_file, UploadOpts, UploadStatus};
use crate::IaClient;

/// Result returned after successfully creating a collection on Internet Archive.
#[derive(Debug, Clone)]
pub struct CreateCollectionResult {
    /// The identifier of the newly created collection item.
    pub identifier: String,
    /// HTTP status code returned by the server.
    pub status: u16,
    /// URL of the collection on archive.org.
    pub url: String,
}

/// Create a new collection item on Internet Archive.
///
/// # Parameters
///
/// - `identifier`: The item identifier for the new collection (3-100 chars, alphanumeric + `._-@`).
/// - `metadata`: Additional metadata key-value pairs. Any `mediatype` entries are silently dropped
///   (collections always use `mediatype=collection`).
/// - `image`: Optional path to a cover image for the collection. Must have a file extension.
/// - `dry_run`: If `true`, validate everything but send no HTTP requests.
///
/// Derive is always disabled for collections (`x-archive-queue-derive: 0`).
///
/// # Errors
///
/// Returns `IaError::InvalidIdentifier` if the identifier is invalid.
/// Returns `IaError::UploadFailed` if the image file is missing or has no extension.
/// Returns `IaError::Auth` if S3 credentials are missing (when not in dry-run mode).
/// Returns `IaError::Http` on S3 API errors.
pub async fn create_collection(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    image: Option<&Path>,
    dry_run: bool,
) -> Result<CreateCollectionResult> {
    validate_identifier(identifier)?;

    // Validate image path eagerly, before any network requests.
    if let Some(img) = image {
        check_image(img).map_err(|message| IaError::UploadFailed {
            identifier: identifier.to_string(),
            key: if img.exists() {
                "(image)".into()
            } else {
                img.display().to_string()
            },
            message,
            status: None,
        })?;
    }

    let full_metadata = build_collection_metadata(metadata);
    let url = details_url(client, identifier);

    if let Some(img) = image {
        create_with_image(client, identifier, img, &full_metadata, dry_run, url).await
    } else {
        create_without_image(client, identifier, &full_metadata, dry_run, url).await
    }
}

/// One collection to create, from a spreadsheet row merged with the
/// command-line defaults by [`collection_rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectionRow {
    /// The new collection's identifier.
    pub identifier: String,
    /// Metadata to send, as `create_collection` takes it.
    pub metadata: Vec<(String, String)>,
    /// Cover image, from the row's `image` column or the default image.
    pub image: Option<PathBuf>,
}

/// The spreadsheet column that names a row's cover image. It is never sent
/// as metadata.
pub(crate) const IMAGE_COLUMN: &str = "image";

/// Turn spreadsheet records into collections to create, checking every row
/// before any request is made.
///
/// Each record's fields become the row's metadata, with the `image` column
/// taken out as the row's cover image (a path relative to the current
/// directory). Indexed columns (`subject[0]`, `subject[1]`, ...) give
/// several values for one field, as in `ia upload --spreadsheet`.
///
/// `defaults` and `default_image` are the values given once for every row
/// (on the command line): a field a row fills in replaces every default
/// value for that field, for that row only; fields the row does not fill in
/// keep the defaults. This is the rule a batch upload applies between `-m`
/// and a row. Rows keep their order.
///
/// # Errors
///
/// Returns `IaError::Config` when `records` is empty, and
/// `IaError::Config` listing every problem when any row has an invalid
/// identifier, an identifier already used on another row, no parent
/// `collection`, an image that does not exist or has no extension, or a
/// metadata field that cannot be sent as an S3 header.
pub fn collection_rows(
    records: Vec<SpreadsheetRecord>,
    defaults: &[(String, String)],
    default_image: Option<&Path>,
) -> Result<Vec<CollectionRow>> {
    rows_with_problems(records, Vec::new(), defaults, default_image)
}

/// [`collection_rows`], with `problems` the reader already found reported
/// first, in the same error.
fn rows_with_problems(
    records: Vec<SpreadsheetRecord>,
    problems: Vec<String>,
    defaults: &[(String, String)],
    default_image: Option<&Path>,
) -> Result<Vec<CollectionRow>> {
    if records.is_empty() && problems.is_empty() {
        return Err(IaError::Config("spreadsheet contains no records".into()));
    }

    let mut errors: Vec<String> = problems;
    // A bad default image is one problem, reported once, not once per row.
    let default_ok = match default_image.map(check_image) {
        Some(Err(message)) => {
            errors.push(format!("--image: {message}"));
            false
        }
        _ => true,
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows = Vec::with_capacity(records.len());

    for (identifier, mut fields) in records {
        let own_image = fields
            .remove(IMAGE_COLUMN)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let checked = own_image.is_some() || default_ok;
        let image = own_image.or_else(|| default_image.map(Path::to_path_buf));

        let own = row_metadata(fields);
        let named: HashSet<&str> = own.iter().map(|(k, _)| k.as_str()).collect();
        let mut metadata: Vec<(String, String)> = defaults
            .iter()
            .filter(|(k, _)| !named.contains(k.as_str()))
            .cloned()
            .collect();
        metadata.extend(own);

        if let Err(e) = validate_identifier(&identifier) {
            errors.push(e.to_string());
        }
        if !seen.insert(identifier.clone()) {
            errors.push(format!("{identifier}: on more than one row"));
        }
        if !metadata
            .iter()
            .any(|(k, v)| k == "collection" && !v.is_empty())
        {
            errors.push(format!(
                "{identifier}: no parent collection (give a collection column or --collection)"
            ));
        }
        if let (Some(img), true) = (&image, checked) {
            if let Err(message) = check_image(img) {
                errors.push(format!("{identifier}: {message}"));
            }
        }
        if let Err(e) = encode_metadata_headers(&build_collection_metadata(&metadata)) {
            errors.push(format!("{identifier}: {e}"));
        }

        rows.push(CollectionRow {
            identifier,
            metadata,
            image,
        });
    }

    if errors.is_empty() {
        Ok(rows)
    } else {
        Err(IaError::Config(format!(
            "batch validation failed:\n  {}",
            errors.join("\n  ")
        )))
    }
}

/// Read a spreadsheet file and turn it into collections to create with
/// [`collection_rows`], checking every row before any request is made.
///
/// CSV, TSV, XLSX and ODS files are read with
/// [`crate::spreadsheet::read_spreadsheet`]. JSONL (`.jsonl`, `.ndjson`)
/// is read here, by value type rather than as text: a `null` is no value,
/// so a default applies; an array of strings gives one value per element,
/// as indexed columns do; a string is the value. Any other value (a number,
/// a boolean, an object, an array holding anything but strings) is refused,
/// since its text would be sent as metadata. Keys are case-insensitive,
/// `identifier` included, and a line with no string identifier is refused
/// rather than skipped. Every such problem is reported with the other
/// validation problems.
///
/// # Errors
///
/// As [`collection_rows`], plus the JSONL problems above, and the read
/// errors of the file.
pub fn read_collection_spreadsheet(
    path: &Path,
    defaults: &[(String, String)],
    default_image: Option<&Path>,
) -> Result<Vec<CollectionRow>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "jsonl" || ext == "ndjson" {
        let (records, problems) = read_jsonl_rows(path)?;
        rows_with_problems(records, problems, defaults, default_image)
    } else {
        collection_rows(
            crate::spreadsheet::read_spreadsheet(path)?,
            defaults,
            default_image,
        )
    }
}

/// Read a JSONL file into records by value type, with the problems found;
/// see [`read_collection_spreadsheet`].
fn read_jsonl_rows(path: &Path) -> Result<(Vec<SpreadsheetRecord>, Vec<String>)> {
    let content = std::fs::read_to_string(path)?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let mut records = Vec::new();
    let mut problems = Vec::new();

    for (n, line) in content.lines().enumerate() {
        let line_no = n + 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let object = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Object(object)) => object,
            Ok(_) => {
                problems.push(format!("line {line_no}: not a JSON object"));
                continue;
            }
            Err(e) => {
                problems.push(format!("line {line_no}: not valid JSON: {e}"));
                continue;
            }
        };

        let id_keys: Vec<&String> = object
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("identifier"))
            .collect();
        let identifier = match (id_keys.as_slice(), id_keys.first().map(|k| &object[*k])) {
            ([_], Some(serde_json::Value::String(id))) if !id.trim().is_empty() => {
                id.trim().to_string()
            }
            ([_], Some(serde_json::Value::String(_) | serde_json::Value::Null)) | ([], _) => {
                problems.push(format!("line {line_no}: no identifier"));
                continue;
            }
            ([_], Some(other)) => {
                problems.push(format!(
                    "line {line_no}: identifier must be a string (got {})",
                    json_kind(other)
                ));
                continue;
            }
            _ => {
                problems.push(format!(
                    "line {line_no}: identifier is given more than once"
                ));
                continue;
            }
        };

        let mut fields: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut keys_seen: HashSet<String> = HashSet::new();
        for (key, value) in &object {
            let key = key.to_lowercase();
            if key == "identifier" {
                continue;
            }
            if !keys_seen.insert(key.clone()) {
                problems.push(format!(
                    "{identifier}: {key} is given more than once (keys are case-insensitive)"
                ));
                continue;
            }
            let mut put = |k: String, v: String, problems: &mut Vec<String>| {
                if fields.insert(k.clone(), v).is_some() {
                    problems.push(format!("{identifier}: {k} is given more than once"));
                }
            };
            match value {
                serde_json::Value::Null => {}
                serde_json::Value::String(v) => put(key, v.clone(), &mut problems),
                serde_json::Value::Array(items)
                    if key != IMAGE_COLUMN
                        && !key.contains('[')
                        && items.iter().all(serde_json::Value::is_string) =>
                {
                    for (i, item) in items.iter().enumerate() {
                        if let Some(v) = item.as_str() {
                            put(format!("{key}[{i}]"), v.to_string(), &mut problems);
                        }
                    }
                }
                other => problems.push(format!(
                    "{identifier}: {key} must be {} (got {})",
                    if key == IMAGE_COLUMN || key.contains('[') {
                        "a string or null"
                    } else {
                        "a string, an array of strings, or null"
                    },
                    json_kind(other)
                )),
            }
        }
        records.push((identifier, fields));
    }

    Ok((records, problems))
}

/// A JSON value's kind, for an error message.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array that holds more than strings",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Check that a cover image exists and has an extension, from which its
/// content type is inferred. Returns the problem as text.
fn check_image(img: &Path) -> std::result::Result<(), String> {
    if !img.exists() {
        return Err(format!("image file not found: {}", img.display()));
    }
    if !img.is_file() {
        return Err(format!("image is not a file: {}", img.display()));
    }
    if img.extension().is_none() {
        return Err(format!(
            "image file has no extension, cannot infer content type: {}",
            img.display()
        ));
    }
    Ok(())
}

/// Build the full metadata list for a collection item.
///
/// Always starts with `("mediatype", "collection")` and appends user-supplied
/// metadata, silently dropping any user-provided `mediatype` keys.
fn build_collection_metadata(user_metadata: &[(String, String)]) -> Vec<(String, String)> {
    let mut result = vec![("mediatype".to_string(), "collection".to_string())];
    result.extend(
        user_metadata
            .iter()
            .filter(|(k, _)| k != "mediatype")
            .cloned(),
    );
    result
}

/// Build the details URL for an item.
///
/// For production (host == "archive.org") always uses https://archive.org.
/// For testing uses the configured protocol and host.
fn details_url(client: &IaClient, identifier: &str) -> String {
    if client.host() == "archive.org" {
        format!("https://archive.org/details/{identifier}")
    } else {
        format!(
            "{}://{}/details/{identifier}",
            client.protocol(),
            client.host()
        )
    }
}

/// Create a collection by uploading a cover image.
///
/// The image is uploaded as `{identifier}_itemimage.{ext}`. The upload carries
/// the full metadata, auto-make-bucket, and derive disabled.
async fn create_with_image(
    client: &IaClient,
    identifier: &str,
    image: &Path,
    metadata: &[(String, String)],
    dry_run: bool,
    url: String,
) -> Result<CreateCollectionResult> {
    // Extension is already validated to be Some by the caller.
    let ext = image.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let key = format!("{identifier}_itemimage.{ext}");
    let content_type = infer_content_type(ext);

    // A dry run returns before `upload_file`, which can send requests
    // before its own dry-run check.
    if dry_run {
        return Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: 0,
            url,
        });
    }

    // `checksum: false`: the upload's skip check would send a metadata GET
    // and, when the item already holds an identical image, skip the PUT,
    // so no metadata would be applied while the create reported success.
    // A create always sends its PUT and reports the server's answer.
    let opts = UploadOpts {
        metadata: metadata.to_vec(),
        no_derive: true,
        verify: false,
        checksum: false,
        headers: vec![("Content-Type".to_string(), content_type.to_string())],
        no_collection_check: true,
        ..UploadOpts::default()
    };

    let result = upload_file(
        client, identifier, image, &key, &opts, true, true, None, None,
    )
    .await?;

    match result.status {
        UploadStatus::Uploaded | UploadStatus::Resumed => Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: 200,
            url,
        }),
        // Neither is produced with `checksum: false` and `dry_run: false`;
        // reporting either as created would claim a PUT that was not sent.
        UploadStatus::Skipped | UploadStatus::DryRun => Err(IaError::UploadFailed {
            identifier: identifier.to_string(),
            key,
            message: "the image upload was not sent".into(),
            status: None,
        }),
        UploadStatus::Failed(msg) => Err(IaError::UploadFailed {
            identifier: identifier.to_string(),
            key,
            message: msg,
            status: None,
        }),
    }
}

/// Create a collection without a cover image.
///
/// Sends a zero-body PUT to the S3 item URL with all required IA headers
/// and the encoded metadata headers. Derive is always disabled.
async fn create_without_image(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    dry_run: bool,
    url: String,
) -> Result<CreateCollectionResult> {
    if dry_run {
        return Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: 0,
            url,
        });
    }

    let (access, secret) = client.require_auth()?;
    let auth_header = format!("LOW {access}:{secret}");
    let s3_url = build_s3_item_url(client, identifier);
    let metadata_headers = encode_metadata_headers(metadata)?;

    let mut request = client
        .raw_http()
        .put(&s3_url)
        .header("Authorization", &auth_header)
        .header("x-amz-auto-make-bucket", "1")
        .header("x-archive-queue-derive", "0")
        .header("Content-Length", "0");

    for (k, v) in &metadata_headers {
        request = request.header(k.as_str(), v.as_str());
    }

    let response = request.send().await.map_err(|e| IaError::UploadFailed {
        identifier: identifier.to_string(),
        key: String::new(),
        message: e.to_string(),
        status: None,
    })?;

    let status = response.status();
    if status.is_success() {
        Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: status.as_u16(),
            url,
        })
    } else {
        let body_text = response.text().await.unwrap_or_default();
        let message = describe_s3_failure(status, &body_text);
        Err(IaError::UploadFailed {
            identifier: identifier.to_string(),
            key: String::new(),
            message,
            status: Some(status.as_u16()),
        })
    }
}

/// Infer a MIME content-type string from a file extension.
fn infer_content_type(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn test_client(mock_server: &MockServer) -> IaClient {
        let host_port = mock_server
            .uri()
            .strip_prefix("http://")
            .unwrap()
            .to_string();
        let mut config = crate::IaConfig::default();
        config.general.host = host_port;
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        IaClient::from_config_no_retry(config).unwrap()
    }

    fn temp_image_with_content(ext: &str, content: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{ext}"))
            .tempfile()
            .unwrap();
        file.write_all(content).unwrap();
        file.flush().unwrap();
        file
    }

    // -- content type helpers --

    #[test]
    fn content_type_jpeg() {
        assert_eq!(infer_content_type("jpg"), "image/jpeg");
        assert_eq!(infer_content_type("jpeg"), "image/jpeg");
    }

    #[test]
    fn content_type_png() {
        assert_eq!(infer_content_type("png"), "image/png");
    }

    #[test]
    fn content_type_gif() {
        assert_eq!(infer_content_type("gif"), "image/gif");
    }

    #[test]
    fn content_type_webp() {
        assert_eq!(infer_content_type("webp"), "image/webp");
    }

    #[test]
    fn content_type_unknown_falls_back() {
        assert_eq!(infer_content_type("xyz"), "application/octet-stream");
    }

    #[test]
    fn content_type_case_insensitive() {
        assert_eq!(infer_content_type("JPG"), "image/jpeg");
        assert_eq!(infer_content_type("PNG"), "image/png");
    }

    // -- build_collection_metadata --

    #[test]
    fn mediatype_always_prepended() {
        let meta = vec![("title".to_string(), "My Coll".to_string())];
        let result = build_collection_metadata(&meta);
        assert_eq!(
            result[0],
            ("mediatype".to_string(), "collection".to_string())
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn user_mediatype_dropped() {
        let meta = vec![
            ("mediatype".to_string(), "texts".to_string()),
            ("title".to_string(), "My Coll".to_string()),
        ];
        let result = build_collection_metadata(&meta);
        // Only one mediatype entry (collection), user's "texts" is dropped
        let mediatypes: Vec<_> = result.iter().filter(|(k, _)| k == "mediatype").collect();
        assert_eq!(mediatypes.len(), 1);
        assert_eq!(mediatypes[0].1, "collection");
    }

    // -- invalid_identifier_rejected --

    #[tokio::test]
    async fn invalid_identifier_rejected() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;
        let err = create_collection(&client, "ab", &[], None, false)
            .await
            .unwrap_err();
        assert!(matches!(err, IaError::InvalidIdentifier { .. }));
    }

    // -- image_not_found_rejected --

    #[tokio::test]
    async fn image_not_found_rejected() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;
        let nonexistent = Path::new("/tmp/ia-test-no-such-file-xyz.png");
        let err = create_collection(&client, "test-coll", &[], Some(nonexistent), false)
            .await
            .unwrap_err();
        assert!(matches!(err, IaError::UploadFailed { .. }));
        assert!(err.to_string().contains("not found") || err.to_string().contains("image"));
    }

    // -- image_no_extension_rejected --

    #[tokio::test]
    async fn image_no_extension_rejected() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;
        // Create a temp file with no extension
        let no_ext = tempfile::Builder::new()
            .prefix("ia-test-no-ext-")
            .tempfile()
            .unwrap();
        let no_ext_path_buf = no_ext.path().with_extension("");
        // Write the file without extension to disk
        std::fs::write(&no_ext_path_buf, b"fake image data").unwrap();
        let err = create_collection(&client, "test-coll", &[], Some(&no_ext_path_buf), false)
            .await
            .unwrap_err();
        assert!(matches!(err, IaError::UploadFailed { ref key, .. } if key == "(image)"));
        // Clean up
        let _ = std::fs::remove_file(&no_ext_path_buf);
    }

    // -- create_without_image (PUT to /{id}) --

    #[tokio::test]
    async fn create_without_image_puts_to_identifier_path() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .and(header("x-amz-auto-make-bucket", "1"))
            .and(header("x-archive-queue-derive", "0"))
            .and(header("content-length", "0"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = test_client(&server).await;
        let result = create_collection(&client, "test-collection", &[], None, false)
            .await
            .unwrap();

        assert_eq!(result.identifier, "test-collection");
        assert_eq!(result.status, 200);
    }

    // -- create_with_image (PUT to /{id}/{id}_itemimage.png) --

    #[tokio::test]
    async fn create_with_image_uploads_itemimage() {
        let server = MockServer::start().await;

        // upload_file sends: PUT /{id}/{key} with authorization, expect, content-length
        // wiremock returns 404 for unmatched requests — keep matchers minimal
        Mock::given(method("PUT"))
            .and(path("/test-collection/test-collection_itemimage.png"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = test_client(&server).await;
        let img = temp_image_with_content("png", b"\x89PNG fake");
        let result = create_collection(&client, "test-collection", &[], Some(img.path()), false)
            .await
            .unwrap();

        assert_eq!(result.identifier, "test-collection");
        assert_eq!(result.status, 200);
    }

    // -- mediatype_always_collection --

    #[tokio::test]
    async fn mediatype_always_collection_in_headers() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/my-coll"))
            .and(header("x-archive-meta00-mediatype", "collection"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = test_client(&server).await;
        // User passes mediatype=texts — should be overridden to collection
        let meta = vec![("mediatype".to_string(), "texts".to_string())];
        let result = create_collection(&client, "my-coll", &meta, None, false)
            .await
            .unwrap();

        assert_eq!(result.status, 200);
    }

    // -- dry_run_no_network --

    #[tokio::test]
    async fn dry_run_no_network() {
        // Use a client with a bad host — any real request would fail
        let mut config = crate::IaConfig::default();
        config.general.host = "127.0.0.1:1".to_string(); // nothing listening
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        let client = IaClient::from_config_no_retry(config).unwrap();

        let result = create_collection(&client, "test-coll", &[], None, true)
            .await
            .unwrap();

        assert_eq!(result.status, 0);
        assert_eq!(result.identifier, "test-coll");
    }

    // -- s3_error_response --

    #[tokio::test]
    async fn s3_error_response_returns_http_error() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/my-coll"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server).await;
        let err = create_collection(&client, "my-coll", &[], None, false)
            .await
            .unwrap_err();

        match err {
            IaError::UploadFailed {
                identifier,
                status,
                message,
                ..
            } => {
                assert_eq!(identifier, "my-coll");
                assert_eq!(status, Some(403));
                assert!(message.contains("AccessDenied") || message.contains("Access Denied"));
            }
            other => panic!("expected IaError::UploadFailed, got {other:?}"),
        }
    }

    // -- extra_metadata_included --

    #[tokio::test]
    async fn extra_metadata_included_in_headers() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/my-coll"))
            .and(header("x-archive-meta00-hidden", "true"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = test_client(&server).await;
        let meta = vec![("hidden".to_string(), "true".to_string())];
        let result = create_collection(&client, "my-coll", &meta, None, false)
            .await
            .unwrap();

        assert_eq!(result.status, 200);
    }

    // -- auth_required --

    #[tokio::test]
    async fn auth_required_without_credentials() {
        let server = MockServer::start().await;
        let host_port = server.uri().strip_prefix("http://").unwrap().to_string();
        let mut config = crate::IaConfig::default();
        config.general.host = host_port;
        config.general.secure = false;
        // No s3_access / s3_secret
        let client = IaClient::from_config_no_retry(config).unwrap();

        let err = create_collection(&client, "test-coll", &[], None, false)
            .await
            .unwrap_err();

        assert!(matches!(err, IaError::Auth(_)));
        assert!(
            err.to_string().to_lowercase().contains("credentials")
                || err.to_string().to_lowercase().contains("s3")
        );
    }

    // -- collection_rows --

    fn rec(id: &str, fields: &[(&str, &str)]) -> SpreadsheetRecord {
        (
            id.to_string(),
            fields
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn sorted(mut v: Vec<(String, String)>) -> Vec<(String, String)> {
        v.sort();
        v
    }

    #[test]
    fn rows_keep_file_order_and_take_identifier_and_image_out_of_metadata() {
        let img = temp_image_with_content("png", b"x");
        let img_path = img.path().display().to_string();
        let records = vec![
            rec(
                "coll-b",
                &[
                    ("collection", "parent"),
                    ("title", "B"),
                    ("image", &img_path),
                ],
            ),
            rec("coll-a", &[("collection", "parent"), ("description", "A")]),
        ];
        let rows = collection_rows(records, &[], None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].identifier, "coll-b");
        assert_eq!(
            sorted(rows[0].metadata.clone()),
            kv(&[("collection", "parent"), ("title", "B")])
        );
        assert_eq!(rows[0].image.as_deref(), Some(img.path()));
        assert_eq!(rows[1].identifier, "coll-a");
        assert_eq!(
            sorted(rows[1].metadata.clone()),
            kv(&[("collection", "parent"), ("description", "A")])
        );
        assert_eq!(rows[1].image, None);
    }

    #[test]
    fn defaults_apply_unless_the_row_names_the_field() {
        let defaults = kv(&[
            ("collection", "shared-parent"),
            ("subject", "default-a"),
            ("subject", "default-b"),
            ("hidden", "true"),
        ]);
        let records = vec![
            rec("coll-one", &[("title", "One")]),
            rec(
                "coll-two",
                &[("collection", "other-parent"), ("subject", "own")],
            ),
        ];
        let rows = collection_rows(records, &defaults, None).unwrap();
        assert_eq!(
            sorted(rows[0].metadata.clone()),
            kv(&[
                ("collection", "shared-parent"),
                ("hidden", "true"),
                ("subject", "default-a"),
                ("subject", "default-b"),
                ("title", "One"),
            ])
        );
        assert_eq!(
            sorted(rows[1].metadata.clone()),
            kv(&[
                ("collection", "other-parent"),
                ("hidden", "true"),
                ("subject", "own"),
            ])
        );
    }

    #[test]
    fn image_cell_replaces_the_default_image() {
        let own = temp_image_with_content("jpg", b"x");
        let shared = temp_image_with_content("png", b"y");
        let own_path = own.path().display().to_string();
        let records = vec![
            rec("coll-own", &[("collection", "p"), ("image", &own_path)]),
            rec("coll-shared", &[("collection", "p")]),
        ];
        let rows = collection_rows(records, &[], Some(shared.path())).unwrap();
        assert_eq!(rows[0].image.as_deref(), Some(own.path()));
        assert_eq!(rows[1].image.as_deref(), Some(shared.path()));
    }

    #[test]
    fn indexed_columns_give_several_values() {
        let records = vec![rec(
            "coll-idx",
            &[
                ("collection", "p"),
                ("subject[0]", "first"),
                ("subject[1]", "second"),
            ],
        )];
        let rows = collection_rows(records, &[], None).unwrap();
        assert_eq!(
            rows[0].metadata,
            kv(&[
                ("collection", "p"),
                ("subject", "first"),
                ("subject", "second")
            ])
        );
    }

    #[test]
    fn every_bad_row_is_reported_together() {
        let no_ext = tempfile::Builder::new()
            .prefix("ia-test-no-ext-")
            .tempfile()
            .unwrap();
        let no_ext_path = no_ext.path().display().to_string();
        let records = vec![
            rec("ok-row", &[("collection", "p")]),
            rec("ab", &[("collection", "p")]),
            rec("dup-row", &[("collection", "p")]),
            rec("dup-row", &[("collection", "p")]),
            rec("no-parent", &[("title", "T")]),
            rec(
                "missing-image",
                &[("collection", "p"), ("image", "/nonexistent/ia-test.png")],
            ),
            rec(
                "no-extension",
                &[("collection", "p"), ("image", &no_ext_path)],
            ),
            rec("bad-key", &[("collection", "p"), ("bad key", "v")]),
        ];
        let err = collection_rows(records, &[], None).unwrap_err();
        assert!(matches!(err, IaError::Config(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("batch validation failed"), "{msg}");
        for needle in [
            "ab",
            "dup-row: on more than one row",
            "no-parent: no parent collection",
            "missing-image: image file not found",
            "no-extension: image file has no extension",
            "bad-key:",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in {msg}");
        }
        assert!(!msg.contains("ok-row"), "{msg}");
    }

    #[test]
    fn empty_records_are_an_error() {
        let err = collection_rows(Vec::new(), &[], None).unwrap_err();
        assert!(err.to_string().contains("no records"), "{err}");
    }

    #[test]
    fn a_missing_default_image_is_reported_once() {
        let records = vec![
            rec("coll-one", &[("collection", "p")]),
            rec("coll-two", &[("collection", "p")]),
        ];
        let missing = Path::new("/nonexistent/ia-test-default.png");
        let msg = collection_rows(records, &[], Some(missing))
            .unwrap_err()
            .to_string();
        assert_eq!(msg.matches("image file not found").count(), 1, "{msg}");
        assert!(msg.contains("--image: image file not found"), "{msg}");
    }

    // -- review of PR 153 --

    #[tokio::test]
    async fn dry_run_with_image_sends_no_request() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(599))
            .expect(0)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        let img = temp_image_with_content("png", b"\x89PNG fake");
        let result = create_collection(&client, "dry-img", &[], Some(img.path()), true)
            .await
            .unwrap();
        assert_eq!(result.status, 0);
    }

    #[tokio::test]
    async fn existing_item_with_the_same_image_is_still_sent() {
        let server = MockServer::start().await;
        let img = temp_image_with_content("png", b"\x89PNG same bytes");
        let md5 = crate::upload::checksum::compute_file_md5_async(img.path())
            .await
            .unwrap();
        Mock::given(method("GET"))
            .and(path("/metadata/existing-coll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "existing-coll", "mediatype": "collection"},
                "files": [{"name": "existing-coll_itemimage.png", "md5": md5, "size": "16"}],
            })))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/existing-coll/existing-coll_itemimage.png"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        let result = create_collection(&client, "existing-coll", &[], Some(img.path()), false)
            .await
            .unwrap();
        assert_eq!(result.status, 200);
    }

    #[test]
    fn an_image_that_is_a_directory_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let logo_dir = dir.path().join("logo.d");
        std::fs::create_dir(&logo_dir).unwrap();
        let logo = logo_dir.display().to_string();
        let records = vec![rec("dir-image", &[("collection", "p"), ("image", &logo)])];
        let msg = collection_rows(records, &[], None).unwrap_err().to_string();
        assert!(msg.contains("dir-image: image is not a file"), "{msg}");
    }

    fn jsonl_file(contents: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn jsonl_null_is_no_value_and_the_default_applies() {
        let f = jsonl_file(r#"{"identifier":"series-1","collection":null,"title":null}"#);
        let defaults = kv(&[("collection", "parent")]);
        let rows = read_collection_spreadsheet(f.path(), &defaults, None).unwrap();
        assert_eq!(rows[0].metadata, kv(&[("collection", "parent")]));
    }

    #[test]
    fn jsonl_array_of_strings_gives_repeated_values() {
        let f = jsonl_file(r#"{"identifier":"series-1","collection":"p","subject":["a","b"]}"#);
        let rows = read_collection_spreadsheet(f.path(), &[], None).unwrap();
        assert_eq!(
            rows[0].metadata,
            kv(&[("collection", "p"), ("subject", "a"), ("subject", "b")])
        );
    }

    #[test]
    fn jsonl_values_that_cannot_be_sent_are_refused_with_every_other_problem() {
        let f = jsonl_file(concat!(
            r#"{"identifier":"num-row","collection":"p","year":1942}"#,
            "\n",
            r#"{"identifier":"bool-row","collection":"p","hidden":true}"#,
            "\n",
            r#"{"identifier":"obj-row","collection":"p","x":{"a":"b"}}"#,
            "\n",
            r#"{"identifier":"mixed-row","collection":"p","subject":["a",1]}"#,
            "\n",
            r#"{"identifier":"no-parent"}"#,
            "\n",
            r#"{"identifier":"good-row","collection":"p"}"#,
            "\n",
        ));
        let msg = read_collection_spreadsheet(f.path(), &[], None)
            .unwrap_err()
            .to_string();
        for needle in [
            "num-row: year",
            "bool-row: hidden",
            "obj-row: x",
            "mixed-row: subject",
            "no-parent: no parent collection",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in {msg}");
        }
        assert!(!msg.contains("good-row"), "{msg}");
    }

    #[test]
    fn jsonl_line_without_a_usable_identifier_is_reported() {
        let f = jsonl_file(concat!(
            r#"{"identifier":12345,"collection":"p"}"#,
            "\n",
            r#"{"Identifier":"upper-key","collection":"p"}"#,
            "\n",
            r#"{"title":"no identifier","collection":"p"}"#,
            "\n",
        ));
        let msg = read_collection_spreadsheet(f.path(), &[], None)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("line 1: identifier must be a string (got a number)"),
            "{msg}"
        );
        assert!(msg.contains("line 3: no identifier"), "{msg}");
        // Keys are case-insensitive, `identifier` included: line 2 is a row.
        assert!(
            !msg.contains("line 2") && !msg.contains("upper-key"),
            "{msg}"
        );
    }
}
