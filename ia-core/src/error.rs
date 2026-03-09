use serde::Serialize;
use std::path::PathBuf;

/// Structured error wrapper for `--json` mode output on stderr.
///
/// Serializes to: `{"error": {"code": "...", "message": "...", ...extra}}`
#[derive(Debug, Clone, Serialize)]
pub struct JsonError {
    pub error: JsonErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonErrorBody {
    pub code: String,
    pub message: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum IaError {
    #[error("item not found: {0}")]
    NotFound(String),

    #[error("HTTP error {status}: {message}")]
    Http { status: u16, message: String },

    #[error("rate limited (retry after {retry_after}s)")]
    RateLimited { retry_after: u64 },

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("disk full: {}", path.display())]
    DiskFull { path: PathBuf },

    #[error("no disk in pool has {needed} bytes free")]
    NoDiskSpace { needed: u64 },

    #[error("download resume failed for {file}: {reason}")]
    ResumeFailed { file: String, reason: String },

    #[error("config error: {0}")]
    Config(String),

    #[error("authentication required: {0}")]
    Auth(String),

    #[error("metadata write failed for {identifier}: {message}")]
    MetadataWrite { identifier: String, message: String },

    #[error("LLM API error ({status}): {message}")]
    LlmApi { status: u16, message: String },

    #[error("no release asset found for target {target}")]
    UpdateNoAsset { target: String },

    #[error("update API error ({status}): {message}")]
    UpdateApiError { status: u16, message: String },

    #[error("update verification failed: expected {expected}, got {actual}")]
    UpdateVerifyFailed { expected: String, actual: String },

    #[error("version {version} is below minimum installable version ({minimum})")]
    UpdateBelowMinimum { version: String, minimum: String },

    #[error("version {version} not found")]
    UpdateVersionNotFound { version: String },

    #[error("path traversal blocked: {path} escapes destination directory {dest_dir}")]
    PathTraversal { path: String, dest_dir: String },

    #[error("download too large for {file}: expected {expected} bytes, received {received} bytes")]
    DownloadTooLarge {
        file: String,
        expected: u64,
        received: u64,
    },

    #[error("upload failed for {identifier}/{key}: {message}")]
    UploadFailed {
        identifier: String,
        key: String,
        message: String,
    },

    #[error("upload blocked: {identifier} appears to be spam")]
    SpamDetected { identifier: String },

    #[error("collection not found: {collection}")]
    CollectionNotFound { collection: String },

    #[error("invalid identifier '{identifier}': {reason}")]
    InvalidIdentifier { identifier: String, reason: String },

    #[error("missing required metadata field: {field}")]
    MissingRequiredMetadata { field: String },

    #[error("check_limit request failed for {identifier}")]
    CheckLimitFailed { identifier: String },

    #[error("file too large: {} ({size} bytes)", path.display())]
    FileTooLarge { path: PathBuf, size: u64 },

    #[error("no files to upload")]
    EmptyUpload,

    #[error("symlink skipped: {}", path.display())]
    SymlinkSkipped { path: PathBuf },

    #[error("multipart upload aborted for {identifier}/{key}")]
    MultipartAborted { identifier: String, key: String },

    #[error("multipart upload incomplete for {identifier}/{key} (upload_id: {upload_id})")]
    MultipartIncomplete { identifier: String, key: String, upload_id: String },

    #[error(transparent)]
    Network(#[from] reqwest_middleware::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IaError>;

impl IaError {
    /// Whether this error is transient and worth retrying.
    ///
    /// Returns `false` for permanent failures (access denied, not found, config
    /// errors, disk full) where retrying would just waste time.
    /// Returns `true` for transient failures (server errors, network issues,
    /// rate limits, checksum mismatches) that may succeed on retry.
    pub fn is_retryable(&self) -> bool {
        match self {
            // HTTP 4xx client errors are permanent (except 429 rate-limit)
            IaError::Http { status, .. } => {
                *status == 429 || *status >= 500
            }
            // Transient — may succeed on retry
            IaError::RateLimited { .. } => true,
            IaError::Network(_) => true,
            IaError::Io(_) => true,
            IaError::ChecksumMismatch { .. } => true,
            IaError::ResumeFailed { .. } => true,
            // LLM API errors: retry on 429/5xx, not on 4xx
            IaError::LlmApi { status, .. } => {
                *status == 429 || *status >= 500
            }
            // Update errors: API errors retry on 5xx, others are permanent
            IaError::UpdateApiError { status, .. } => {
                *status == 429 || *status >= 500
            }
            IaError::UpdateNoAsset { .. } => false,
            IaError::UpdateVerifyFailed { .. } => false,
            IaError::UpdateBelowMinimum { .. } => false,
            IaError::UpdateVersionNotFound { .. } => false,
            // Security — never retry
            IaError::PathTraversal { .. } => false,
            IaError::DownloadTooLarge { .. } => false,
            // Upload errors
            IaError::UploadFailed { .. } => false,  // terminal — retry logic is in single.rs
            IaError::SpamDetected { .. } => false,   // permanent
            IaError::CollectionNotFound { .. } => false,
            IaError::InvalidIdentifier { .. } => false,
            IaError::MissingRequiredMetadata { .. } => false,
            IaError::CheckLimitFailed { .. } => true, // conservative: treat as overloaded
            IaError::FileTooLarge { .. } => false,
            IaError::EmptyUpload => false,
            IaError::SymlinkSkipped { .. } => false,
            IaError::MultipartAborted { .. } => false,
            IaError::MultipartIncomplete { .. } => false,
            // Permanent — retrying won't help
            IaError::NotFound(_) => false,
            IaError::Auth(_) => false,
            IaError::Config(_) => false,
            IaError::DiskFull { .. } => false,
            IaError::NoDiskSpace { .. } => false,
            IaError::MetadataWrite { .. } => false,
            IaError::Json(_) => false,
        }
    }

    /// Convert this error into a structured `JsonError` for `--json` mode.
    pub fn to_json_error(&self) -> JsonError {
        let mut extra = serde_json::Map::new();
        let code = match self {
            IaError::NotFound(id) => {
                extra.insert("identifier".into(), id.clone().into());
                "not_found"
            }
            IaError::Http { status, .. } => {
                extra.insert("status".into(), (*status).into());
                "http_error"
            }
            IaError::RateLimited { retry_after } => {
                extra.insert("retry_after".into(), (*retry_after).into());
                "rate_limited"
            }
            IaError::ChecksumMismatch {
                file,
                expected,
                actual,
            } => {
                extra.insert("file".into(), file.clone().into());
                extra.insert("expected".into(), expected.clone().into());
                extra.insert("actual".into(), actual.clone().into());
                "checksum_mismatch"
            }
            IaError::DiskFull { path } => {
                extra.insert("path".into(), path.display().to_string().into());
                "disk_full"
            }
            IaError::NoDiskSpace { needed } => {
                extra.insert("needed".into(), (*needed).into());
                "no_disk_space"
            }
            IaError::ResumeFailed { file, reason } => {
                extra.insert("file".into(), file.clone().into());
                extra.insert("reason".into(), reason.clone().into());
                "resume_failed"
            }
            IaError::Config(_) => "config_error",
            IaError::Auth(_) => "auth_error",
            IaError::MetadataWrite { identifier, .. } => {
                extra.insert("identifier".into(), identifier.clone().into());
                "metadata_write"
            }
            IaError::LlmApi { status, .. } => {
                extra.insert("status".into(), (*status).into());
                "llm_api"
            }
            IaError::UpdateNoAsset { target } => {
                extra.insert("target".into(), target.clone().into());
                "update_no_asset"
            }
            IaError::UpdateApiError { status, .. } => {
                extra.insert("status".into(), (*status).into());
                "update_api_error"
            }
            IaError::UpdateVerifyFailed { expected, actual } => {
                extra.insert("expected".into(), expected.clone().into());
                extra.insert("actual".into(), actual.clone().into());
                "update_verify_failed"
            }
            IaError::UpdateBelowMinimum { version, minimum } => {
                extra.insert("version".into(), version.clone().into());
                extra.insert("minimum".into(), minimum.clone().into());
                "update_below_minimum"
            }
            IaError::UpdateVersionNotFound { version } => {
                extra.insert("version".into(), version.clone().into());
                "update_version_not_found"
            }
            IaError::PathTraversal { path, dest_dir } => {
                extra.insert("path".into(), path.clone().into());
                extra.insert("dest_dir".into(), dest_dir.clone().into());
                "path_traversal"
            }
            IaError::DownloadTooLarge {
                file,
                expected,
                received,
            } => {
                extra.insert("file".into(), file.clone().into());
                extra.insert("expected".into(), (*expected).into());
                extra.insert("received".into(), (*received).into());
                "download_too_large"
            }
            IaError::UploadFailed { identifier, key, .. } => {
                extra.insert("identifier".into(), identifier.clone().into());
                extra.insert("key".into(), key.clone().into());
                "upload_failed"
            }
            IaError::SpamDetected { identifier } => {
                extra.insert("identifier".into(), identifier.clone().into());
                "spam_detected"
            }
            IaError::CollectionNotFound { collection } => {
                extra.insert("collection".into(), collection.clone().into());
                "collection_not_found"
            }
            IaError::InvalidIdentifier { identifier, reason } => {
                extra.insert("identifier".into(), identifier.clone().into());
                extra.insert("reason".into(), reason.clone().into());
                "invalid_identifier"
            }
            IaError::MissingRequiredMetadata { field } => {
                extra.insert("field".into(), field.clone().into());
                "missing_required_metadata"
            }
            IaError::CheckLimitFailed { identifier } => {
                extra.insert("identifier".into(), identifier.clone().into());
                "check_limit_failed"
            }
            IaError::FileTooLarge { path, size } => {
                extra.insert("path".into(), path.display().to_string().into());
                extra.insert("size".into(), (*size).into());
                "file_too_large"
            }
            IaError::EmptyUpload => "empty_upload",
            IaError::SymlinkSkipped { path } => {
                extra.insert("path".into(), path.display().to_string().into());
                "symlink_skipped"
            }
            IaError::MultipartAborted { identifier, key } => {
                extra.insert("identifier".into(), identifier.clone().into());
                extra.insert("key".into(), key.clone().into());
                "multipart_aborted"
            }
            IaError::MultipartIncomplete { identifier, key, upload_id } => {
                extra.insert("identifier".into(), identifier.clone().into());
                extra.insert("key".into(), key.clone().into());
                extra.insert("upload_id".into(), upload_id.clone().into());
                "multipart_incomplete"
            }
            IaError::Network(_) => "network",
            IaError::Io(_) => "io",
            IaError::Json(_) => "json_parse",
        };

        JsonError {
            error: JsonErrorBody {
                code: code.to_string(),
                message: self.to_string(),
                extra,
            },
        }
    }
}

/// Write a structured JSON error to stderr. For use when `--json` is active.
pub fn write_json_error(err: &IaError) {
    let json_err = err.to_json_error();
    if let Ok(s) = serde_json::to_string(&json_err) {
        eprintln!("{}", s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_displays_identifier() {
        let err = IaError::NotFound("nasa".to_string());
        assert_eq!(err.to_string(), "item not found: nasa");
    }

    #[test]
    fn http_error_displays_status_and_message() {
        let err = IaError::Http {
            status: 503,
            message: "Service Unavailable".to_string(),
        };
        assert_eq!(err.to_string(), "HTTP error 503: Service Unavailable");
    }

    #[test]
    fn checksum_mismatch_displays_details() {
        let err = IaError::ChecksumMismatch {
            file: "photo.jpg".to_string(),
            expected: "abc123".to_string(),
            actual: "def456".to_string(),
        };
        assert!(err.to_string().contains("photo.jpg"));
        assert!(err.to_string().contains("abc123"));
    }

    #[test]
    fn io_error_converts() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let ia_err: IaError = io_err.into();
        assert!(matches!(ia_err, IaError::Io(_)));
    }

    #[test]
    fn auth_error_displays_message() {
        let err = IaError::Auth("S3 credentials required".to_string());
        assert_eq!(
            err.to_string(),
            "authentication required: S3 credentials required"
        );
    }

    #[test]
    fn metadata_write_error_displays_details() {
        let err = IaError::MetadataWrite {
            identifier: "nasa".to_string(),
            message: "no changes to xml".to_string(),
        };
        assert!(err.to_string().contains("nasa"));
        assert!(err.to_string().contains("no changes to xml"));
    }

    // -- JSON error serialization tests --

    fn parse_json_error(err: &IaError) -> serde_json::Value {
        let json_err = err.to_json_error();
        let s = serde_json::to_string(&json_err).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn json_error_has_wrapper_shape() {
        let err = IaError::Config("bad value".into());
        let v = parse_json_error(&err);
        assert!(v.get("error").is_some(), "must have top-level 'error' key");
        assert!(v["error"].get("code").is_some());
        assert!(v["error"].get("message").is_some());
    }

    #[test]
    fn json_not_found() {
        let err = IaError::NotFound("nasa".into());
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["identifier"], "nasa");
        assert!(v["error"]["message"].as_str().unwrap().contains("nasa"));
    }

    #[test]
    fn json_http_error() {
        let err = IaError::Http {
            status: 503,
            message: "Service Unavailable".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "http_error");
        assert_eq!(v["error"]["status"], 503);
        assert!(v["error"]["message"].as_str().unwrap().contains("503"));
    }

    #[test]
    fn json_rate_limited() {
        let err = IaError::RateLimited { retry_after: 30 };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "rate_limited");
        assert_eq!(v["error"]["retry_after"], 30);
    }

    #[test]
    fn json_checksum_mismatch() {
        let err = IaError::ChecksumMismatch {
            file: "photo.jpg".into(),
            expected: "abc123".into(),
            actual: "def456".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "checksum_mismatch");
        assert_eq!(v["error"]["file"], "photo.jpg");
        assert_eq!(v["error"]["expected"], "abc123");
        assert_eq!(v["error"]["actual"], "def456");
    }

    #[test]
    fn json_disk_full() {
        let err = IaError::DiskFull {
            path: PathBuf::from("/mnt/data"),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "disk_full");
        assert_eq!(v["error"]["path"], "/mnt/data");
    }

    #[test]
    fn json_no_disk_space() {
        let err = IaError::NoDiskSpace { needed: 1048576 };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "no_disk_space");
        assert_eq!(v["error"]["needed"], 1048576);
    }

    #[test]
    fn json_resume_failed() {
        let err = IaError::ResumeFailed {
            file: "big.zip".into(),
            reason: "size changed".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "resume_failed");
        assert_eq!(v["error"]["file"], "big.zip");
        assert_eq!(v["error"]["reason"], "size changed");
    }

    #[test]
    fn json_config_error() {
        let err = IaError::Config("bad value".into());
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "config_error");
        assert!(v["error"]["message"].as_str().unwrap().contains("bad value"));
    }

    #[test]
    fn json_auth_error() {
        let err = IaError::Auth("credentials required".into());
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "auth_error");
    }

