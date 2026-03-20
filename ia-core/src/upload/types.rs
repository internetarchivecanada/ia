use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Options for upload operations.
#[derive(Debug, Clone)]
pub struct UploadOpts {
    /// Metadata key-value pairs to set on the item.
    pub metadata: Vec<(String, String)>,
    /// Explicit remote filename (required for stdin uploads).
    pub remote_name: Option<String>,
    /// Prepend this path prefix to all remote filenames.
    pub remote_dir: Option<String>,
    /// Preserve relative directory structure in remote filenames.
    pub keep_directories: bool,
    /// Send Content-MD5 header for server-side verification.
    pub verify: bool,
    /// Skip files whose MD5 matches the remote copy.
    pub checksum: bool,
    /// Pre-computed MD5 checksums keyed by filename.
    pub checksum_file: Option<HashMap<String, String>>,
    /// Delete local file after verified upload.
    pub delete_after_upload: bool,
    /// Skip derivative generation (x-archive-queue-derive: 0 on all files).
    pub no_derive: bool,
    /// Don't keep old file versions (omit x-archive-keep-old-version).
    pub no_backup: bool,
    /// Error if item doesn't already exist.
    pub no_auto_make_bucket: bool,
    /// Don't send x-archive-size-hint header.
    pub no_size_hint: bool,
    /// Skip collection existence check.
    pub no_collection_check: bool,
    /// Upload to test_collection (items auto-removed after 30 days).
    pub test_item: bool,
    /// Use multipart upload (Phase 2).
    pub multipart: bool,
    /// Maximum retry attempts on transient failure.
    pub retries: u32,
    /// Sleep duration between retries.
    pub retry_sleep: Duration,
    /// Additional HTTP headers to include.
    pub headers: Vec<(String, String)>,
    /// Validate everything but don't actually upload.
    pub dry_run: bool,
}

impl Default for UploadOpts {
    fn default() -> Self {
        Self {
            metadata: Vec::new(),
            remote_name: None,
            remote_dir: None,
            keep_directories: false,
            verify: true,
            checksum: true,
            checksum_file: None,
            delete_after_upload: false,
            no_derive: false,
            no_backup: false,
            no_auto_make_bucket: false,
            no_size_hint: false,
            no_collection_check: false,
            test_item: false,
            multipart: false,
            retries: 10,
            retry_sleep: Duration::from_secs(30),
            headers: Vec::new(),
            dry_run: false,
        }
    }
}

/// Builder for [`UploadOpts`].
///
/// All fields default to the same values as `UploadOpts::default()`.
///
/// # Example
///
/// ```
/// use ia_core::upload::UploadOptsBuilder;
///
/// let opts = UploadOptsBuilder::new()
///     .verify(true)
///     .dry_run(true)
///     .metadata(vec![("mediatype".into(), "texts".into())])
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct UploadOptsBuilder {
    opts: UploadOpts,
}

