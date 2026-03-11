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
}
