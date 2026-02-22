use std::path::Path;

/// Export format for search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jsonl,
    Csv,
    Identifiers,
}

impl ExportFormat {
    pub fn from_str(s: &str) -> Self {
        match s {
            "CSV" => Self::Csv,
            "Identifiers" => Self::Identifiers,
            _ => Self::Jsonl,
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Csv => "csv",
            Self::Identifiers => "txt",
        }
    }
}

/// A single search result for export purposes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportRecord {
    pub identifier: String,
    pub title: String,
    pub mediatype: String,
    pub description: String,
}

/// Format records as JSONL (one JSON object per line).
pub fn format_jsonl(records: &[ExportRecord]) -> String {
    records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format records as CSV with header row.
pub fn format_csv(records: &[ExportRecord]) -> String {
    let mut lines = Vec::with_capacity(records.len() + 1);
    lines.push("identifier,title,mediatype,description".to_string());
    for r in records {
        lines.push(format!(
            "{},{},{},{}",
            csv_escape(&r.identifier),
            csv_escape(&r.title),
            csv_escape(&r.mediatype),
            csv_escape(&r.description),
        ));
    }
    lines.join("\n")
}

/// Format records as plain identifiers (one per line).
pub fn format_identifiers(records: &[ExportRecord]) -> String {
    records
        .iter()
        .map(|r| r.identifier.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format records according to the given format.
pub fn format_records(records: &[ExportRecord], format: ExportFormat) -> String {
    match format {
        ExportFormat::Jsonl => format_jsonl(records),
        ExportFormat::Csv => format_csv(records),
        ExportFormat::Identifiers => format_identifiers(records),
    }
}

/// Write exported records to a file, returning the path written.
pub fn export_to_file(
    records: &[ExportRecord],
    format: ExportFormat,
    dir: &Path,
) -> std::io::Result<std::path::PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("ia-export-{}.{}", timestamp, format.extension());
    let path = dir.join(filename);

    let content = format_records(records, format);
    std::fs::write(&path, content)?;

    Ok(path)
}

/// Escape a field for CSV output. Wraps in quotes if the field contains
/// commas, quotes, or newlines.
fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_records() -> Vec<ExportRecord> {
        vec![
            ExportRecord {
                identifier: "test-item-1".into(),
                title: "Test Item One".into(),
                mediatype: "texts".into(),
                description: "A test item".into(),
            },
            ExportRecord {
                identifier: "test-item-2".into(),
                title: "Test Item Two".into(),
                mediatype: "audio".into(),
                description: "Another test".into(),
            },
        ]
    }

    #[test]
    fn test_format_jsonl() {
        let records = sample_records();
        let output = format_jsonl(&records);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line should be valid JSON
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("identifier").is_some());
            assert!(parsed.get("title").is_some());
        }
        assert!(lines[0].contains("test-item-1"));
        assert!(lines[1].contains("test-item-2"));
    }

    #[test]
    fn test_format_csv() {
        let records = sample_records();
        let output = format_csv(&records);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 records
        assert_eq!(lines[0], "identifier,title,mediatype,description");
        assert!(lines[1].starts_with("test-item-1,"));
        assert!(lines[2].starts_with("test-item-2,"));
    }

    #[test]
    fn test_format_csv_escaping() {
        let records = vec![ExportRecord {
            identifier: "item-with-comma".into(),
            title: "Title, with comma".into(),
            mediatype: "texts".into(),
            description: "Has \"quotes\" too".into(),
        }];
        let output = format_csv(&records);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        // Title should be quoted because it contains a comma
        assert!(lines[1].contains("\"Title, with comma\""));
        // Description should be quoted because it contains quotes
        assert!(lines[1].contains("\"Has \"\"quotes\"\" too\""));
    }

    #[test]
    fn test_format_identifiers() {
        let records = sample_records();
        let output = format_identifiers(&records);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "test-item-1");
        assert_eq!(lines[1], "test-item-2");
    }

    #[test]
    fn test_format_records_dispatches() {
        let records = sample_records();
        assert_eq!(
            format_records(&records, ExportFormat::Identifiers),
            format_identifiers(&records)
        );
        assert_eq!(
            format_records(&records, ExportFormat::Csv),
            format_csv(&records)
        );
        assert_eq!(
            format_records(&records, ExportFormat::Jsonl),
            format_jsonl(&records)
        );
    }

    #[test]
    fn test_export_format_from_str() {
        assert_eq!(ExportFormat::from_str("JSONL"), ExportFormat::Jsonl);
        assert_eq!(ExportFormat::from_str("CSV"), ExportFormat::Csv);
        assert_eq!(ExportFormat::from_str("Identifiers"), ExportFormat::Identifiers);
        assert_eq!(ExportFormat::from_str("anything"), ExportFormat::Jsonl);
    }

    #[test]
    fn test_export_format_extension() {
        assert_eq!(ExportFormat::Jsonl.extension(), "jsonl");
        assert_eq!(ExportFormat::Csv.extension(), "csv");
        assert_eq!(ExportFormat::Identifiers.extension(), "txt");
    }

    #[test]
    fn test_export_to_file() {
        let records = sample_records();
        let dir = tempfile::tempdir().unwrap();
        let path = export_to_file(&records, ExportFormat::Jsonl, dir.path()).unwrap();
        assert!(path.exists());
        assert!(path.extension().unwrap() == "jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[test]
    fn test_empty_records() {
        let records: Vec<ExportRecord> = vec![];
        assert_eq!(format_jsonl(&records), "");
        assert_eq!(format_csv(&records), "identifier,title,mediatype,description");
        assert_eq!(format_identifiers(&records), "");
    }
}
