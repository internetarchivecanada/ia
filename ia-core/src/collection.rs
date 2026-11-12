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
pub const IMAGE_COLUMN: &str = "image";

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
    if records.is_empty() {
        return Err(IaError::Config("spreadsheet contains no records".into()));
    }

    let mut errors: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows = Vec::with_capacity(records.len());

    for (identifier, mut fields) in records {
        let image = fields
            .remove(IMAGE_COLUMN)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| default_image.map(Path::to_path_buf));

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
        if let Some(img) = &image {
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

/// Check that a cover image exists and has an extension, from which its
/// content type is inferred. Returns the problem as text.
fn check_image(img: &Path) -> std::result::Result<(), String> {
    if !img.exists() {
        return Err(format!("image file not found: {}", img.display()));
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

    let opts = UploadOpts {
        metadata: metadata.to_vec(),
        no_derive: true,
        verify: false,
        dry_run,
        headers: vec![("Content-Type".to_string(), content_type.to_string())],
        no_collection_check: true,
        ..UploadOpts::default()
    };

    let result = upload_file(
        client, identifier, image, &key, &opts, true, true, None, None,
    )
    .await?;

    match result.status {
        UploadStatus::Uploaded | UploadStatus::Skipped | UploadStatus::Resumed => {
            Ok(CreateCollectionResult {
                identifier: identifier.to_string(),
                status: 200,
                url,
            })
        }
        UploadStatus::DryRun => Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: 0,
            url,
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
}
