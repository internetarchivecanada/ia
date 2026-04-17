use globset::{Glob, GlobMatcher};

use crate::types::{FileMetadata, FileSource, ItemMetadata};

/// Options for filtering files.
#[derive(Debug, Clone, Default)]
pub struct FileFilter {
    pub glob: Option<String>,
    pub exclude: Option<String>,
    pub formats: Vec<String>,
    pub source: Option<FileSource>,
    pub exclude_source: Option<FileSource>,
    pub names: Vec<String>,
}

/// Validate that glob and exclude patterns in a filter are syntactically valid.
///
/// Returns an error describing the first invalid pattern found.
/// Call this at the CLI boundary before starting work so the user gets a clear
/// message instead of silently downloading everything.
pub fn validate_filter(filter: &FileFilter) -> std::result::Result<(), String> {
    if let Some(ref glob) = filter.glob {
        for pattern in glob.split('|') {
            let p = pattern.trim();
            if !p.is_empty() {
                if let Err(e) = Glob::new(p) {
                    return Err(format!("invalid --glob pattern \"{p}\": {e}"));
                }
            }
        }
    }
    if let Some(ref exclude) = filter.exclude {
        for pattern in exclude.split('|') {
            let p = pattern.trim();
            if !p.is_empty() {
                if let Err(e) = Glob::new(p) {
                    return Err(format!("invalid --exclude pattern \"{p}\": {e}"));
                }
            }
        }
    }
    Ok(())
}

/// Placeholders recognized inside positional file-name args (e.g. `{identifier}.pdf`).
///
/// Add new entries here when expanding the template grammar.
pub const KNOWN_NAME_PLACEHOLDERS: &[&str] = &["{identifier}"];

/// Substitute supported placeholders in a list of file names with per-item values.
///
/// Currently supports `{identifier}`. Names without placeholders pass through
/// unchanged, so callers may invoke this unconditionally.
pub fn substitute_names(names: &[String], identifier: &str) -> Vec<String> {
    names
        .iter()
        .map(|n| n.replace("{identifier}", identifier))
        .collect()
}

/// Reject `{...}` placeholders that aren't in [`KNOWN_NAME_PLACEHOLDERS`].
///
/// Run this at the CLI boundary so a typo like `{ident}.pdf` fails loudly
/// instead of silently producing an empty download.
pub fn validate_name_placeholders(names: &[String]) -> std::result::Result<(), String> {
    for name in names {
        let mut rest = name.as_str();
        while let Some(open) = rest.find('{') {
            let after_open = &rest[open..];
            let close = after_open
                .find('}')
                .ok_or_else(|| format!("unterminated `{{` placeholder in file name \"{name}\""))?;
            let placeholder = &after_open[..=close];
            if !KNOWN_NAME_PLACEHOLDERS.contains(&placeholder) {
                return Err(format!(
                    "unknown placeholder \"{placeholder}\" in file name \"{name}\" \
                     (supported: {})",
                    KNOWN_NAME_PLACEHOLDERS.join(", ")
                ));
            }
            rest = &after_open[close + 1..];
        }
    }
    Ok(())
}

/// List files from an item, applying filters.
pub fn list<'a>(item: &'a ItemMetadata, filter: &FileFilter) -> Vec<&'a FileMetadata> {
    let glob_matcher = filter.glob.as_ref().and_then(|g| {
        // Support pipe-separated globs like Python: "*.mp4|*.webm"
        // We match if ANY sub-glob matches
        let patterns: Vec<GlobMatcher> = g
            .split('|')
            .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
            .collect();
        if patterns.is_empty() {
            None
        } else {
            Some(patterns)
        }
    });

    let exclude_matcher = filter.exclude.as_ref().and_then(|g| {
        let patterns: Vec<GlobMatcher> = g
            .split('|')
            .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
            .collect();
        if patterns.is_empty() {
            None
        } else {
            Some(patterns)
        }
    });

    item.files
        .iter()
        .filter(|f| {
            // Filter by specific file names
            if !filter.names.is_empty() {
                return filter.names.iter().any(|n| n == &f.name);
            }

            // Filter by glob pattern
            if let Some(matchers) = &glob_matcher {
                if !matchers.iter().any(|m| m.is_match(&f.name)) {
                    return false;
                }
            }

            // Filter by exclude pattern
            if let Some(matchers) = &exclude_matcher {
                if matchers.iter().any(|m| m.is_match(&f.name)) {
                    return false;
                }
            }

            // Filter by format
            if !filter.formats.is_empty() {
                if let Some(fmt) = &f.format {
                    if !filter.formats.iter().any(|ff| ff.eq_ignore_ascii_case(fmt)) {
                        return false;
                    }
                } else {
                    return false;
                }
            }

            // Filter by source
            if let Some(src) = &filter.source {
                if f.source.as_deref() != Some(src.as_str()) {
                    return false;
                }
            }

            // Filter by excluded source
            if let Some(src) = &filter.exclude_source {
                if f.source.as_deref() == Some(src.as_str()) {
                    return false;
                }
            }

            true
        })
        .collect()
}

