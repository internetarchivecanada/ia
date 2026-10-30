pub mod batch;
pub mod check_limit;
pub mod checksum;
pub mod headers;
mod item;
pub mod multipart;
pub(crate) mod progress_body;
mod retry;
pub mod s3_error;
mod single;
pub(crate) mod stall_watch;
pub mod template;
mod types;
pub mod validate;

pub use batch::upload_batch;
pub use check_limit::RateLimitStatus;
pub use item::upload_item;
pub use multipart::MULTIPART_FALLBACK_MIN_SIZE;
pub use single::upload_file;
pub use template::{generate_template, write_template_csv, TemplateOpts, TemplateRow};
pub use types::{
    MultipartFallback, MultipartUploadInfo, PartInfo, ProgressCallback, UploadOpts,
    UploadOptsBuilder, UploadProgress, UploadProgressStatus, UploadResult, UploadStatus,
};

use crate::error::{IaError, Result};
use crate::IaClient;

// ── Shared S3 URL helpers ───────────────────────────────────────────────

/// Check that `key` names a file inside an item.
///
/// A key is a `/`-separated path. Percent-encoding leaves `.` and `..`
/// segments as they are, and URL parsing resolves them, so a `..` segment
/// would send the request to another item; a leading `/` or an empty
/// segment names a different file than the one meant. Such a key is refused,
/// never rewritten.
///
/// ```
/// use ia_core::upload::validate_key;
/// assert!(validate_key("scans/a.pdf").is_ok());
/// assert!(validate_key("../scans/a.pdf").is_err());
/// assert!(validate_key("scans//a.pdf").is_err());
/// ```
///
/// # Errors
///
/// Returns `IaError::InvalidKey` if the key is empty, starts with `/`, or
/// has a `..`, `.` or empty segment.
pub fn validate_key(key: &str) -> Result<()> {
    let reason = if key.is_empty() {
        "it is empty"
    } else if key.starts_with('/') {
        "it starts with '/'"
    } else if key.split('/').any(|seg| seg == "..") {
        "it has a '..' segment"
    } else if key.split('/').any(|seg| seg == ".") {
        "it has a '.' segment"
    } else if key.split('/').any(str::is_empty) {
        "it has an empty segment"
    } else {
        return Ok(());
    };
    Err(IaError::InvalidKey {
        key: key.to_string(),
        reason: reason.to_string(),
    })
}

/// Build the S3 URL for a file within an item.
///
/// For production (host == "archive.org"), targets s3.us.archive.org.
/// For testing (host != "archive.org"), targets the mock server host directly.
///
/// # Errors
///
/// Returns `IaError::InvalidKey` if `key` does not name a file inside the
/// item (see [`validate_key`]).
pub(crate) fn build_s3_url(client: &IaClient, identifier: &str, key: &str) -> Result<String> {
    validate_key(key)?;
    // Encode each path segment individually, preserving `/` separators.
    // Python uses urllib.parse.quote(key) which also preserves `/`.
    let encoded_key = key
        .split('/')
        .map(|seg| urlencoding::encode(seg))
        .collect::<Vec<_>>()
        .join("/");
    let protocol = client.protocol();
    let host = client.host();

    Ok(if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{identifier}/{encoded_key}")
    } else {
        format!("{protocol}://{host}/{identifier}/{encoded_key}")
    })
}

/// Build the S3 URL for item-level operations (e.g. list uploads).
pub(crate) fn build_s3_item_url(client: &IaClient, identifier: &str) -> String {
    let protocol = client.protocol();
    let host = client.host();
    if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{identifier}")
    } else {
        format!("{protocol}://{host}/{identifier}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_s3_url_refuses_a_key_outside_the_item() {
        let client = IaClient::from_config(crate::IaConfig::default()).unwrap();
        for key in ["../other/a.bin", "/a.bin", "scans//a.bin", "./a.bin", ""] {
            let err = build_s3_url(&client, "test-item", key).unwrap_err();
            assert!(matches!(err, IaError::InvalidKey { .. }), "{key}: {err:?}");
        }
    }
}
