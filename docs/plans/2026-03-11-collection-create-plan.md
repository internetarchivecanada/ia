# `ia collection create` Implementation Plan

**Goal:** Add `ia collection create` command that creates Internet Archive collections via S3 PUT with required metadata guardrails, optional image upload, and arbitrary extra metadata.

**Architecture:** `create_collection()` in `ia-core/src/collection.rs` has two paths: (1) with image — delegates to existing `upload::upload_file()` with collection-appropriate `UploadOpts` (gets retry, rate-limit, progress for free); (2) without image — thin zero-body PUT using shared URL builders, auth, header encoding, and S3 error parsing. CLI in `ia-cli/src/commands/collection.rs` uses enum-based subcommand routing.

**Tech Stack:** Rust, clap (Args/Subcommand), reqwest, wiremock (tests), anyhow, thiserror

**Spec:** `docs/plans/2026-03-11-collection-create-design.md`

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `ia-core/src/collection.rs` | `create_collection()`, `CreateCollectionResult`, content-type helper, image validation |
| Modify | `ia-core/src/lib.rs` | Add `pub mod collection;` |
| Create | `ia-cli/src/commands/collection.rs` | CLI args, subcommand routing, output formatting |
| Modify | `ia-cli/src/commands/mod.rs` | Add `pub mod collection;` |
| Modify | `ia-cli/src/main.rs` | Register `Collection` command with alias `col` |
| Create | `ia-cli/tests/collection.rs` | CLI integration tests |

---

## Chunk 1: Core Module

### Task 1: Create `collection.rs` with types and content-type helper

**Files:**
- Create: `ia-core/src/collection.rs`
- Modify: `ia-core/src/lib.rs:1-18`

- [ ] **Step 1: Write failing tests for content-type inference**

In `ia-core/src/collection.rs`, write the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core collection::tests --no-run 2>&1 | head -20`
Expected: compilation failure — `infer_content_type` not defined

- [ ] **Step 3: Write types and content-type helper**

At the top of `ia-core/src/collection.rs`:

```rust
use std::path::Path;

use anyhow::Context;

use crate::upload::headers::encode_metadata_headers;
use crate::upload::s3_error::parse_s3_error;
use crate::upload::validate::validate_identifier;
use crate::upload::{build_s3_item_url, upload_file, UploadOpts};
use crate::IaClient;

/// Result of creating a collection.
#[derive(Debug, Clone)]
pub struct CreateCollectionResult {
    pub identifier: String,
    pub status: u16,
    pub url: String,
}

