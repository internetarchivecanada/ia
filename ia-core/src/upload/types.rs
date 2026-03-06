use serde::Serialize;
use std::collections::HashMap;
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
    pub skip_existing: bool,
    /// Pre-computed MD5 checksums keyed by filename.
    pub checksums: Option<HashMap<String, String>>,
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
            skip_existing: false,
            checksums: None,
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

/// Result of a single file upload.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub identifier: String,
    pub key: String,
    #[serde(flatten)]
    pub status: UploadStatus,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    pub elapsed_ms: u64,
    pub retries: u32,
}

/// Upload outcome for a single file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum UploadStatus {
    Uploaded,
    Skipped,
    Failed(String),
    DryRun,
}

/// Progress update during an upload.
#[derive(Debug, Clone)]
pub struct UploadProgress {
    pub identifier: String,
    pub key: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub status: UploadProgressStatus,
}

/// Current phase of an individual file upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadProgressStatus {
    Verifying,
    Uploading,
    WaitingRateLimit,
    Complete,
    Skipped,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_opts_defaults() {
        let opts = UploadOpts::default();
        assert!(opts.verify);
        assert!(!opts.skip_existing);
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
}
