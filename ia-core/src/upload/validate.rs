use crate::error::IaError;
use crate::IaClient;
use std::path::Path;

/// Validate an IA identifier.
///
/// Rules: 3-100 chars, `[a-zA-Z0-9._-@]`, must start with alphanumeric or `@`.
///
/// # Examples
///
/// ```
/// use ia_core::upload::validate::validate_identifier;
///
/// assert!(validate_identifier("nasa").is_ok());
/// assert!(validate_identifier("@username").is_ok());
/// assert!(validate_identifier("ab").is_err()); // too short
/// assert!(validate_identifier("has space").is_err()); // invalid char
/// ```
pub fn validate_identifier(id: &str) -> Result<(), IaError> {
    if id.is_empty() || id.len() < 3 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at least 3 characters".into(),
        });
    }
    if id.len() > 100 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at most 100 characters".into(),
        });
    }

    let Some(first) = id.chars().next() else {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "identifier is empty".into(),
        });
    };
    if !first.is_ascii_alphanumeric() && first != '@' {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("must start with alphanumeric or '@', got '{first}'"),
        });
    }

    if let Some(bad) = id
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '@'))
    {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("contains invalid character '{bad}'"),
        });
    }

    Ok(())
}

/// Validate that required metadata fields are present.
///
/// Required fields: `mediatype`, `collection`. Values must be non-empty.
///
/// # Examples
///
/// ```
/// use ia_core::upload::validate::validate_required_metadata;
///
/// let meta = vec![
///     ("mediatype".into(), "texts".into()),
///     ("collection".into(), "test_collection".into()),
/// ];
/// assert!(validate_required_metadata(&meta).is_ok());
/// ```
pub fn validate_required_metadata(metadata: &[(String, String)]) -> Result<(), IaError> {
    let has = |field: &str| metadata.iter().any(|(k, v)| k == field && !v.is_empty());

    if !has("mediatype") {
        return Err(IaError::MissingRequiredMetadata {
            field: "mediatype".into(),
        });
    }
    if !has("collection") {
        return Err(IaError::MissingRequiredMetadata {
            field: "collection".into(),
        });
    }

    Ok(())
}

/// Check that the named collections exist on archive.org.
///
/// Makes GET /metadata/{collection} requests. Returns an error listing
/// all collections that were not found.
///
/// This is a pre-flight validation: better to fail early than to upload
/// files and discover the collection doesn't exist.
pub async fn check_collections(client: &IaClient, collections: &[&str]) -> Result<(), IaError> {
    let mut not_found = Vec::new();

    for collection in collections {
        match client.item_exists(collection).await {
            Ok(true) => {} // exists
            Ok(false) => not_found.push((*collection).to_string()),
            Err(e) => {
                // Log but don't fail — collection check is best-effort
                tracing::warn!("failed to check collection {collection}: {e}");
            }
        }
    }

    if not_found.is_empty() {
        Ok(())
    } else {
        Err(IaError::CollectionNotFound {
            collection: not_found.join(", "),
        })
    }
}

/// Check that a file exists and is not a symlink.
///
/// Returns `Err(SymlinkSkipped)` for symlinks, `Err(Io)` if file doesn't exist.
pub fn validate_file(path: &Path) -> Result<(), IaError> {
    let symlink_meta = std::fs::symlink_metadata(path)?;
    if symlink_meta.file_type().is_symlink() {
        return Err(IaError::SymlinkSkipped {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_identifier tests --

    #[test]
    fn valid_identifiers() {
        assert!(validate_identifier("nasa").is_ok());
        assert!(validate_identifier("my-item-123").is_ok());
        assert!(validate_identifier("test.item").is_ok());
        assert!(validate_identifier("a_b_c").is_ok());
        assert!(validate_identifier("abc").is_ok());
        assert!(validate_identifier("@username").is_ok());
    }

    #[test]
    fn invalid_identifier_too_short() {
        assert!(validate_identifier("ab").is_err());
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn invalid_identifier_too_long() {
        let long = "a".repeat(101);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn invalid_identifier_bad_chars() {
        assert!(validate_identifier("has space").is_err());
        assert!(validate_identifier("has!bang").is_err());
        assert!(validate_identifier("has#hash").is_err());
    }

    #[test]
    fn invalid_identifier_bad_start() {
        assert!(validate_identifier(".dotstart").is_err());
        assert!(validate_identifier("_understart").is_err());
        assert!(validate_identifier("-dashstart").is_err());
    }

    #[test]
    fn valid_identifier_at_max_length() {
        let exactly_100 = "a".repeat(100);
        assert!(validate_identifier(&exactly_100).is_ok());
    }

    // -- validate_required_metadata tests --

    #[test]
    fn valid_metadata_has_required_fields() {
        let meta = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "test_collection".into()),
        ];
        assert!(validate_required_metadata(&meta).is_ok());
    }

    #[test]
    fn missing_mediatype() {
        let meta = vec![("collection".into(), "test_collection".into())];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "mediatype"));
    }

    #[test]
    fn missing_collection() {
        let meta = vec![("mediatype".into(), "texts".into())];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "collection"));
    }

    #[test]
    fn empty_mediatype_counts_as_missing() {
        let meta = vec![
            ("mediatype".into(), "".into()),
            ("collection".into(), "test_collection".into()),
        ];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "mediatype"));
    }

    #[test]
    fn empty_collection_counts_as_missing() {
        let meta = vec![
            ("mediatype".into(), "texts".into()),
            ("collection".into(), "".into()),
        ];
        let err = validate_required_metadata(&meta).unwrap_err();
        assert!(matches!(err, IaError::MissingRequiredMetadata { field } if field == "collection"));
    }

    // -- validate_file tests --

    #[test]
    fn validate_file_nonexistent() {
        let result = validate_file(Path::new("/tmp/ia-test-nonexistent-file-xyz"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IaError::Io(_)));
    }

    #[test]
    fn validate_file_regular_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_file(tmp.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn validate_file_symlink() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let link_path = dir.path().join("link");
        std::os::unix::fs::symlink(tmp.path(), &link_path).unwrap();
        let result = validate_file(&link_path);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IaError::SymlinkSkipped { .. }
        ));
    }
}