/// Infer Content-Type from a file extension.
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
```

- [ ] **Step 4: Register module in lib.rs**

Add `pub mod collection;` to `ia-core/src/lib.rs` (alphabetically, after `pub mod client;`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-core collection::tests -- --nocapture`
Expected: all 7 tests pass

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/collection.rs ia-core/src/lib.rs
git commit -m "feat(core): add collection module with types and content-type helper"
```

### Task 2: Implement `create_collection()` with wiremock tests

**Files:**
- Modify: `ia-core/src/collection.rs`

Key design decision: **reuse the upload codepath**.

- **With image:** Delegates to `upload::upload_file()` with `UploadOpts` configured for
  collection creation (`no_derive`, `verify: false`, metadata includes `mediatype: collection`).
  The image is uploaded as `{identifier}_itemimage.{ext}` with `is_first_file=true`
  (triggers `x-amz-auto-make-bucket: 1`) and `is_last_file=true`. This gets retry logic,
  rate-limit handling, and S3 error parsing for free.

- **Without image:** Zero-body PUT to `s3.us.archive.org/{identifier}`. This is genuinely
  different from a file upload (no file to stream), so it uses shared infrastructure
  (URL builders, auth, header encoding, S3 error parsing) directly — ~25 lines of code.

- [ ] **Step 1: Write failing wiremock test for zero-body create**

Add to the `tests` module in `collection.rs`:

```rust
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: build a test client pointing at the mock server.
    async fn test_client(mock_server: &MockServer) -> IaClient {
        let mut config = crate::IaConfig::default();
        config.general.host = mock_server.uri().replace("http://", "");
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        IaClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn create_without_image() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .and(header("x-amz-auto-make-bucket", "1"))
            .and(header("Content-Length", "0"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let metadata = vec![
            ("title".into(), "Test Collection".into()),
            ("description".into(), "A test".into()),
            ("subject".into(), "testing".into()),
            ("collection".into(), "test_parent".into()),
        ];

        let result = create_collection(
            &client,
            "test-collection",
            &metadata,
            None,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.identifier, "test-collection");
        assert_eq!(result.status, 200);
        assert!(result.url.contains("test-collection"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core collection::tests::create_without_image`
Expected: FAIL — `create_collection` not defined

- [ ] **Step 3: Implement `create_collection()`**

Add to `ia-core/src/collection.rs`, after the `infer_content_type` function:

```rust
/// Build the details URL for a collection.
fn details_url(client: &IaClient, identifier: &str) -> String {
    let protocol = client.protocol();
    let host = client.host();
    if host == "archive.org" {
        format!("https://archive.org/details/{identifier}")
    } else {
        format!("{protocol}://{host}/details/{identifier}")
    }
}

/// Build the full metadata list, always injecting `mediatype=collection`.
///
/// User-provided `mediatype` values are silently dropped to prevent
/// accidentally creating a non-collection item.
fn build_collection_metadata(
    user_metadata: &[(String, String)],
) -> Vec<(String, String)> {
    let mut full = vec![("mediatype".into(), "collection".into())];
    for (k, v) in user_metadata {
        if k == "mediatype" {
            continue; // always force collection
        }
        full.push((k.clone(), v.clone()));
    }
    full
}

/// Create an Internet Archive collection via S3 PUT.
///
/// Collections are items with `mediatype=collection`. If `image` is provided,
/// delegates to `upload_file()` (same codepath as `ia upload`) to upload
/// `{identifier}_itemimage.{ext}` with collection metadata — getting retry,
/// rate-limit, and progress handling for free. Without an image, sends a
/// zero-body PUT to create the empty bucket.
///
/// # Arguments
///
/// * `client` — Authenticated IA client
/// * `identifier` — New collection identifier (validated)
/// * `metadata` — Ordered key-value metadata pairs (title, description, etc.)
/// * `image` — Optional path to a collection image file
/// * `queue_derive` — If true, allows derive; if false sets `x-archive-queue-derive: 0`
/// * `dry_run` — If true, validate only, don't send request
pub async fn create_collection(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    image: Option<&Path>,
    queue_derive: bool,
    dry_run: bool,
) -> anyhow::Result<CreateCollectionResult> {
    // Validate identifier
    validate_identifier(identifier)?;

    // Validate image extension early (before any network)
    if let Some(img_path) = image {
        if !img_path.exists() {
            anyhow::bail!("image file not found: {}", img_path.display());
        }
        img_path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "image file has no extension: {} (extension required for S3 key)",
                    img_path.display()
                )
            })?;
    }

    let full_metadata = build_collection_metadata(metadata);
    let url = details_url(client, identifier);

    if let Some(img_path) = image {
        create_with_image(client, identifier, &full_metadata, img_path, queue_derive, dry_run, &url).await
    } else {
        create_without_image(client, identifier, &full_metadata, queue_derive, dry_run, &url).await
    }
}

/// Create collection WITH image — delegates to `upload_file()`.
async fn create_with_image(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    image: &Path,
    queue_derive: bool,
    dry_run: bool,
    details_url: &str,
) -> anyhow::Result<CreateCollectionResult> {
    let ext = image
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| anyhow::anyhow!("image has no extension"))?;
    let key = format!("{identifier}_itemimage.{ext}");

    let content_type = infer_content_type(ext);

    let opts = UploadOpts {
        metadata: metadata.to_vec(),
        no_derive: !queue_derive,
        verify: false,
        dry_run,
        headers: vec![("Content-Type".into(), content_type.into())],
        ..UploadOpts::default()
    };

    let result = upload_file(
        client,
        identifier,
        image,
        &key,
        &opts,
        true,  // is_first_file → auto-make-bucket
        true,  // is_last_file → derive on last (if not no_derive)
        None,  // no size hint
        None,  // no progress callback
    )
    .await?;

    match result.status {
        crate::upload::UploadStatus::Failed(msg) => {
            anyhow::bail!("collection creation failed: {msg}");
        }
        crate::upload::UploadStatus::Uploaded | crate::upload::UploadStatus::Skipped => {
            Ok(CreateCollectionResult {
                identifier: identifier.to_string(),
                status: 200,
                url: details_url.to_string(),
            })
        }
        crate::upload::UploadStatus::DryRun => {
            Ok(CreateCollectionResult {
                identifier: identifier.to_string(),
                status: 0,
                url: details_url.to_string(),
            })
        }
    }
}

/// Create collection WITHOUT image — zero-body PUT using shared infra.
async fn create_without_image(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    queue_derive: bool,
    dry_run: bool,
    details_url: &str,
) -> anyhow::Result<CreateCollectionResult> {
    if dry_run {
        return Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status: 0,
            url: details_url.to_string(),
        });
    }

    let (access, secret) = client.require_auth()?;
    let auth_header = format!("LOW {access}:{secret}");
    let s3_url = build_s3_item_url(client, identifier);
    let meta_headers = encode_metadata_headers(metadata);

    let mut request = client
        .raw_http()
        .put(&s3_url)
        .header("Authorization", &auth_header)
        .header("x-amz-auto-make-bucket", "1")
        .header("Content-Length", "0");

    if !queue_derive {
        request = request.header("x-archive-queue-derive", "0");
    }

    for (k, v) in &meta_headers {
        request = request.header(k.as_str(), v.as_str());
    }

    let response = request.send().await?;
    let status = response.status().as_u16();

    if response.status().is_success() {
        Ok(CreateCollectionResult {
            identifier: identifier.to_string(),
            status,
            url: details_url.to_string(),
        })
    } else {
        let body = response.text().await.unwrap_or_default();
        let msg = if let Some(s3_err) = parse_s3_error(&body) {
            format!("S3 returned {status}: {} ({})", s3_err.message, s3_err.code)
        } else if !body.is_empty() {
            format!("S3 returned {status}: {body}")
        } else {
            format!("S3 returned {status}")
        };
        anyhow::bail!(msg)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core collection::tests::create_without_image`
Expected: PASS

- [ ] **Step 5: Write and run additional wiremock tests**

Add these tests to the `tests` module:

```rust
    #[tokio::test]
    async fn create_with_image() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        // upload_file sends PUT with the image body
        Mock::given(method("PUT"))
            .and(path("/test-collection/test-collection_itemimage.png"))
            .and(header("x-amz-auto-make-bucket", "1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("logo.png");
        std::fs::write(&img_path, b"fake-png-data").unwrap();

        let metadata = vec![
            ("title".into(), "Test Collection".into()),
            ("description".into(), "A test".into()),
            ("subject".into(), "testing".into()),
            ("collection".into(), "test_parent".into()),
        ];

        let result = create_collection(
            &client,
            "test-collection",
            &metadata,
            Some(img_path.as_path()),
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.status, 200);
    }

    #[tokio::test]
    async fn mediatype_always_collection() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .and(header("x-archive-meta00-mediatype", "collection"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let metadata = vec![
            ("mediatype".into(), "texts".into()), // user tries to override
            ("title".into(), "Test".into()),
            ("collection".into(), "parent".into()),
        ];

        let result = create_collection(
            &client,
            "test-collection",
            &metadata,
            None,
            false,
            false,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn queue_derive_off_by_default() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .and(header("x-archive-queue-derive", "0"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let metadata = vec![
            ("title".into(), "Test".into()),
            ("collection".into(), "parent".into()),
        ];

        create_collection(&client, "test-collection", &metadata, None, false, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dry_run_no_network() {
        let mut config = crate::IaConfig::default();
        config.general.host = "localhost:0".into();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let metadata = vec![
            ("title".into(), "Test".into()),
            ("collection".into(), "parent".into()),
        ];

        let result = create_collection(
            &client,
            "test-collection",
            &metadata,
            None,
            false,
            true,
        )
        .await
        .unwrap();

        assert_eq!(result.status, 0);
        assert_eq!(result.identifier, "test-collection");
    }

    #[tokio::test]
    async fn s3_error_response() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"),
            )
            .mount(&server)
            .await;

        let metadata = vec![
            ("title".into(), "Test".into()),
            ("collection".into(), "parent".into()),
        ];

        let err = create_collection(&client, "test-collection", &metadata, None, false, false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("403"));
        assert!(err.to_string().contains("Access Denied"));
    }

    #[test]
    fn invalid_identifier_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = {
            let mut config = crate::IaConfig::default();
            config.general.host = "localhost:0".into();
            config.general.secure = false;
            config.s3_access = Some("a".into());
            config.s3_secret = Some("s".into());
            IaClient::from_config(config).unwrap()
        };

        let metadata = vec![("title".into(), "T".into()), ("collection".into(), "c".into())];
        let err = rt
            .block_on(create_collection(&client, "ab", &metadata, None, false, false))
            .unwrap_err();
        assert!(err.to_string().contains("at least 3 characters"));
    }

    #[test]
    fn image_no_extension_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("noext");
        std::fs::write(&img, b"data").unwrap();

        let client = {
            let mut config = crate::IaConfig::default();
            config.general.host = "localhost:0".into();
            config.general.secure = false;
            config.s3_access = Some("a".into());
            config.s3_secret = Some("s".into());
            IaClient::from_config(config).unwrap()
        };

        let metadata = vec![("title".into(), "T".into()), ("collection".into(), "c".into())];
        let err = rt
            .block_on(create_collection(&client, "test-col", &metadata, Some(img.as_path()), false, false))
            .unwrap_err();
        assert!(err.to_string().contains("no extension"));
    }

    #[test]
    fn image_not_found_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = {
            let mut config = crate::IaConfig::default();
            config.general.host = "localhost:0".into();
            config.general.secure = false;
            config.s3_access = Some("a".into());
            config.s3_secret = Some("s".into());
            IaClient::from_config(config).unwrap()
        };

        let metadata = vec![("title".into(), "T".into()), ("collection".into(), "c".into())];
        let img = Path::new("/tmp/ia-test-nonexistent-image.png");
        let err = rt
            .block_on(create_collection(&client, "test-col", &metadata, Some(img), false, false))
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn extra_metadata_included() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;

        Mock::given(method("PUT"))
            .and(path("/test-collection"))
            .and(header("x-archive-meta00-mediatype", "collection"))
            .and(header("x-archive-meta00-title", "uri(My%20Collection)"))
            .and(header("x-archive-meta00-hidden", "true"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let metadata = vec![
            ("title".into(), "My Collection".into()),
            ("collection".into(), "parent".into()),
            ("hidden".into(), "true".into()),
        ];

        create_collection(&client, "test-collection", &metadata, None, false, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn auth_required() {
        let client = {
            let mut config = crate::IaConfig::default();
            config.general.host = "localhost:0".into();
            config.general.secure = false;
            IaClient::from_config(config).unwrap()
        };

        let metadata = vec![
            ("title".into(), "T".into()),
            ("collection".into(), "c".into()),
        ];

        let err = create_collection(&client, "test-col", &metadata, None, false, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("credentials"));
    }
```

- [ ] **Step 6: Run all collection tests**

Run: `cargo test -p ia-core collection::tests`
Expected: all tests pass (13 tests)

- [ ] **Step 7: Commit**

```bash
git add ia-core/src/collection.rs
git commit -m "feat(core): implement create_collection with upload_file reuse

With image: delegates to upload_file() with collection UploadOpts —
gets retry, rate-limit, and S3 error handling for free.
Without image: thin zero-body PUT using shared URL/auth/header infra.

13 tests covering: with/without image, mediatype enforcement, derive
control, dry-run, S3 errors, auth required, identifier/image validation,
extra metadata."
```

---

## Chunk 2: CLI Command

### Task 3: Create `ia-cli/src/commands/collection.rs` with args and subcommand structure

**Files:**
- Create: `ia-cli/src/commands/collection.rs`
- Modify: `ia-cli/src/commands/mod.rs:1-11`
- Modify: `ia-cli/src/main.rs:88-117` (Commands enum), `ia-cli/src/main.rs:218-255` (match dispatch)

- [ ] **Step 1: Write the CLI command module**

Create `ia-cli/src/commands/collection.rs`:

```rust
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

use ia_core::collection::{create_collection, CreateCollectionResult};
use ia_core::IaClient;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
#[command(
    about = "Manage Internet Archive collections",
    long_about = "Create and manage Internet Archive collections. Collections are items with \
        mediatype=collection that group related items together.",
)]
pub struct CollectionArgs {
    #[command(subcommand)]
    pub command: CollectionCommand,
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    /// Create a new collection
    #[command(
        long_about = "Create a new Internet Archive collection via S3. Requires title, \
            description, subject, and parent collection. Optionally upload a collection image.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Create a simple collection</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource</bold>\
             \n\n  <dim># Create with an image</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"A collection of things\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    --image logo.png</bold>\
             \n\n  <dim># Create with extra metadata</dim>\
             \n  <bold>$ ia collection create my-collection \\</bold>\
             \n  <bold>    --title \"My Collection\" \\</bold>\
             \n  <bold>    --description \"Desc\" \\</bold>\
             \n  <bold>    --subject \"things\" \\</bold>\
             \n  <bold>    --collection opensource \\</bold>\
             \n  <bold>    -m hidden:true -m num-top-dl:5</bold>\n"
        ),
    )]
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Collection identifier
    pub identifier: String,

    /// Collection title
    #[arg(short = 't', long)]
    pub title: String,

    /// Collection description
    #[arg(long)]
    pub description: String,

    /// Subject/topic
    #[arg(short = 's', long)]
    pub subject: String,

    /// Parent collection identifier
    #[arg(short = 'C', long)]
    pub collection: String,

    /// Path to collection image file
    #[arg(short = 'I', long)]
    pub image: Option<PathBuf>,

    /// Additional metadata (repeatable, KEY:VALUE)
    #[arg(short = 'm', long = "metadata")]
    pub metadata: Vec<String>,

    /// Enable derive (default: derive is off for collections)
    #[arg(long)]
    pub derive: bool,

    /// Validate everything without sending the request
    #[arg(long)]
    pub dry_run: bool,

    /// Output JSON
    #[arg(long)]
    pub json: bool,
}

// ─── Run ─────────────────────────────────────────────────────────────────────

pub async fn run(client: &IaClient, args: CollectionArgs, quiet: u8) -> Result<()> {
    match args.command {
        CollectionCommand::Create(create_args) => run_create(client, create_args, quiet).await,
    }
}

async fn run_create(client: &IaClient, args: CreateArgs, quiet: u8) -> Result<()> {
    // Build metadata list from required flags + extra -m pairs
    let mut metadata: Vec<(String, String)> = vec![
        ("title".into(), args.title),
        ("description".into(), args.description),
        ("subject".into(), args.subject),
        ("collection".into(), args.collection),
    ];

    // Parse and append extra metadata
    for m in &args.metadata {
        let (key, value) = m
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid KEY:VALUE format: '{m}'"))?;
        metadata.push((key.to_string(), value.to_string()));
    }

    let image_path = args.image.as_deref();

    let result = create_collection(
        client,
        &args.identifier,
        &metadata,
        image_path,
        args.derive,
        args.dry_run,
    )
    .await;

    match result {
        Ok(ref r) => print_success(r, args.json, args.dry_run, quiet),
        Err(ref e) => {
            if args.json {
                // Note: status code is not always available (e.g., validation errors,
                // network errors). Include it when the error message contains one.
                let json = serde_json::json!({
                    "error": true,
                    "identifier": args.identifier,
                    "message": e.to_string(),
                });
                eprintln!("{}", serde_json::to_string(&json)?);
            }
        }
    }

    result.map(|_| ())
}

fn print_success(result: &CreateCollectionResult, json: bool, dry_run: bool, quiet: u8) {
    if json {
        let json = serde_json::json!({
            "identifier": result.identifier,
            "status": result.status,
            "url": result.url,
        });
        println!("{}", serde_json::to_string(&json).unwrap());
    } else if quiet == 0 {
        let prefix = if dry_run {
            style("dry-run:").yellow().bold()
        } else {
            style("created:").green().bold()
        };
        println!("{prefix} {}", result.url);
    }
}
```

- [ ] **Step 2: Register the module in `commands/mod.rs`**

Add `pub mod collection;` to `ia-cli/src/commands/mod.rs` (alphabetically, after `pub mod completions;`).

- [ ] **Step 3: Register the command in `main.rs`**

In the `Commands` enum (after `Completions`):

```rust
    /// Create and manage collections
    #[command(visible_alias = "col")]
    Collection(commands::collection::CollectionArgs),
```

In the match dispatch (after `Commands::Completions(_)` unreachable):

```rust
        Commands::Collection(args) => {
            commands::collection::run(&client, args, cli.quiet).await?
        }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p ia-cli`
Expected: compiles without errors

- [ ] **Step 5: Verify help output**

Run: `cargo run -p ia-cli -- collection --help`
Expected: shows "Create and manage Internet Archive collections" with `create` subcommand listed

Run: `cargo run -p ia-cli -- collection create --help`
Expected: shows all required flags and examples

- [ ] **Step 6: Commit**

```bash
git add ia-cli/src/commands/collection.rs ia-cli/src/commands/mod.rs ia-cli/src/main.rs
git commit -m "feat(cli): add ia collection create command

Registers the collection command with alias 'col'. Supports required
metadata flags (--title, --description, --subject, --collection),
optional --image, extra -m metadata, --derive, --dry-run, and --json.

Follows existing enum-based subcommand pattern (extensible for future
collection operations like list)."
```

### Task 4: CLI integration tests

**Files:**
- Create: `ia-cli/tests/collection.rs`

- [ ] **Step 1: Write integration tests**

Create `ia-cli/tests/collection.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::NamedTempFile;

/// Helper: create `ia` command with an empty config file.
fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn collection_create_missing_title_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--title"));
}

#[test]
fn collection_create_missing_description_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--subject", "subj",
            "--collection", "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--description"));
}

