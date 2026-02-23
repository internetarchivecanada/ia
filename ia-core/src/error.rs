use std::path::PathBuf;

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

    #[error(transparent)]
    Network(#[from] reqwest_middleware::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IaError>;

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
}
