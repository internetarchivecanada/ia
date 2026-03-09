use std::collections::BTreeMap;
use std::path::PathBuf;

use futures::stream::{self, StreamExt};

use crate::error::{IaError, Result};
use crate::spreadsheet::SpreadsheetRecord;
use crate::upload::item::upload_item;
use crate::upload::types::{UploadOpts, UploadProgress, UploadResult};
use crate::upload::validate::{validate_file, validate_identifier};
use crate::IaClient;

/// A group of files and metadata for a single IA item.
#[derive(Debug)]
pub struct ItemGroup {
    pub identifier: String,
    pub metadata: Vec<(String, String)>,
    pub files: Vec<PathBuf>,
}

/// Batch upload items from spreadsheet records.
///
/// Groups records by identifier, validates upfront, then uploads items
/// concurrently (controlled by `concurrency` parameter).
///
/// Each record must have a `file` field containing the path to upload.
/// All other fields become metadata key-value pairs. Records sharing the
/// same identifier are grouped into a single item upload with multiple files.
///
/// # Errors
///
/// Returns `IaError::EmptyUpload` if `records` is empty.
/// Returns `IaError::Config` if any record is missing the `file` field,
/// or if a referenced file does not exist or is a symlink.
/// Returns `IaError::InvalidIdentifier` if any identifier is malformed.
pub async fn upload_batch(
    client: &IaClient,
    records: Vec<SpreadsheetRecord>,
    opts: &UploadOpts,
    concurrency: usize,
    progress: Option<&(dyn Fn(UploadProgress) + Send + Sync)>,
) -> Result<Vec<UploadResult>> {
    if records.is_empty() {
        return Err(IaError::EmptyUpload);
    }

    // 1. Group records by identifier
    let groups = group_records(records)?;

    // 2. Pre-validate all groups
    validate_groups(&groups)?;

    // 3. Upload items concurrently
    // Return (identifier, Result) so we can attribute failures to specific items.
    let results: Vec<(String, Result<Vec<UploadResult>>)> = stream::iter(groups)
        .map(|group| async move {
            let id = group.identifier.clone();
            // Build per-item opts: spreadsheet metadata overrides CLI metadata for same keys
            let mut item_opts = opts.clone();
            for (key, value) in group.metadata {
                if let Some(existing) = item_opts.metadata.iter_mut().find(|(k, _)| k == &key) {
                    existing.1 = value;
                } else {
                    item_opts.metadata.push((key, value));
                }
            }

            let result =
                upload_item(client, &group.identifier, &group.files, &item_opts, progress).await;
            (id, result)
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    // 4. Flatten results — collect successes AND failures
    let mut all_results = Vec::new();
    let mut errors: Vec<(String, IaError)> = Vec::new();
    for (identifier, result) in results {
        match result {
            Ok(item_results) => all_results.extend(item_results),
            Err(e) => errors.push((identifier, e)),
        }
    }

    // Convert errors to Failed results so callers can attribute them
    for (identifier, err) in &errors {
        all_results.push(UploadResult {
            identifier: identifier.clone(),
            key: String::new(),
            status: crate::upload::types::UploadStatus::Failed(err.to_string()),
            bytes: 0,
            md5: None,
            elapsed_ms: 0,
            retries: 0,
        });
    }

    // If ALL items failed and we have no real results, return the first error
    if all_results.iter().all(|r| matches!(r.status, crate::upload::types::UploadStatus::Failed(_))) {
        if let Some((_, first_err)) = errors.into_iter().next() {
            return Err(first_err);
        }
    }

    Ok(all_results)
}

/// Group spreadsheet records by identifier, extracting file paths and metadata.
pub fn group_records(records: Vec<SpreadsheetRecord>) -> Result<Vec<ItemGroup>> {
    // Use BTreeMap for deterministic ordering by identifier
    let mut map: BTreeMap<String, ItemGroup> = BTreeMap::new();

    for (identifier, mut fields) in records {
        let file_path = fields
            .remove("file")
            .ok_or_else(|| IaError::Config(format!(
                "record for identifier '{identifier}' is missing required 'file' field"
            )))?;

        // Strip REMOTE_NAME from metadata — it's a template column, not an IA metadata field
        fields.remove("REMOTE_NAME");

        let metadata: Vec<(String, String)> = fields.into_iter().collect();

        let group = map
            .entry(identifier.clone())
            .or_insert_with(|| ItemGroup {
                identifier,
                metadata: Vec::new(),
                files: Vec::new(),
            });

        // Use metadata from the first record for this identifier
        if group.metadata.is_empty() {
            group.metadata = metadata;
        }

        group.files.push(PathBuf::from(file_path));
    }

    Ok(map.into_values().collect())
}

/// Validate all item groups upfront before uploading.
///
/// Checks identifiers and file existence/type. Collects all errors
/// and reports them together.
pub fn validate_groups(groups: &[ItemGroup]) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for group in groups {
        if let Err(e) = validate_identifier(&group.identifier) {
            errors.push(e.to_string());
        }

        // Validate required metadata per group
        if !group.metadata.is_empty() {
            if let Err(e) = crate::upload::validate::validate_required_metadata(&group.metadata) {
                errors.push(format!("{}: {e}", group.identifier));
            }
        }

        for file in &group.files {
            if let Err(e) = validate_file(file) {
                errors.push(format!("{}: {e}", file.display()));
            }
        }
    }

    if !errors.is_empty() {
        return Err(IaError::Config(format!(
            "batch validation failed:\n  {}",
            errors.join("\n  ")
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn record(
        id: &str,
        file: &str,
        extra: &[(&str, &str)],
    ) -> SpreadsheetRecord {
        let mut fields = HashMap::new();
        fields.insert("file".into(), file.into());
        for (k, v) in extra {
            fields.insert((*k).into(), (*v).into());
        }
        (id.into(), fields)
    }

    #[test]
    fn group_records_single_item() {
        let records = vec![record("item-1", "/tmp/a.txt", &[("mediatype", "texts")])];
        let groups = group_records(records).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].identifier, "item-1");
        assert_eq!(groups[0].files, vec![PathBuf::from("/tmp/a.txt")]);
    }

    #[test]
    fn group_records_multiple_files_same_id() {
        let records = vec![
            record("item-1", "/tmp/a.txt", &[("mediatype", "texts")]),
            record("item-1", "/tmp/b.txt", &[("mediatype", "texts")]),
        ];
        let groups = group_records(records).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
    }

    #[test]
    fn group_records_uses_first_metadata() {
        let records = vec![
            record("item-1", "/tmp/a.txt", &[("title", "First")]),
            record("item-1", "/tmp/b.txt", &[("title", "Second")]),
        ];
        let groups = group_records(records).unwrap();
        let title = groups[0]
            .metadata
            .iter()
            .find(|(k, _)| k == "title")
            .map(|(_, v)| v.as_str());
        assert_eq!(title, Some("First"));
    }

    #[test]
    fn group_records_missing_file_field() {
        let mut fields = HashMap::new();
        fields.insert("mediatype".into(), "texts".into());
        let records = vec![("item-1".into(), fields)];
        let err = group_records(records).unwrap_err();
        assert!(matches!(err, IaError::Config(_)));
        assert!(err.to_string().contains("missing required 'file' field"));
    }

    #[test]
    fn group_records_removes_file_from_metadata() {
        let records = vec![record("item-1", "/tmp/a.txt", &[("mediatype", "texts")])];
        let groups = group_records(records).unwrap();
        assert!(!groups[0].metadata.iter().any(|(k, _)| k == "file"));
    }

    #[test]
    fn group_records_deterministic_order() {
        let records = vec![
            record("zzz", "/tmp/z.txt", &[]),
            record("aaa", "/tmp/a.txt", &[]),
        ];
        let groups = group_records(records).unwrap();
        assert_eq!(groups[0].identifier, "aaa");
        assert_eq!(groups[1].identifier, "zzz");
    }

    #[test]
    fn validate_groups_checks_required_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let groups = vec![ItemGroup {
            identifier: "test-item".into(),
            metadata: vec![("title".into(), "My Item".into())], // missing mediatype + collection
            files: vec![file],
        }];
        let err = validate_groups(&groups).unwrap_err();
        assert!(err.to_string().contains("mediatype"));
    }

    #[test]
    fn validate_groups_passes_with_valid_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let groups = vec![ItemGroup {
            identifier: "test-item".into(),
            metadata: vec![
                ("mediatype".into(), "texts".into()),
                ("collection".into(), "test_collection".into()),
            ],
            files: vec![file],
        }];
        validate_groups(&groups).unwrap();
    }

    #[test]
    fn validate_groups_skips_empty_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        // No metadata — should pass (no validation triggered)
        let groups = vec![ItemGroup {
            identifier: "test-item".into(),
            metadata: vec![],
            files: vec![file],
        }];
        validate_groups(&groups).unwrap();
    }
}