impl UploadOptsBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            opts: UploadOpts::default(),
        }
    }

    /// Set metadata key-value pairs.
    pub fn metadata(mut self, metadata: Vec<(String, String)>) -> Self {
        self.opts.metadata = metadata;
        self
    }

    /// Set the explicit remote filename.
    pub fn remote_name(mut self, name: impl Into<String>) -> Self {
        self.opts.remote_name = Some(name.into());
        self
    }

    /// Set the remote directory prefix.
    pub fn remote_dir(mut self, dir: impl Into<String>) -> Self {
        self.opts.remote_dir = Some(dir.into());
        self
    }

    /// Set whether to preserve directory structure.
    pub fn keep_directories(mut self, keep: bool) -> Self {
        self.opts.keep_directories = keep;
        self
    }

    /// Set whether to send Content-MD5 for verification.
    pub fn verify(mut self, verify: bool) -> Self {
        self.opts.verify = verify;
        self
    }

    /// Set whether to skip already-uploaded files.
    pub fn checksum(mut self, skip: bool) -> Self {
        self.opts.checksum = skip;
        self
    }

    /// Set pre-computed MD5 checksums.
    pub fn checksum_file(mut self, checksums: HashMap<String, String>) -> Self {
        self.opts.checksum_file = Some(checksums);
        self
    }

    /// Set whether to delete local files after upload.
    pub fn delete_after_upload(mut self, delete: bool) -> Self {
        self.opts.delete_after_upload = delete;
        self
    }

    /// Set whether to skip derivative generation.
    pub fn no_derive(mut self, no_derive: bool) -> Self {
        self.opts.no_derive = no_derive;
        self
    }

    /// Set whether to skip keeping old file versions.
    pub fn no_backup(mut self, no_backup: bool) -> Self {
        self.opts.no_backup = no_backup;
        self
    }

    /// Set whether to error if item doesn't exist.
    pub fn no_auto_make_bucket(mut self, no_auto: bool) -> Self {
        self.opts.no_auto_make_bucket = no_auto;
        self
    }

    /// Set whether to skip the size hint header.
    pub fn no_size_hint(mut self, no_hint: bool) -> Self {
        self.opts.no_size_hint = no_hint;
        self
    }

    /// Set whether to skip collection existence check.
    pub fn no_collection_check(mut self, no_check: bool) -> Self {
        self.opts.no_collection_check = no_check;
        self
    }

    /// Set whether to upload to test_collection.
    pub fn test_item(mut self, test: bool) -> Self {
        self.opts.test_item = test;
        self
    }

    /// Set whether to use multipart upload.
    pub fn multipart(mut self, multipart: bool) -> Self {
        self.opts.multipart = multipart;
        self
    }

    /// Set maximum retry attempts.
    pub fn retries(mut self, retries: u32) -> Self {
        self.opts.retries = retries;
        self
    }

    /// Set sleep duration between retries.
    pub fn retry_sleep(mut self, duration: Duration) -> Self {
        self.opts.retry_sleep = duration;
        self
    }

    /// Set additional HTTP headers.
    pub fn headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.opts.headers = headers;
        self
    }

    /// Set whether to validate without uploading.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.opts.dry_run = dry_run;
        self
    }

    /// Consume the builder and return the configured [`UploadOpts`].
    pub fn build(self) -> UploadOpts {
        self.opts
    }
}

impl Default for UploadOptsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a single file upload.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    /// Item identifier on archive.org.
    pub identifier: String,
    /// Remote filename (S3 key).
    pub key: String,
    /// Upload outcome.
    #[serde(flatten)]
    pub status: UploadStatus,
    /// File size in bytes.
    pub bytes: u64,
    /// MD5 hex digest (if computed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    /// Wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Number of retry attempts.
    pub retries: u32,
}

/// Upload outcome for a single file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum UploadStatus {
    Uploaded,
    Skipped,
    Resumed,
    Failed(String),
    DryRun,
}

/// Progress update during an upload.
#[derive(Debug, Clone)]
pub struct UploadProgress {
    /// Item identifier.
    pub identifier: String,
    /// Remote filename (S3 key).
    pub key: String,
    /// Bytes sent so far.
    pub bytes_sent: u64,
    /// Total file size in bytes.
    pub total_bytes: u64,
    /// Current upload phase.
    pub status: UploadProgressStatus,
}

/// Current phase of an individual file upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadProgressStatus {
    /// File list and total size are now known for the item.
    ///
    /// Emitted once per item after directory expansion and size computation,
    /// before any file uploads begin. Mirrors `DownloadStatus::Enumerated`.
    Enumerated {
        /// Number of files that will be uploaded.
        files_count: usize,
        /// Total bytes across all files (0 when size is unknown).
        bytes_total: u64,
    },
    Verifying,
    Uploading,
    /// Retrying after a non-503 error (network error, server error, etc.).
    Retrying,
    WaitingRateLimit,
    Complete,
    Skipped,
    Resumed,
    /// A file upload failed. Contains the sanitized error message.
    Failed(String),
}

/// Progress callback type for upload operations.
///
/// Wrapping in `Arc` allows the callback to be shared across async tasks
/// and to satisfy the `'static` bound required by `reqwest::Body::wrap_stream()`
/// without resorting to `unsafe` lifetime transmutes.
pub type ProgressCallback = Arc<dyn Fn(UploadProgress) + Send + Sync>;

/// Information about an in-progress multipart upload (from S3 list-uploads).
#[derive(Debug, Clone, Serialize)]
pub struct MultipartUploadInfo {
    /// Remote filename (S3 key).
    pub key: String,
    /// Server-assigned upload ID.
    pub upload_id: String,
    /// ISO 8601 timestamp when the upload was initiated.
    pub initiated: String,
}