#[test]
fn collection_create_missing_subject_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--collection", "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--subject"));
}

#[test]
fn collection_create_missing_collection_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--collection"));
}

#[test]
fn collection_create_missing_identifier_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
        ])
        .assert()
        .failure();
}

#[test]
fn collection_create_invalid_metadata_format() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
            "-m", "no-colon",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid KEY:VALUE format"));
}

#[test]
fn collection_create_dry_run_no_auth_needed() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run:"));
}

#[test]
fn collection_create_dry_run_json() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
            "--dry-run", "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"identifier\":\"test-col\""));
}

#[test]
fn collection_create_nonexistent_image_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
            "--image", "/tmp/ia-test-nonexistent-image.png",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn collection_alias_col_works() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "col", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run:"));
}

#[test]
fn collection_create_no_auth_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "collection", "create", "test-col",
            "--title", "Title",
            "--description", "desc",
            "--subject", "subj",
            "--collection", "parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("credentials"));
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p ia-cli --test collection`
Expected: all 11 tests pass

- [ ] **Step 3: Run full test suite to check for regressions**

Run: `cargo test --workspace`
Expected: all existing tests still pass, plus new collection tests

- [ ] **Step 4: Commit**

```bash
git add ia-cli/tests/collection.rs
git commit -m "test(cli): add collection create integration tests

11 tests covering: missing required flags, invalid metadata format,
dry-run without auth, dry-run JSON output, nonexistent image,
col alias, and auth required without dry-run."
```

### Task 5: Final verification

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings

- [ ] **Step 2: Run fmt check**

Run: `cargo fmt --check`
Expected: no formatting issues

- [ ] **Step 3: Run doc check**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
Expected: no doc warnings

- [ ] **Step 4: Run full test suite one more time**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 5: Commit any fixes from verification, then done**
