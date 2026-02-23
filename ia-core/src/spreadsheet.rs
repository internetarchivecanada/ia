use crate::error::{IaError, Result};
use std::collections::HashMap;
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
}
