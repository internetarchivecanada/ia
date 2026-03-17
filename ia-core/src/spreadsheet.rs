use crate::error::{IaError, Result};
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;

/// A single record from a spreadsheet: (identifier, field_name → value).
pub type SpreadsheetRecord = (String, HashMap<String, String>);

/// Read a spreadsheet file and return records.
/// Format auto-detected by file extension: .csv, .tsv, .xlsx, .ods, .jsonl
pub fn read_spreadsheet(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => read_csv(path, b','),
        "tsv" => read_csv(path, b'\t'),
        "xlsx" | "ods" | "xls" => read_calamine(path),
        "jsonl" | "ndjson" => read_jsonl(path),
        other => Err(IaError::Config(format!(
            "unsupported spreadsheet format: .{other} (supported: .csv, .tsv, .xlsx, .ods, .jsonl)"
        ))),
    }
}

fn read_csv(path: &Path, delimiter: u8) -> Result<Vec<SpreadsheetRecord>> {
    let data = std::fs::read(path)?;
    // Strip BOM if present
    let data = if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &data[3..]
    } else {
        &data
    };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(data);

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| IaError::Config(format!("failed to read CSV headers: {e}")))?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| IaError::Config("CSV must have an 'identifier' column".into()))?;

    let mut records = Vec::new();
    for result in reader.records() {
        let row = result.map_err(|e| IaError::Config(format!("CSV parse error: {e}")))?;
        let identifier = row.get(id_col).unwrap_or("").trim().to_string();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (i, value) in row.iter().enumerate() {
            if i == id_col || i >= headers.len() {
                continue;
            }
            let value = value.trim();
            if !value.is_empty() {
                fields.insert(headers[i].clone(), value.to_string());
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}

fn read_calamine(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    use calamine::{open_workbook_auto, DataType, Reader};

    let mut workbook = open_workbook_auto(path)
        .map_err(|e| IaError::Config(format!("failed to open spreadsheet: {e}")))?;

    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| IaError::Config("spreadsheet has no sheets".into()))?;

    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| IaError::Config(format!("failed to read sheet: {e}")))?;

    let mut rows = range.rows();

    // First row = headers
    let header_row = rows
        .next()
        .ok_or_else(|| IaError::Config("spreadsheet is empty".into()))?;
    let headers: Vec<String> = header_row
        .iter()
        .map(|cell| cell.to_string().trim().to_lowercase())
        .collect();

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| IaError::Config("spreadsheet must have an 'identifier' column".into()))?;

    let mut records = Vec::new();
    for row in rows {
        let identifier = row
            .get(id_col)
            .map(|c| c.to_string().trim().to_string())
            .unwrap_or_default();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (i, cell) in row.iter().enumerate() {
            if i == id_col || i >= headers.len() {
                continue;
            }
            if cell.is_empty() {
                continue;
            }
            let value = cell.to_string().trim().to_string();
            if !value.is_empty() {
                fields.insert(headers[i].clone(), value);
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}

fn read_jsonl(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    let content = std::fs::read_to_string(path)?;
    let mut records = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let obj: HashMap<String, serde_json::Value> = serde_json::from_str(line)
            .map_err(|e| IaError::Config(format!("JSONL parse error: {e}")))?;

        let identifier = obj
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (key, value) in &obj {
            if key == "identifier" {
                continue;
            }
            let s = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !s.is_empty() {
                fields.insert(key.to_lowercase(), s);
            }
        }

        records.push((identifier, fields));
    }

    Ok(records)
}

/// Merge bracket-indexed columns into JSON arrays.
///
/// Spreadsheets may use `subject[0]`, `subject[1]`, etc. to represent
/// multi-value fields. This function groups them into a single field with
/// a JSON array value, sorted by index.
///
/// - Non-indexed columns pass through as scalar JSON strings.
/// - Columns with an op-prefix (containing `:`) are never treated as indexed.
/// - A single indexed value (e.g. only `subject[0]`) becomes a scalar, not
///   a one-element array (matches export behavior).
/// - Entries whose value is `REMOVE_TAG` are filtered out; if all entries for
///   a field are `REMOVE_TAG`, the field emits `REMOVE_TAG` as a sentinel.
pub fn merge_indexed_columns(fields: &HashMap<String, String>) -> Vec<(String, serde_json::Value)> {
    use crate::metadata::write::{parse_indexed_key, REMOVE_TAG};
    use serde_json::json;

    let mut resolved: Vec<(String, serde_json::Value)> = Vec::new();
    let mut indexed: HashMap<String, Vec<(usize, String)>> = HashMap::new();

    for (col_name, value) in fields {
        // Only bare columns (no op-prefix) can be indexed
        if !col_name.contains(':') {
            if let Some((base_field, idx)) = parse_indexed_key(col_name) {
                indexed
                    .entry(base_field)
                    .or_default()
                    .push((idx, value.clone()));
                continue;
            }
        }
        resolved.push((col_name.clone(), json!(value)));
    }

    // Append merged indexed fields as array values.
    // - Single index (e.g. only subject[0]) → bare scalar (same as unindexed)
    // - REMOVE_TAG entries are filtered out; if all are REMOVE_TAG, remove field
    for (base_field, mut entries) in indexed {
        entries.sort_by_key(|(idx, _)| *idx);
        let values: Vec<serde_json::Value> = entries
            .into_iter()
            .filter(|(_, v)| v != REMOVE_TAG)
            .map(|(_, v)| json!(v))
            .collect();
        match values.len() {
            0 => {
                // All entries were REMOVE_TAG → delete the field
                resolved.push((base_field, json!(REMOVE_TAG)));
            }
            1 => {
                // Single value → bare scalar (matches export of single-element arrays)
                resolved.push((base_field, values.into_iter().next().unwrap()));
            }
            _ => {
                resolved.push((base_field, serde_json::Value::Array(values)));
            }
        }
    }

    resolved
}

/// File extensions recognized as structured spreadsheet formats.
const SPREADSHEET_EXTENSIONS: &[&str] = &["csv", "tsv", "xlsx", "ods", "xls", "jsonl", "ndjson"];

/// Read identifiers from a file. Spreadsheet formats (.csv, .tsv, .xlsx, .ods,
/// .jsonl) are parsed and the `identifier` column is extracted. Unrecognized
/// extensions (including no extension) are read as plain text — one identifier
/// per line, skipping blanks and `#` comments. Duplicate identifiers are removed
/// (preserving first occurrence order).
pub fn read_identifiers_from_file(path: &Path) -> Result<Vec<String>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let ids: Vec<String> = if SPREADSHEET_EXTENSIONS.contains(&ext.as_str()) {
        let records = read_spreadsheet(path)?;
        records.into_iter().map(|(id, _)| id).collect()
    } else {
        // Plain text: one identifier per line
        let content = std::fs::read_to_string(path)?;
        content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    };

    // Deduplicate preserving order
    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect())
}

