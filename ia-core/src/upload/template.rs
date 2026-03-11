use crate::error::{IaError, Result};
use std::path::Path;

/// Options for template generation.
#[derive(Debug, Clone, Default)]
pub struct TemplateOpts {
    /// Prefix to prepend to generated identifiers.
    pub identifier_prefix: Option<String>,
    /// Generate identifiers from filenames.
    pub identifier_from_filename: bool,
    /// Generate identifiers from parent directory names.
    pub identifier_from_dirname: bool,
}

/// A single row in the upload template.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateRow {
    pub identifier: String,
    pub file: String,
    pub remote_name: String,
    pub mediatype: String,
    pub collection: String,
    pub title: String,
    pub creator: String,
    pub date: String,
    pub description: String,
    pub subject: String,
    pub language: String,
}

/// Sanitize a string into a valid IA identifier component.
///
/// - Lowercases ASCII alphanumeric characters
/// - Replaces non-allowed characters with `-`
/// - Strips leading non-alphanumeric characters
/// - Returns empty string if result is less than 3 chars
/// - Truncates to 100 chars
fn sanitize_identifier(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Ensure starts with alphanumeric
    let sanitized = sanitized
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if sanitized.len() < 3 {
        String::new() // too short, leave for user to fill in
    } else if sanitized.len() > 100 {
        sanitized[..100].to_string()
    } else {
        sanitized
    }
}

/// Generate template rows by walking a directory.
///
/// Recursively finds all regular files (skipping dotfiles and symlinks),
/// and creates one row per file with generated or empty identifiers.
pub fn generate_template(dir: &Path, opts: &TemplateOpts) -> Result<Vec<TemplateRow>> {
    let mut files = Vec::new();
    walk_dir(dir, &mut files)?;
    files.sort();

    let rows = files
        .into_iter()
        .map(|path| {
            let identifier = generate_identifier(&path, opts);
            TemplateRow {
                identifier,
                file: path.to_string_lossy().into_owned(),
                remote_name: String::new(),
                mediatype: String::new(),
                collection: String::new(),
                title: String::new(),
                creator: String::new(),
                date: String::new(),
                description: String::new(),
                subject: String::new(),
                language: String::new(),
            }
        })
        .collect();

    Ok(rows)
}

