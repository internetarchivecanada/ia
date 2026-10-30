use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{self, StreamExt};

use crate::error::{IaError, Result};
use crate::identifier::validate_identifier;
use crate::metadata::write::{fill_positions, parse_indexed_key};
use crate::spreadsheet::SpreadsheetRecord;
use crate::upload::item::upload_item;
use crate::upload::template::REMOTE_NAME_COLUMN;
use crate::upload::types::{UploadOpts, UploadProgress, UploadResult};
use crate::upload::validate::validate_file;
use crate::IaClient;

/// Callback invoked after each file result is produced (for streaming joblog writes).
type OnResultCallback = Arc<dyn Fn(&UploadResult) + Send + Sync>;

/// A group of files and metadata for a single IA item.
#[derive(Debug)]
#[non_exhaustive]
pub struct ItemGroup {
    pub identifier: String,
    pub metadata: Vec<(String, String)>,
    pub files: Vec<PathBuf>,
    /// The remote name of each file in `files`, from the row's
    /// `REMOTE_NAME` column; `None` keeps the file's usual name.
    pub remote_names: Vec<Option<String>>,
}

impl ItemGroup {
    /// The upload options for this item: `base` with the group's metadata
    /// merged in. A field the spreadsheet names replaces every value `base`
    /// has for it, as the Python client does; fields it does not name are
    /// kept (#97). Remote names come from the rows only, one per file in
    /// `remote_names` (#98); `base.remote_name` and `base.remote_names` are
    /// not used, so a caller's name is never given to every item of a
    /// batch.
    #[must_use]
    pub fn item_opts(&self, base: &UploadOpts) -> UploadOpts {
        let mut opts = base.clone();
        let named: HashSet<&str> = self.metadata.iter().map(|(k, _)| k.as_str()).collect();
        opts.metadata.retain(|(k, _)| !named.contains(k.as_str()));
        opts.metadata.extend(self.metadata.iter().cloned());
        opts.remote_name = None;
        opts.remote_names = self
            .files
            .iter()
            .zip(&self.remote_names)
            .filter_map(|(file, name)| Some((file.clone(), name.clone()?)))
            .collect();
        opts
    }
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
    progress: Option<Arc<dyn Fn(UploadProgress) + Send + Sync>>,
    skip_set: Option<Arc<HashSet<(String, String)>>>,
    on_result: Option<OnResultCallback>,
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
        .map(|group| {
            let progress = progress.clone();
            let skip = skip_set.clone();
            let on_res = on_result.clone();
            async move {
                let id = group.identifier.clone();
                let item_opts = group.item_opts(opts);

                let result = upload_item(
                    client,
                    &group.identifier,
                    &group.files,
                    &item_opts,
                    progress,
                    skip.as_deref(),
                    on_res,
                    1, // sequential within batch items; concurrency is across items
                    None,
                )
                .await;
                (id, result)
            }
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
    if all_results
        .iter()
        .all(|r| matches!(r.status, crate::upload::types::UploadStatus::Failed(_)))
    {
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
        let file_path = fields.remove("file").ok_or_else(|| {
            IaError::Config(format!(
                "record for identifier '{identifier}' is missing required 'file' field"
            ))
        })?;

        // The template's REMOTE_NAME column names the file on IA; it is
        // never item metadata. The spreadsheet readers lowercase column
        // names, and records built by a library caller may not, so the
        // match ignores case (#98).
        let mut remote_columns: Vec<String> = fields
            .keys()
            .filter(|k| k.eq_ignore_ascii_case(REMOTE_NAME_COLUMN))
            .cloned()
            .collect();
        remote_columns.sort();
        let remote_name = remote_columns
            .iter()
            .filter_map(|k| fields.remove(k))
            .filter(|v| !v.is_empty())
            .last();

        let metadata = row_metadata(fields);

        let group = map.entry(identifier.clone()).or_insert_with(|| ItemGroup {
            identifier,
            metadata: Vec::new(),
            files: Vec::new(),
            remote_names: Vec::new(),
        });

        // Use metadata from the first record for this identifier
        if group.metadata.is_empty() {
            group.metadata = metadata;
        }

        group.files.push(PathBuf::from(file_path));
        group.remote_names.push(remote_name);
    }

    Ok(map.into_values().collect())
}

/// Turn one row's metadata columns into `(field, value)` pairs.
///
/// Per field, a bare column (`subject`) is the field's list, and the
/// indexed columns (`subject[N]`) are placed in it with `fill_positions`,
/// the slot rule `ia metadata` gives repeated `-m` arguments (#65), so the
/// same syntax means the same thing in both places: position N is
/// overwritten, or the value appended when N is past the end, in index
/// order. `subject=a` with `subject[1]=b` gives `[a, b]`; `subject=a` with
/// `subject[0]=x` gives `[x]`. The readers do not keep column order, so the
/// columns are taken in column-name order, and of two columns for one
/// position the later in that order wins (`subject[0]` over
/// `subject[00]`). Every cell is a literal value. Fields come out in name
/// order. The header encoder numbers repeated keys in list order, so the
/// values become `x-archive-meta00-subject`, `x-archive-meta01-subject`,
/// ... (#97).
///
/// An empty value is no value, as the readers treat a blank cell: it
/// neither sends an empty header nor replaces a `-m` value.
fn row_metadata(fields: HashMap<String, String>) -> Vec<(String, String)> {
    let mut columns: Vec<(String, String)> =
        fields.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    columns.sort();

    let mut by_field: BTreeMap<String, FieldColumns> = BTreeMap::new();
    for (column, value) in columns {
        match parse_indexed_key(&column) {
            Some((field, idx)) => by_field
                .entry(field)
                .or_default()
                .indexed
                .push((idx, Some(value))),
            None => by_field.entry(column).or_default().bare = Some(value),
        }
    }

    by_field
        .into_iter()
        .flat_map(|(field, columns)| {
            let mut list: Vec<Option<String>> = columns.bare.into_iter().map(Some).collect();
            fill_positions(&mut list, columns.indexed);
            list.into_iter()
                .flatten()
                .map(move |value| (field.clone(), value))
        })
        .collect()
}

/// One field's columns in a row, for [`row_metadata`].
#[derive(Debug, Default)]
struct FieldColumns {
    /// The bare column's value (`subject`).
    bare: Option<String>,
    /// The indexed columns (`subject[N]`) as `(N, value)`, in column-name
    /// order.
    indexed: Vec<(usize, Option<String>)>,
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

        // Remote names are keyed by file, so one file named on two rows
        // must not be given two different names; only one would be used.
        let mut name_of: HashMap<&PathBuf, Option<&String>> = HashMap::new();
        for (file, name) in group.files.iter().zip(&group.remote_names) {
            match name_of.get(file) {
                Some(other) if *other != name.as_ref() => errors.push(format!(
                    "{}: {} is on two rows with different {REMOTE_NAME_COLUMN} values",
                    group.identifier,
                    file.display(),
                )),
                Some(_) => {}
                None => {
                    name_of.insert(file, name.as_ref());
                }
            }
        }

        for (file, remote_name) in group.files.iter().zip(&group.remote_names) {
            if let Err(e) = validate_file(file) {
                errors.push(format!("{}: {e}", file.display()));
            }
            // A directory row expands to its files, which a single remote
            // name cannot name.
            if remote_name.is_some() && file.is_dir() {
                errors.push(format!(
                    "{}: {REMOTE_NAME_COLUMN} cannot name a directory",
                    file.display()
                ));
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

    // -- group_records tests --

    fn record(id: &str, file: &str, extra: &[(&str, &str)]) -> SpreadsheetRecord {
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
    fn group_records_applies_the_set_rule_of_issue_65() {
        let records = vec![record(
            "item-1",
            "/tmp/a.txt",
            &[
                ("subject[10]", "ten"),
                ("subject[00]", "zero-b"),
                ("subject", "bare"),
                ("subject[2]", "two"),
                ("subject[0]", "zero-a"),
                ("title", "T"),
            ],
        )];
        let groups = group_records(records).unwrap();
        let values: Vec<(&str, &str)> = groups[0]
            .metadata
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            values,
            // The bare value is the list [bare]. Position 0 is overwritten,
            // by "subject[00]" and then "subject[0]" (column-name order, the
            // later wins); 2 and 10 are past the end and appended in index
            // order.
            vec![
                ("subject", "zero-a"),
                ("subject", "two"),
                ("subject", "ten"),
                ("title", "T"),
            ]
        );
    }

    #[test]
    fn item_opts_replaces_named_fields_and_keeps_the_rest() {
        let group = ItemGroup {
            identifier: "item-1".into(),
            metadata: vec![
                ("subject".into(), "alpha".into()),
                ("subject".into(), "beta".into()),
            ],
            files: vec![PathBuf::from("/tmp/a.txt")],
            remote_names: vec![None],
        };
        let base = UploadOpts {
            metadata: vec![
                ("subject".into(), "cli".into()),
                ("creator".into(), "C".into()),
            ],
            ..Default::default()
        };
        let opts = group.item_opts(&base);
        assert_eq!(
            opts.metadata,
            vec![
                ("creator".to_string(), "C".to_string()),
                ("subject".to_string(), "alpha".to_string()),
                ("subject".to_string(), "beta".to_string()),
            ]
        );
    }

    #[test]
    fn group_records_takes_the_remote_name_column_in_any_case_out_of_metadata() {
        let records = vec![
            record("item-1", "/tmp/a.txt", &[("remote_name", "lower.txt")]),
            record("item-2", "/tmp/b.txt", &[("REMOTE_NAME", "upper.txt")]),
            record("item-3", "/tmp/c.txt", &[("title", "T")]),
        ];
        let groups = group_records(records).unwrap();
        let names: Vec<Option<&str>> = groups
            .iter()
            .map(|g| g.remote_names[0].as_deref())
            .collect();
        assert_eq!(names, vec![Some("lower.txt"), Some("upper.txt"), None]);
        for group in &groups {
            assert!(
                !group
                    .metadata
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("remote_name")),
                "{:?}",
                group.metadata
            );
        }
    }

    #[test]
    fn an_empty_value_does_not_replace_the_command_line_value() {
        let records = vec![record(
            "item-1",
            "/tmp/a.txt",
            &[("collection", ""), ("subject[0]", ""), ("title", "T")],
        )];
        let groups = group_records(records).unwrap();
        assert_eq!(
            groups[0].metadata,
            vec![("title".to_string(), "T".to_string())]
        );
        let base = UploadOpts {
            metadata: vec![("collection".into(), "cli-coll".into())],
            ..Default::default()
        };
        assert_eq!(
            groups[0].item_opts(&base).metadata,
            vec![
                ("collection".to_string(), "cli-coll".to_string()),
                ("title".to_string(), "T".to_string()),
            ]
        );
    }

    #[test]
    fn item_opts_takes_the_remote_names_from_the_rows_only() {
        let records = vec![
            record("item-1", "/tmp/a.txt", &[("remote_name", "one.txt")]),
            record("item-1", "/tmp/b.txt", &[("remote_name", "two.txt")]),
            record("item-1", "/tmp/c.txt", &[]),
            record("item-2", "/tmp/d.txt", &[]),
        ];
        let groups = group_records(records).unwrap();
        let base = UploadOpts {
            remote_name: Some("from-caller.txt".into()),
            remote_names: HashMap::from([(PathBuf::from("/tmp/d.txt"), "caller.txt".into())]),
            ..Default::default()
        };
        let opts = groups[0].item_opts(&base);
        assert_eq!(opts.remote_name, None);
        assert_eq!(
            opts.remote_names,
            HashMap::from([
                (PathBuf::from("/tmp/a.txt"), "one.txt".to_string()),
                (PathBuf::from("/tmp/b.txt"), "two.txt".to_string()),
            ])
        );
        let opts = groups[1].item_opts(&base);
        assert_eq!(opts.remote_name, None);
        assert!(opts.remote_names.is_empty());
    }

    #[test]
    fn validate_groups_refuses_one_file_given_two_remote_names() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let path = file.to_str().unwrap();
        let records = vec![
            record("item-1", path, &[("remote_name", "one.txt")]),
            record("item-1", path, &[("remote_name", "two.txt")]),
        ];
        let groups = group_records(records).unwrap();
        let err = validate_groups(&groups).unwrap_err().to_string();
        assert!(err.contains("different REMOTE_NAME values"), "{err}");
    }

    #[test]
    fn validate_groups_refuses_a_remote_name_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("scans");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.jpg"), "a").unwrap();
        std::fs::write(sub.join("b.jpg"), "b").unwrap();
        let records = vec![record(
            "item-1",
            sub.to_str().unwrap(),
            &[
                ("mediatype", "texts"),
                ("collection", "test_collection"),
                ("remote_name", "renamed"),
            ],
        )];
        let groups = group_records(records).unwrap();
        let err = validate_groups(&groups).unwrap_err().to_string();
        assert!(err.contains("cannot name a directory"), "{err}");
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
            remote_names: vec![None],
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
            remote_names: vec![None],
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
            remote_names: vec![None],
        }];
        validate_groups(&groups).unwrap();
    }
}