/// Write records to a spreadsheet file.
/// Format auto-detected by file extension: .csv, .tsv, .xlsx, .jsonl
pub fn write_spreadsheet(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => write_csv(path, records, b','),
        "tsv" => write_csv(path, records, b'\t'),
        "xlsx" => write_xlsx(path, records),
        "jsonl" | "ndjson" => write_jsonl_file(path, records),
        "ods" | "xls" => Err(IaError::Config(format!(
            "export to .{ext} is not supported (import reads .{ext}, but export only writes .csv, .tsv, .xlsx, .jsonl)"
        ))),
        other => Err(IaError::Config(format!(
            "unsupported export format: .{other} (supported: .csv, .tsv, .xlsx, .jsonl)"
        ))),
    }
}

/// Collect all unique field names across records in sorted order.
fn collect_field_names(records: &[SpreadsheetRecord]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for (_, fields) in records {
        for key in fields.keys() {
            names.insert(key.clone());
        }
    }
    names.into_iter().collect()
}

fn write_csv(path: &Path, records: &[SpreadsheetRecord], delimiter: u8) -> Result<()> {
    let field_names = collect_field_names(records);

    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .map_err(std::io::Error::other)?;

    // Header row: identifier + field names
    let mut header = vec!["identifier".to_string()];
    header.extend(field_names.iter().cloned());
    writer
        .write_record(&header)
        .map_err(std::io::Error::other)?;

    // Data rows
    for (identifier, fields) in records {
        let mut row = vec![identifier.clone()];
        for name in &field_names {
            row.push(fields.get(name).cloned().unwrap_or_default());
        }
        writer.write_record(&row).map_err(std::io::Error::other)?;
    }

    writer.flush()?;

    Ok(())
}