/// Calculate total size of a set of files.
pub fn total_size(files: &[&FileMetadata]) -> u64 {
    files.iter().filter_map(|f| f.size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileMetadata, ItemMetadata, MetadataFields};
    use std::collections::HashMap;

    fn make_item(files: Vec<FileMetadata>) -> ItemMetadata {
        ItemMetadata {
            metadata: MetadataFields::default(),
            files,
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra: HashMap::new(),
        }
    }

    fn make_file(name: &str, source: &str, format: &str, size: u64) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some(source.to_string()),
            format: Some(format.to_string()),
            size: Some(size),
            md5: None,
            mtime: None,
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn list_all_files() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let result = list(&item, &FileFilter::default());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_glob() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.jpg", "original", "JPEG", 150),
        ]);
        let result = list(
            &item,
            &FileFilter {
                glob: Some("*.jpg".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|f| f.name.ends_with(".jpg")));
    }

    #[test]
    fn filter_by_pipe_separated_glob() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.png", "original", "PNG", 150),
        ]);
        let result = list(
            &item,
            &FileFilter {
                glob: Some("*.jpg|*.png".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_exclude() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("a_thumb.jpg", "derivative", "JPEG", 10),
            make_file("b.jpg", "original", "JPEG", 200),
        ]);
        let result = list(
            &item,
            &FileFilter {
                exclude: Some("*_thumb*".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_source() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("a_thumb.jpg", "derivative", "JPEG", 10),
        ]);
        let result = list(
            &item,
            &FileFilter {
                source: Some(FileSource::Original),
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "a.jpg");
    }

    #[test]
    fn filter_by_format() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let result = list(
            &item,
            &FileFilter {
                formats: vec!["JPEG".to_string()],
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_by_specific_names() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.jpg", "original", "JPEG", 150),
        ]);
        let result = list(
            &item,
            &FileFilter {
                names: vec!["a.jpg".to_string(), "c.jpg".to_string()],
                ..Default::default()
            },
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn total_size_calculation() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let files = list(&item, &FileFilter::default());
        assert_eq!(total_size(&files), 300);
    }

    #[test]
    fn validate_filter_accepts_valid_glob() {
        let filter = FileFilter {
            glob: Some("*.mp4|*.webm".to_string()),
            ..Default::default()
        };
        assert!(validate_filter(&filter).is_ok());
    }

    #[test]
    fn validate_filter_rejects_invalid_glob() {
        let filter = FileFilter {
            glob: Some("[invalid".to_string()),
            ..Default::default()
        };
        let err = validate_filter(&filter).unwrap_err();
        assert!(err.contains("--glob"), "error should mention --glob: {err}");
        assert!(
            err.contains("[invalid"),
            "error should include pattern: {err}"
        );
    }

    #[test]
    fn validate_filter_rejects_invalid_exclude() {
        let filter = FileFilter {
            exclude: Some("[bad".to_string()),
            ..Default::default()
        };
        let err = validate_filter(&filter).unwrap_err();
        assert!(
            err.contains("--exclude"),
            "error should mention --exclude: {err}"
        );
    }

    #[test]
    fn validate_filter_catches_first_bad_in_pipe_separated() {
        let filter = FileFilter {
            glob: Some("*.mp4|[bad|*.webm".to_string()),
            ..Default::default()
        };
        let err = validate_filter(&filter).unwrap_err();
        assert!(
            err.contains("[bad"),
            "error should include bad pattern: {err}"
        );
    }

    #[test]
    fn validate_filter_accepts_empty() {
        assert!(validate_filter(&FileFilter::default()).is_ok());
    }

    #[test]
    fn substitute_names_expands_identifier() {
        let names = vec!["{identifier}.pdf".to_string()];
        let out = substitute_names(&names, "scotus-12-345");
        assert_eq!(out, vec!["scotus-12-345.pdf".to_string()]);
    }

    #[test]
    fn substitute_names_passes_literals_unchanged() {
        let names = vec!["README.md".to_string(), "data.json".to_string()];
        let out = substitute_names(&names, "anything");
        assert_eq!(out, names);
    }

    #[test]
    fn substitute_names_replaces_multiple_occurrences() {
        let names = vec!["{identifier}/{identifier}_meta.xml".to_string()];
        let out = substitute_names(&names, "abc");
        assert_eq!(out, vec!["abc/abc_meta.xml".to_string()]);
    }

    #[test]
    fn substitute_names_is_idempotent_when_already_substituted() {
        let once = substitute_names(&["{identifier}.pdf".to_string()], "abc");
        let twice = substitute_names(&once, "different");
        assert_eq!(once, twice);
    }

    #[test]
    fn validate_name_placeholders_accepts_known() {
        let names = vec![
            "{identifier}.pdf".to_string(),
            "{identifier}_meta.xml".to_string(),
            "literal.txt".to_string(),
        ];
        assert!(validate_name_placeholders(&names).is_ok());
    }

    #[test]
    fn validate_name_placeholders_rejects_unknown() {
        let names = vec!["{ident}.pdf".to_string()];
        let err = validate_name_placeholders(&names).unwrap_err();
        assert!(
            err.contains("{ident}"),
            "error should name the placeholder: {err}"
        );
        assert!(
            err.contains("{identifier}"),
            "error should list supported placeholders: {err}"
        );
    }

    #[test]
    fn validate_name_placeholders_rejects_unterminated_brace() {
        let names = vec!["{identifier.pdf".to_string()];
        let err = validate_name_placeholders(&names).unwrap_err();
        assert!(err.contains("unterminated"), "error: {err}");
    }

    #[test]
    fn validate_name_placeholders_accepts_empty() {
        assert!(validate_name_placeholders(&[]).is_ok());
    }
}