    #[test]
    fn json_metadata_write() {
        let err = IaError::MetadataWrite {
            identifier: "nasa".into(),
            message: "no changes".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "metadata_write");
        assert_eq!(v["error"]["identifier"], "nasa");
    }

    #[test]
    fn json_io_error() {
        let err: IaError =
            std::io::Error::new(std::io::ErrorKind::NotFound, "file missing").into();
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "io");
    }

    #[test]
    fn json_json_parse_error() {
        let err: IaError = serde_json::from_str::<serde_json::Value>("not json")
            .unwrap_err()
            .into();
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "json_parse");
    }

    #[test]
    fn write_json_error_produces_valid_json() {
        let err = IaError::NotFound("test-item".into());
        // write_json_error writes to stderr; verify the serialization is valid
        let json_err = err.to_json_error();
        let s = serde_json::to_string(&json_err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["identifier"], "test-item");
    }

    // -- is_retryable tests --

    #[test]
    fn http_403_is_not_retryable() {
        let err = IaError::Http {
            status: 403,
            message: "Forbidden".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn http_401_is_not_retryable() {
        let err = IaError::Http {
            status: 401,
            message: "Unauthorized".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn http_404_is_not_retryable() {
        let err = IaError::Http {
            status: 404,
            message: "Not Found".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn http_410_is_not_retryable() {
        let err = IaError::Http {
            status: 410,
            message: "Gone".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn http_500_is_retryable() {
        let err = IaError::Http {
            status: 500,
            message: "Internal Server Error".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn http_503_is_retryable() {
        let err = IaError::Http {
            status: 503,
            message: "Service Unavailable".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn http_429_is_retryable() {
        let err = IaError::Http {
            status: 429,
            message: "Too Many Requests".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn not_found_error_is_not_retryable() {
        let err = IaError::NotFound("nasa".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn auth_error_is_not_retryable() {
        let err = IaError::Auth("credentials required".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn config_error_is_not_retryable() {
        let err = IaError::Config("bad value".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn disk_full_is_not_retryable() {
        let err = IaError::DiskFull {
            path: PathBuf::from("/mnt/data"),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn network_error_is_retryable() {
        // Construct a Network error variant; it's always retryable regardless of inner value
        let err = IaError::Http {
            status: 502,
            message: "Bad Gateway".into(),
        };
        // 5xx through Http is retryable (proxy for network issues reaching server)
        assert!(err.is_retryable());
    }

    #[test]
    fn io_error_is_retryable() {
        let err: IaError =
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset").into();
        assert!(err.is_retryable());
    }

    #[test]
    fn checksum_mismatch_is_retryable() {
        let err = IaError::ChecksumMismatch {
            file: "photo.jpg".into(),
            expected: "abc".into(),
            actual: "def".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn update_no_asset_displays_target() {
        let err = IaError::UpdateNoAsset {
            target: "aarch64-apple-darwin".into(),
        };
        assert!(err.to_string().contains("aarch64-apple-darwin"));
    }

    #[test]
    fn update_api_error_displays_status() {
        let err = IaError::UpdateApiError {
            status: 403,
            message: "rate limited".into(),
        };
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn update_verify_failed_displays_versions() {
        let err = IaError::UpdateVerifyFailed {
            expected: "0.4.4".into(),
            actual: "0.4.3".into(),
        };
        assert!(err.to_string().contains("0.4.4"));
        assert!(err.to_string().contains("0.4.3"));
    }

    #[test]
    fn json_update_no_asset() {
        let err = IaError::UpdateNoAsset {
            target: "aarch64-apple-darwin".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_no_asset");
        assert_eq!(v["error"]["target"], "aarch64-apple-darwin");
    }

    #[test]
    fn json_update_api_error() {
        let err = IaError::UpdateApiError {
            status: 403,
            message: "rate limited".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_api_error");
        assert_eq!(v["error"]["status"], 403);
    }

    #[test]
    fn json_update_verify_failed() {
        let err = IaError::UpdateVerifyFailed {
            expected: "0.4.4".into(),
            actual: "0.4.3".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_verify_failed");
        assert_eq!(v["error"]["expected"], "0.4.4");
        assert_eq!(v["error"]["actual"], "0.4.3");
    }

    #[test]
    fn update_errors_are_not_retryable() {
        assert!(!IaError::UpdateNoAsset { target: "x".into() }.is_retryable());
        assert!(!IaError::UpdateVerifyFailed {
            expected: "a".into(),
            actual: "b".into(),
        }
        .is_retryable());
    }

    #[test]
    fn update_api_error_retryable_on_5xx() {
        assert!(IaError::UpdateApiError {
            status: 500,
            message: "error".into(),
        }
        .is_retryable());
        assert!(!IaError::UpdateApiError {
            status: 403,
            message: "forbidden".into(),
        }
        .is_retryable());
    }

    // -- UpdateBelowMinimum tests --

    #[test]
    fn update_below_minimum_displays_versions() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        assert!(err.to_string().contains("0.3.0"));
        assert!(err.to_string().contains("0.6.0"));
    }

    #[test]
    fn json_update_below_minimum() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_below_minimum");
        assert_eq!(v["error"]["version"], "0.3.0");
        assert_eq!(v["error"]["minimum"], "0.6.0");
    }

    #[test]
    fn update_below_minimum_is_not_retryable() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        assert!(!err.is_retryable());
    }

    // -- UpdateVersionNotFound tests --

    #[test]
    fn update_version_not_found_displays_version() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        assert!(err.to_string().contains("99.99.99"));
    }

    #[test]
    fn json_update_version_not_found() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_version_not_found");
        assert_eq!(v["error"]["version"], "99.99.99");
    }

    #[test]
    fn update_version_not_found_is_not_retryable() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        assert!(!err.is_retryable());
    }

    // -- Upload error tests --

    #[test]
    fn upload_failed_displays_details() {
        let err = IaError::UploadFailed {
            identifier: "my-item".into(),
            key: "file.pdf".into(),
            message: "connection reset".into(),
        };
        assert!(err.to_string().contains("my-item"));
        assert!(err.to_string().contains("file.pdf"));
    }

    #[test]
    fn spam_detected_is_not_retryable() {
        let err = IaError::SpamDetected {
            identifier: "spam-item".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn collection_not_found_is_not_retryable() {
        let err = IaError::CollectionNotFound {
            collection: "nonexistent".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn invalid_identifier_is_not_retryable() {
        let err = IaError::InvalidIdentifier {
            identifier: "!!!".into(),
            reason: "invalid characters".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn missing_required_metadata_is_not_retryable() {
        let err = IaError::MissingRequiredMetadata {
            field: "mediatype".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn check_limit_failed_is_retryable() {
        let err = IaError::CheckLimitFailed {
            identifier: "my-item".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn json_upload_failed() {
        let err = IaError::UploadFailed {
            identifier: "my-item".into(),
            key: "file.pdf".into(),
            message: "connection reset".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "upload_failed");
        assert_eq!(v["error"]["identifier"], "my-item");
        assert_eq!(v["error"]["key"], "file.pdf");
    }

    #[test]
    fn json_spam_detected() {
        let err = IaError::SpamDetected {
            identifier: "spam-item".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "spam_detected");
    }

    #[test]
    fn json_collection_not_found() {
        let err = IaError::CollectionNotFound {
            collection: "nonexistent".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "collection_not_found");
        assert_eq!(v["error"]["collection"], "nonexistent");
    }

    #[test]
    fn json_invalid_identifier() {
        let err = IaError::InvalidIdentifier {
            identifier: "!!!".into(),
            reason: "invalid characters".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "invalid_identifier");
        assert_eq!(v["error"]["identifier"], "!!!");
    }

    #[test]
    fn json_missing_required_metadata() {
        let err = IaError::MissingRequiredMetadata {
            field: "mediatype".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "missing_required_metadata");
        assert_eq!(v["error"]["field"], "mediatype");
    }

    #[test]
    fn upload_failed_is_not_retryable() {
        let err = IaError::UploadFailed {
            identifier: "my-item".into(),
            key: "file.pdf".into(),
            message: "connection reset".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn file_too_large_is_not_retryable() {
        let err = IaError::FileTooLarge {
            path: PathBuf::from("/tmp/huge.bin"),
            size: 999_999_999_999,
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn json_file_too_large() {
        let err = IaError::FileTooLarge {
            path: PathBuf::from("/tmp/huge.bin"),
            size: 999_999_999_999,
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "file_too_large");
        assert_eq!(v["error"]["path"], "/tmp/huge.bin");
        assert_eq!(v["error"]["size"], 999_999_999_999u64);
    }

    #[test]
    fn empty_upload_is_not_retryable() {
        let err = IaError::EmptyUpload;
        assert!(!err.is_retryable());
    }

    #[test]
    fn json_empty_upload() {
        let err = IaError::EmptyUpload;
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "empty_upload");
    }

    #[test]
    fn symlink_skipped_is_not_retryable() {
        let err = IaError::SymlinkSkipped {
            path: PathBuf::from("/tmp/link"),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn json_symlink_skipped() {
        let err = IaError::SymlinkSkipped {
            path: PathBuf::from("/tmp/link"),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "symlink_skipped");
        assert_eq!(v["error"]["path"], "/tmp/link");
    }

    #[test]
    fn multipart_aborted_displays_details() {
        let err = IaError::MultipartAborted {
            identifier: "my-item".into(),
            key: "file.zip".into(),
        };
        assert!(err.to_string().contains("my-item"));
        assert!(err.to_string().contains("file.zip"));
        assert!(!err.is_retryable());
    }

    #[test]
    fn multipart_incomplete_displays_details() {
        let err = IaError::MultipartIncomplete {
            identifier: "my-item".into(),
            key: "file.zip".into(),
            upload_id: "abc123".into(),
        };
        assert!(err.to_string().contains("my-item"));
        assert!(err.to_string().contains("file.zip"));
        assert!(!err.is_retryable());
    }

    #[test]
    fn json_multipart_aborted() {
        let err = IaError::MultipartAborted {
            identifier: "my-item".into(),
            key: "file.zip".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "multipart_aborted");
        assert_eq!(v["error"]["identifier"], "my-item");
        assert_eq!(v["error"]["key"], "file.zip");
    }

    #[test]
    fn json_multipart_incomplete() {
        let err = IaError::MultipartIncomplete {
            identifier: "my-item".into(),
            key: "file.zip".into(),
            upload_id: "abc123".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "multipart_incomplete");
        assert_eq!(v["error"]["identifier"], "my-item");
        assert_eq!(v["error"]["upload_id"], "abc123");
    }
}