fn write_xlsx(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    use rust_xlsxwriter::Workbook;

    let field_names = collect_field_names(records);

    // XLSX columns are u16 (max 65536). Validate upfront.
    let total_cols = field_names.len() + 1; // +1 for identifier column
    if total_cols > u16::MAX as usize {
        return Err(IaError::Config(format!(
            "too many columns for XLSX format: {total_cols} (max {})",
            u16::MAX
        )));
    }

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    // Header row
    worksheet
        .write_string(0, 0, "identifier")
        .map_err(std::io::Error::other)?;
    for (col, name) in field_names.iter().enumerate() {
        let col_idx = u16::try_from(col + 1).map_err(std::io::Error::other)?;
        worksheet
            .write_string(0, col_idx, name)
            .map_err(std::io::Error::other)?;
    }

    // Data rows
    for (row_idx, (identifier, fields)) in records.iter().enumerate() {
        let row = u32::try_from(row_idx + 1).map_err(std::io::Error::other)?;
        worksheet
            .write_string(row, 0, identifier)
            .map_err(std::io::Error::other)?;
        for (col_idx, name) in field_names.iter().enumerate() {
            let col = u16::try_from(col_idx + 1).map_err(std::io::Error::other)?;
            let value = fields.get(name).cloned().unwrap_or_default();
            worksheet
                .write_string(row, col, &value)
                .map_err(std::io::Error::other)?;
        }
    }

    workbook.save(path).map_err(std::io::Error::other)?;

    Ok(())
}

