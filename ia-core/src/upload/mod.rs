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
pub mod template;
mod types;
pub mod validate;

pub use batch::upload_batch;
pub use check_limit::RateLimitStatus;
pub use item::upload_item;
pub use single::upload_file;
pub use template::{generate_template, write_template_csv, TemplateOpts, TemplateRow};
pub use types::{
    MultipartUploadInfo, PartInfo, ProgressCallback, UploadOpts, UploadOptsBuilder, UploadProgress,
    UploadProgressStatus, UploadResult, UploadStatus,
};

use crate::IaClient;

// ── Shared S3 URL helpers ───────────────────────────────────────────────

/// Build the S3 URL for a file within an item.
///
/// For production (host == "archive.org"), targets s3.us.archive.org.
/// For testing (host != "archive.org"), targets the mock server host directly.
pub(crate) fn build_s3_url(client: &IaClient, identifier: &str, key: &str) -> String {
    // Encode each path segment individually, preserving `/` separators.
    // Python uses urllib.parse.quote(key) which also preserves `/`.
    let encoded_key = key
        .split('/')
        .map(|seg| urlencoding::encode(seg))
        .collect::<Vec<_>>()
        .join("/");
    let protocol = client.protocol();
    let host = client.host();

    if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{identifier}/{encoded_key}")
    } else {
        format!("{protocol}://{host}/{identifier}/{encoded_key}")
    }
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
