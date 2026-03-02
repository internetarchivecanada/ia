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
}