/// Recursively walk a directory, collecting regular files.
/// Skips dotfiles (names starting with `.`) and symlinks.
fn walk_dir(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip dotfiles
        if name_str.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;

        // Skip symlinks
        if file_type.is_symlink() {
            continue;
        }

        let path = entry.path();
        if file_type.is_dir() {
            walk_dir(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Generate an identifier for a file based on the template options.
fn generate_identifier(path: &Path, opts: &TemplateOpts) -> String {
    let raw = if opts.identifier_from_filename {
        // Use filename without extension
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else if opts.identifier_from_dirname {
        // Use parent directory name
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        return String::new();
    };

    let sanitized = sanitize_identifier(&raw);
    if sanitized.is_empty() {
        return String::new();
    }

    match &opts.identifier_prefix {
        Some(prefix) => format!("{prefix}-{sanitized}"),
        None => sanitized,
    }
}

/// Write template rows as CSV to a writer.
pub fn write_template_csv<W: std::io::Write>(rows: &[TemplateRow], writer: &mut W) -> Result<()> {
    let mut csv_writer = csv::Writer::from_writer(writer);

    // Header
    csv_writer
        .write_record([
            "identifier",
            "file",
            "REMOTE_NAME",
            "mediatype",
            "collection",
            "title",
            "creator",
            "date",
            "description",
            "subject",
            "language",
        ])
        .map_err(|e| IaError::Config(format!("CSV write error: {e}")))?;

    // Rows
    for row in rows {
        csv_writer
            .write_record([
                &row.identifier,
                &row.file,
                &row.remote_name,
                &row.mediatype,
                &row.collection,
                &row.title,
                &row.creator,
                &row.date,
                &row.description,
                &row.subject,
                &row.language,
            ])
            .map_err(|e| IaError::Config(format!("CSV write error: {e}")))?;
    }

    csv_writer
        .flush()
        .map_err(|e| IaError::Config(format!("CSV flush error: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_identifier("Hello World"), "hello-world");
    }

    #[test]
    fn sanitize_strips_leading_non_alnum() {
        assert_eq!(sanitize_identifier("--my-file"), "my-file");
    }

    #[test]
    fn sanitize_too_short() {
        assert_eq!(sanitize_identifier("ab"), "");
    }

    #[test]
    fn sanitize_truncates_long() {
        let long = "a".repeat(150);
        assert_eq!(sanitize_identifier(&long).len(), 100);
    }

    #[test]
    fn sanitize_preserves_dots_and_underscores() {
        assert_eq!(sanitize_identifier("my_file.v2"), "my_file.v2");
    }

    #[test]
    fn sanitize_empty_input() {
        assert_eq!(sanitize_identifier(""), "");
    }

    #[test]
    fn sanitize_all_special_chars() {
        assert_eq!(sanitize_identifier("@#$"), "");
    }

    #[test]
    fn generate_template_empty_dir() {
        let dir = TempDir::new().unwrap();
        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn generate_template_with_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file1.txt"), "content").unwrap();
        fs::write(dir.path().join("file2.pdf"), "content").unwrap();
        fs::write(dir.path().join(".hidden"), "hidden").unwrap();

        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        assert_eq!(rows.len(), 2); // hidden file skipped
        assert!(rows[0].identifier.is_empty()); // default: empty
        assert!(rows[0].file.contains("file"));
    }

    #[test]
    fn generate_template_from_filename() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("My Document.pdf"), "content").unwrap();

        let opts = TemplateOpts {
            identifier_from_filename: true,
            ..Default::default()
        };
        let rows = generate_template(dir.path(), &opts).unwrap();
        assert_eq!(rows[0].identifier, "my-document");
    }

    #[test]
    fn generate_template_from_dirname() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("My Collection");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("file.txt"), "content").unwrap();

        let opts = TemplateOpts {
            identifier_from_dirname: true,
            ..Default::default()
        };
        let rows = generate_template(dir.path(), &opts).unwrap();
        assert_eq!(rows[0].identifier, "my-collection");
    }

    #[test]
    fn generate_template_with_prefix() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let opts = TemplateOpts {
            identifier_prefix: Some("myproject".into()),
            identifier_from_filename: true,
            ..Default::default()
        };
        let rows = generate_template(dir.path(), &opts).unwrap();
        assert!(rows[0].identifier.starts_with("myproject-"));
    }

    #[test]
    fn generate_template_skips_symlinks() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("real.txt"), "content").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
                .unwrap();
        }

        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        // Should have only the real file, not the symlink
        assert_eq!(rows.len(), 1);
        assert!(rows[0].file.contains("real.txt"));
    }

    #[test]
    fn generate_template_recursive() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("top.txt"), "content").unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("nested.txt"), "content").unwrap();

        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn generate_template_sorted_output() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("zzz.txt"), "content").unwrap();
        fs::write(dir.path().join("aaa.txt"), "content").unwrap();
        fs::write(dir.path().join("mmm.txt"), "content").unwrap();

        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        assert!(rows[0].file < rows[1].file);
        assert!(rows[1].file < rows[2].file);
    }

    #[test]
    fn generate_template_file_paths_are_absolute() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let rows = generate_template(dir.path(), &TemplateOpts::default()).unwrap();
        let file_path = Path::new(&rows[0].file);
        assert!(file_path.is_absolute());
    }

    #[test]
    fn write_csv_output() {
        let rows = vec![TemplateRow {
            identifier: "test-item".into(),
            file: "/tmp/file.txt".into(),
            remote_name: String::new(),
            mediatype: "texts".into(),
            collection: String::new(),
            title: String::new(),
            creator: String::new(),
            date: String::new(),
            description: String::new(),
            subject: String::new(),
            language: String::new(),
        }];
        let mut buf = Vec::new();
        write_template_csv(&rows, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("identifier"));
        assert!(output.contains("test-item"));
        assert!(output.contains("texts"));
    }

    #[test]
    fn write_csv_empty_rows() {
        let rows: Vec<TemplateRow> = Vec::new();
        let mut buf = Vec::new();
        write_template_csv(&rows, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // Should still have the header
        assert!(output.contains("identifier"));
        assert!(output.contains("file"));
        // Only one line (header) plus trailing newline
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn write_csv_has_all_columns() {
        let rows = vec![TemplateRow {
            identifier: "id".into(),
            file: "/path".into(),
            remote_name: String::new(),
            mediatype: String::new(),
            collection: String::new(),
            title: String::new(),
            creator: String::new(),
            date: String::new(),
            description: String::new(),
            subject: String::new(),
            language: String::new(),
        }];
        let mut buf = Vec::new();
        write_template_csv(&rows, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let header = output.lines().next().unwrap();
        for col in [
            "identifier",
            "file",
            "REMOTE_NAME",
            "mediatype",
            "collection",
            "title",
            "creator",
            "date",
            "description",
            "subject",
            "language",
        ] {
            assert!(header.contains(col), "header missing column: {col}");
        }
    }

    #[test]
    fn prefix_without_identifier_generation_is_noop() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let opts = TemplateOpts {
            identifier_prefix: Some("myproject".into()),
            identifier_from_filename: false,
            identifier_from_dirname: false,
        };
        let rows = generate_template(dir.path(), &opts).unwrap();
        // No identifier generation mode enabled, so identifier should be empty
        assert!(rows[0].identifier.is_empty());
    }
}
