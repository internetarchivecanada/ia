use crate::error::IaError;
use crate::IaClient;
use std::path::Path;

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
