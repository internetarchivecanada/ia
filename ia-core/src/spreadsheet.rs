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
        writer
            .write_record(&row)
            .map_err(std::io::Error::other)?;
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

    workbook
        .save(path)
        .map_err(std::io::Error::other)?;

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
        let csv = "identifier,title,date\nnasa,NASA Images,2024-01-01\nmars,Mars Rover,2024-06-01\n";
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
        assert!(msg.contains("import reads .ods"), "error should hint that ODS is read-only: {msg}");
    }

    #[test]
    fn write_unknown_format_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.parquet");
        let records = vec![];
        let err = write_spreadsheet(&path, &records).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported export format"), "error should say unsupported: {msg}");
    }
}