/// Information about a completed part (from S3 list-parts).
#[derive(Debug, Clone)]
pub struct PartInfo {
    /// 1-based part number.
    pub part_number: u32,
    /// ETag returned by S3 for this part (includes quotes).
    pub etag: String,
    /// Size in bytes.
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_opts_defaults() {
        let opts = UploadOpts::default();
        assert!(opts.verify);
        assert!(opts.checksum);
        assert!(!opts.no_derive);
        assert!(!opts.no_backup);
        assert_eq!(opts.retries, 10);
        assert_eq!(opts.retry_sleep, Duration::from_secs(30));
        assert!(opts.metadata.is_empty());
    }

    #[test]
    fn upload_result_serializes_to_json() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: Some("abc123".into()),
            elapsed_ms: 500,
            retries: 0,
        };
        let val: serde_json::Value = serde_json::to_value(&result).unwrap();
        // Adjacently-tagged + flatten: "status" appears at top level
        assert_eq!(val["identifier"], "test-item");
        assert_eq!(val["status"], "uploaded");
        assert_eq!(val["bytes"], 1024);
        // Unit variants have no "detail" key
        assert!(val.get("detail").is_none());
    }

    #[test]
    fn upload_result_failed_includes_detail() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Failed("connection reset".into()),
            bytes: 0,
            md5: None,
            elapsed_ms: 100,
            retries: 3,
        };
        let val: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(val["status"], "failed");
        assert_eq!(val["detail"], "connection reset");
        assert_eq!(val["retries"], 3);
    }

    #[test]
    fn upload_result_skips_none_md5() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Uploaded,
            bytes: 1024,
            md5: None,
            elapsed_ms: 500,
            retries: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("md5"));
    }

    #[test]
    fn upload_result_dry_run_json_shape() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::DryRun,
            bytes: 4096,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        };
        let val: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(val["status"], "dry_run");
        assert!(val.get("detail").is_none());
    }

    #[test]
    fn upload_result_skipped_json_shape() {
        let result = UploadResult {
            identifier: "test-item".into(),
            key: "file.pdf".into(),
            status: UploadStatus::Skipped,
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        };
        let val: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(val["status"], "skipped");
        assert!(val.get("detail").is_none());
    }

    #[test]
    fn builder_defaults_match_direct_default() {
        let from_builder = UploadOptsBuilder::new().build();
        let from_default = UploadOpts::default();
        assert_eq!(from_builder.verify, from_default.verify);
        assert_eq!(from_builder.retries, from_default.retries);
        assert_eq!(from_builder.dry_run, from_default.dry_run);
        assert!(from_builder.metadata.is_empty());
    }

    #[test]
    fn builder_chained_setters() {
        let opts = UploadOptsBuilder::new()
            .verify(false)
            .dry_run(true)
            .retries(5)
            .metadata(vec![("key".into(), "val".into())])
            .build();
        assert!(!opts.verify);
        assert!(opts.dry_run);
        assert_eq!(opts.retries, 5);
        assert_eq!(opts.metadata.len(), 1);
    }

    #[test]
    fn multipart_upload_info_debug() {
        let info = MultipartUploadInfo {
            key: "file.zip".into(),
            upload_id: "abc123".into(),
            initiated: "2026-03-06T12:00:00Z".into(),
        };
        assert_eq!(info.key, "file.zip");
        assert_eq!(info.upload_id, "abc123");
        let dbg = format!("{info:?}");
        assert!(dbg.contains("file.zip"));
    }

    #[test]
    fn part_info_debug() {
        let part = PartInfo {
            part_number: 1,
            etag: "\"abc123\"".into(),
            size: 104857600,
        };
        assert_eq!(part.part_number, 1);
        assert_eq!(part.size, 104857600);
    }

    #[test]
    fn multipart_upload_info_serializes() {
        let info = MultipartUploadInfo {
            key: "file.zip".into(),
            upload_id: "abc123".into(),
            initiated: "2026-03-06T12:00:00Z".into(),
        };
        let val: serde_json::Value = serde_json::to_value(&info).unwrap();
        assert_eq!(val["key"], "file.zip");
        assert_eq!(val["upload_id"], "abc123");
        assert_eq!(val["initiated"], "2026-03-06T12:00:00Z");
    }
}