fn write_jsonl_file(path: &Path, records: &[SpreadsheetRecord]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;

    for (identifier, fields) in records {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "identifier".to_string(),
            serde_json::Value::String(identifier.clone()),
        );
        for (key, value) in fields {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
        let line = serde_json::to_string(&serde_json::Value::Object(obj))?;
        writeln!(file, "{line}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_csv() {
        let csv =
            "identifier,title,date\nnasa,NASA Images,2024-01-01\nmars,Mars Rover,2024-06-01\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "nasa");
        assert_eq!(records[0].1.get("title").unwrap(), "NASA Images");
        assert_eq!(records[1].0, "mars");
    }

    #[test]
    fn read_csv_with_bom() {
        let csv = "\u{FEFF}identifier,title\nnasa,NASA\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "nasa");
    }

    #[test]
    fn read_csv_skips_empty_identifier() {
        let csv = "identifier,title\nnasa,NASA\n,Empty\n\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn read_csv_skips_empty_values() {
        let csv = "identifier,title,date\nnasa,NASA,\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, csv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert!(records[0].1.get("date").is_none()); // empty → skipped
    }

    #[test]
    fn read_tsv() {
        let tsv = "identifier\ttitle\nnasa\tNASA Images\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tsv");
        std::fs::write(&path, tsv).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.get("title").unwrap(), "NASA Images");
    }

    #[test]
    fn read_jsonl() {
        let jsonl = r#"{"identifier":"nasa","title":"NASA","date":"2024"}
{"identifier":"mars","title":"Mars"}
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, jsonl).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "nasa");
        assert_eq!(records[0].1.get("title").unwrap(), "NASA");
    }

    #[test]
    fn read_jsonl_skips_empty_lines() {
        let jsonl = "{\"identifier\":\"nasa\",\"title\":\"NASA\"}\n\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, jsonl).unwrap();

        let records = read_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn unknown_extension_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xyz");
        std::fs::write(&path, "data").unwrap();
        assert!(read_spreadsheet(&path).is_err());
    }

    #[test]
    fn write_and_read_csv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let records = vec![
            ("nasa".to_string(), {
                let mut m = HashMap::new();
                m.insert("title".to_string(), "NASA Images".to_string());
                m.insert("date".to_string(), "2024-01-01".to_string());
                m
            }),
            ("mars".to_string(), {
                let mut m = HashMap::new();
                m.insert("title".to_string(), "Mars Rover".to_string());
                m
            }),
        ];

        write_spreadsheet(&path, &records).unwrap();
        let read_back = read_spreadsheet(&path).unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].0, "nasa");
        assert_eq!(read_back[0].1.get("title").unwrap(), "NASA Images");
        assert_eq!(read_back[1].0, "mars");
        assert_eq!(read_back[1].1.get("title").unwrap(), "Mars Rover");
    }

    #[test]
    fn write_and_read_tsv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tsv");
        let records = vec![("item1".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "Test".to_string());
            m
        })];

        write_spreadsheet(&path, &records).unwrap();
        let read_back = read_spreadsheet(&path).unwrap();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].1.get("title").unwrap(), "Test");
    }

    #[test]
    fn write_and_read_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        let records = vec![("nasa".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "NASA".to_string());
            m
        })];

        write_spreadsheet(&path, &records).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let line: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(line["identifier"], "nasa");
        assert_eq!(line["title"], "NASA");
    }

    #[test]
    fn write_and_read_xlsx() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.xlsx");
        let records = vec![("nasa".to_string(), {
            let mut m = HashMap::new();
            m.insert("title".to_string(), "NASA".to_string());
            m
        })];

        write_spreadsheet(&path, &records).unwrap();
        let read_back = read_spreadsheet(&path).unwrap();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].0, "nasa");
    }

    #[test]
    fn write_csv_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("round.csv");
        let original = vec![
            ("a".to_string(), {
                let mut m = HashMap::new();
                m.insert("x".to_string(), "1".to_string());
                m.insert("y".to_string(), "2".to_string());
                m
            }),
            ("b".to_string(), {
                let mut m = HashMap::new();
                m.insert("x".to_string(), "3".to_string());
                m
            }),
        ];

        write_spreadsheet(&path, &original).unwrap();
        let read_back = read_spreadsheet(&path).unwrap();

        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].0, "a");
        assert_eq!(read_back[0].1.get("x").unwrap(), "1");
        assert_eq!(read_back[0].1.get("y").unwrap(), "2");
        assert_eq!(read_back[1].0, "b");
        assert_eq!(read_back[1].1.get("x").unwrap(), "3");
        assert!(read_back[1].1.get("y").is_none());
    }

    #[test]
    fn write_ods_gives_read_only_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.ods");
        let records = vec![];
        let err = write_spreadsheet(&path, &records).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("import reads .ods"),
            "error should hint that ODS is read-only: {msg}"
        );
    }

    #[test]
    fn write_unknown_format_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.parquet");
        let records = vec![];
        let err = write_spreadsheet(&path, &records).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported export format"),
            "error should say unsupported: {msg}"
        );
    }

    // -- merge_indexed_columns tests --

    #[test]
    fn merge_indexed_columns_combines_into_array() {
        let fields: HashMap<String, String> = [
            ("title".into(), "Apollo 11".into()),
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "nasa".into()),
            ("description".into(), "Moon landing".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        // 3 entries: title, description, and subject (merged)
        assert_eq!(merged.len(), 3);
        // Find each by field name since HashMap order is non-deterministic
        let subject = merged.iter().find(|(k, _)| k == "subject").unwrap();
        assert_eq!(subject.1, serde_json::json!(["science", "nasa"]));
        let title = merged.iter().find(|(k, _)| k == "title").unwrap();
        assert_eq!(title.1, serde_json::json!("Apollo 11"));
        let desc = merged.iter().find(|(k, _)| k == "description").unwrap();
        assert_eq!(desc.1, serde_json::json!("Moon landing"));
    }

    #[test]
    fn merge_indexed_columns_sorts_by_index() {
        let fields: HashMap<String, String> = [
            ("subject[2]".into(), "history".into()),
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "nasa".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0],
            (
                "subject".into(),
                serde_json::json!(["science", "nasa", "history"])
            )
        );
    }

    #[test]
    fn merge_indexed_columns_preserves_prefixed_columns() {
        let fields: HashMap<String, String> = [
            ("append:subject".into(), "new-tag".into()),
            ("title".into(), "Test".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 2);
        assert!(merged
            .iter()
            .any(|(k, v)| k == "append:subject" && v == &serde_json::json!("new-tag")));
        assert!(merged
            .iter()
            .any(|(k, v)| k == "title" && v == &serde_json::json!("Test")));
    }

    #[test]
    fn merge_indexed_columns_single_index_becomes_scalar() {
        // Single indexed column treated as bare field (matches export behavior)
        let fields: HashMap<String, String> = [("subject[0]".into(), "science".into())]
            .into_iter()
            .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], ("subject".into(), serde_json::json!("science")));
    }

    #[test]
    fn merge_indexed_columns_remove_tag_filters_entries() {
        let fields: HashMap<String, String> = [
            ("subject[0]".into(), "science".into()),
            ("subject[1]".into(), "REMOVE_TAG".into()),
            ("subject[2]".into(), "nasa".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0],
            ("subject".into(), serde_json::json!(["science", "nasa"]))
        );
    }

    #[test]
    fn merge_indexed_columns_all_remove_tag_deletes_field() {
        let fields: HashMap<String, String> = [
            ("subject[0]".into(), "REMOVE_TAG".into()),
            ("subject[1]".into(), "REMOVE_TAG".into()),
        ]
        .into_iter()
        .collect();
        let merged = merge_indexed_columns(&fields);
        assert_eq!(merged.len(), 1);
        // Emits REMOVE_TAG sentinel so MetadataOp::Set removes the field
        assert_eq!(
            merged[0],
            ("subject".into(), serde_json::json!("REMOVE_TAG"))
        );
    }

    // -- read_identifiers_from_file tests --

    #[test]
    fn read_identifiers_from_csv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.csv");
        std::fs::write(&path, "identifier,title\nnasa,NASA\nmars,Mars\n").unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa", "mars"]);
    }

    #[test]
    fn read_identifiers_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.jsonl");
        std::fs::write(
            &path,
            "{\"identifier\":\"nasa\"}\n{\"identifier\":\"mars\"}\n",
        )
        .unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa", "mars"]);
    }

    #[test]
    fn read_identifiers_from_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ids.txt");
        std::fs::write(&path, "nasa\nmars\n# comment\n\napollo\n").unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa", "mars", "apollo"]);
    }

    #[test]
    fn read_identifiers_from_extensionless_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("myids");
        std::fs::write(&path, "nasa\nmars\n").unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa", "mars"]);
    }

    #[test]
    fn read_identifiers_from_tsv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.tsv");
        std::fs::write(&path, "identifier\ttitle\nnasa\tNASA\n").unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa"]);
    }

    #[test]
    fn read_identifiers_file_not_found() {
        let path = std::path::Path::new("/nonexistent/file.csv");
        assert!(read_identifiers_from_file(path).is_err());
    }

    #[test]
    fn read_identifiers_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dupes.txt");
        std::fs::write(&path, "nasa\nmars\nnasa\n").unwrap();

        let ids = read_identifiers_from_file(&path).unwrap();
        assert_eq!(ids, vec!["nasa", "mars"]);
    }
}
