use crate::error::{IaError, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

/// A single record from a spreadsheet: (identifier, field_name → value).
pub type SpreadsheetRecord = (String, HashMap<String, String>);

/// Read a spreadsheet file and return records.
/// Format auto-detected by file extension: .csv, .tsv, .xlsx, .ods, .jsonl
///
/// Header names are trimmed and compared lowercased, JSONL keys are
/// compared lowercased, `identifier` included. Values are trimmed, and a
/// blank value is no value. A CSV, TSV, XLSX or ODS row with a blank
/// identifier is skipped.
///
/// JSONL values are read by type: a string is the value; a number or a
/// boolean is the value as written in the file (`1.50` stays `1.50`, `1e3`
/// stays `1e3`); `null` is no value; an array of strings gives one value
/// per element, under the keys `key[0]`, `key[1]`, … that indexed columns
/// use.
///
/// # Errors
///
/// [`IaError::InvalidArgument`] listing every problem in the file: a header
/// given twice (blank headers aside), a missing `identifier` column, and in
/// JSONL a line that is not a JSON object, has no string identifier, gives
/// a key twice, or holds a value of another type (an object, an array
/// holding anything but strings, or an array under an indexed key). Plus
/// the read and parse errors of the file.
pub fn read_spreadsheet(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    read_records(path, Scope::AllFields)
}

/// Read back a spreadsheet this tool wrote with [`write_spreadsheet`], such
/// as an `ia metadata export` file being resumed, keeping the case of
/// header names and JSONL keys.
///
/// Item metadata keys are case-sensitive on IA, and an export writes each
/// key in the case IA returned, so `Title` and `title` are two columns. The
/// file is rewritten from what is read here, so lowercasing would rename
/// `Title` to `title` and then refuse the file as soon as a new row brings
/// `Title` back. Every other rule of [`read_spreadsheet`] applies; the
/// `identifier` header is still matched in any case. Do not use this for
/// input a person wrote: there, `Title` and `title` are the same field.
///
/// # Errors
///
/// As [`read_spreadsheet`], with a header refused only when it is given
/// twice in the same case.
pub fn read_exported_spreadsheet(path: &Path) -> Result<Vec<SpreadsheetRecord>> {
    read_records(path, Scope::AllFieldsKeepCase)
}

/// What a read checks and returns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Every field, with every rule of [`read_spreadsheet`].
    AllFields,
    /// Every field, keeping the case of names, for
    /// [`read_exported_spreadsheet`].
    AllFieldsKeepCase,
    /// The identifier only: other fields are neither checked nor returned,
    /// for [`read_identifiers_from_file`].
    IdentifierOnly,
}

impl Scope {
    /// The name a header or JSONL key is compared and stored under: trimmed
    /// headers lowercased, or kept as written for
    /// [`Scope::AllFieldsKeepCase`]. `identifier` in any case is always
    /// `identifier`.
    fn name(self, raw: &str) -> String {
        if self == Scope::AllFieldsKeepCase && !raw.eq_ignore_ascii_case("identifier") {
            raw.to_string()
        } else {
            raw.to_lowercase()
        }
    }
}

fn read_records(path: &Path, scope: Scope) -> Result<Vec<SpreadsheetRecord>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => read_csv(path, b',', scope),
        "tsv" => read_csv(path, b'\t', scope),
        "xlsx" | "ods" | "xls" => read_calamine(path, scope),
        "jsonl" | "ndjson" => read_jsonl(path, scope),
        other => Err(IaError::Config(format!(
            "unsupported spreadsheet format: .{other} (supported: .csv, .tsv, .xlsx, .ods, .jsonl)"
        ))),
    }
}

/// The error for the problems found in a file, one per line.
fn problems_error(problems: &[String]) -> IaError {
    IaError::InvalidArgument(format!(
        "spreadsheet has problems:\n  {}",
        problems.join("\n  ")
    ))
}

/// Refuse a header given twice. Rows are keyed by header, so one of the two
/// cells would be lost. Blank headers are not compared: a trailing
/// delimiter in the header row gives them.
fn check_headers(headers: &[String], scope: Scope) -> Result<()> {
    let mut seen = HashSet::new();
    let mut repeated = BTreeSet::new();
    for header in headers {
        if header.is_empty() || (scope == Scope::IdentifierOnly && header != "identifier") {
            continue;
        }
        if !seen.insert(header.as_str()) {
            repeated.insert(header.as_str());
        }
    }
    if repeated.is_empty() {
        return Ok(());
    }
    let problems: Vec<String> = repeated
        .into_iter()
        .map(|h| {
            if h == "identifier" || scope == Scope::AllFieldsKeepCase {
                format!("header '{h}' is given more than once")
            } else {
                format!(
                    "header '{h}' is given more than once \
                     (headers are case-insensitive; write several values as '{h}[0]', '{h}[1]')"
                )
            }
        })
        .collect();
    Err(problems_error(&problems))
}

fn read_csv(path: &Path, delimiter: u8, scope: Scope) -> Result<Vec<SpreadsheetRecord>> {
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
        .headers()?
        .iter()
        .map(|h| scope.name(h.trim()))
        .collect();
    check_headers(&headers, scope)?;

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| {
            IaError::InvalidArgument("spreadsheet must have an 'identifier' column".into())
        })?;

    let mut records = Vec::new();
    for result in reader.records() {
        let row = result?;
        let identifier = row.get(id_col).unwrap_or("").trim().to_string();
        if identifier.is_empty() {
            continue;
        }

        let mut fields = HashMap::new();
        for (i, value) in row.iter().enumerate() {
            if scope == Scope::IdentifierOnly || i == id_col || i >= headers.len() {
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

fn read_calamine(path: &Path, scope: Scope) -> Result<Vec<SpreadsheetRecord>> {
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
        .map(|cell| scope.name(cell.to_string().trim()))
        .collect();
    check_headers(&headers, scope)?;

    let id_col = headers
        .iter()
        .position(|h| h == "identifier")
        .ok_or_else(|| {
            IaError::InvalidArgument("spreadsheet must have an 'identifier' column".into())
        })?;

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
            if scope == Scope::IdentifierOnly || i == id_col || i >= headers.len() {
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

/// Read JSONL by value type; see [`read_spreadsheet`]. Every problem in the
/// file is collected and reported together.
fn read_jsonl(path: &Path, scope: Scope) -> Result<Vec<SpreadsheetRecord>> {
    use serde_json::value::RawValue;
    use serde_json::Value;

    let content = std::fs::read_to_string(path)?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let mut records = Vec::new();
    let mut problems = Vec::new();

    for (n, line) in content.lines().enumerate() {
        let line_no = n + 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Each value is kept as its raw JSON text, so a number is sent as
        // written (`1.50`, `1e3`), not as serde_json would print it.
        let raw = match serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(line) {
            Ok(raw) => raw,
            Err(e) => {
                problems.push(match serde_json::from_str::<Value>(line) {
                    Ok(_) => format!("line {line_no}: not a JSON object"),
                    Err(_) => format!("line {line_no}: not valid JSON: {e}"),
                });
                continue;
            }
        };
        // The line parsed, so a value that does not parse again is a number
        // too large for f64 (`1e400`); it is a number, sent as written.
        let object: Vec<(&String, &str, Value)> = raw
            .iter()
            .map(|(key, text)| {
                let value = serde_json::from_str::<Value>(text.get())
                    .unwrap_or_else(|_| Value::Number(0.into()));
                (key, text.get().trim(), value)
            })
            .collect();

        let id_values: Vec<&Value> = object
            .iter()
            .filter(|(k, _, _)| k.eq_ignore_ascii_case("identifier"))
            .map(|(_, _, v)| v)
            .collect();
        let identifier = match id_values.as_slice() {
            [Value::String(id)] if !id.trim().is_empty() => id.trim().to_string(),
            [] | [Value::Null] | [Value::String(_)] => {
                problems.push(format!("line {line_no}: no identifier"));
                continue;
            }
            [other] => {
                problems.push(format!(
                    "line {line_no}: identifier must be a string (got {})",
                    match other {
                        Value::Array(_) => "an array",
                        other => json_kind(other),
                    }
                ));
                continue;
            }
            _ => {
                problems.push(format!(
                    "line {line_no}: identifier is given more than once (keys are case-insensitive)"
                ));
                continue;
            }
        };
        if scope == Scope::IdentifierOnly {
            records.push((identifier, HashMap::new()));
            continue;
        }

        let mut fields = HashMap::new();
        let mut keys_seen = HashSet::new();
        let mut put = |key: String, value: &str, problems: &mut Vec<String>| {
            let value = value.trim();
            if value.is_empty() {
                return;
            }
            if fields.insert(key.clone(), value.to_string()).is_some() {
                problems.push(format!("line {line_no}: {key} is given more than once"));
            }
        };
        for (key, text, value) in &object {
            let key = scope.name(key);
            if key == "identifier" {
                continue;
            }
            if !keys_seen.insert(key.clone()) {
                problems.push(format!(
                    "line {line_no}: {key} is given more than once (keys are case-insensitive)"
                ));
                continue;
            }
            let indexed = key.contains('[');
            match value {
                Value::Null => {}
                Value::String(v) => put(key, v, &mut problems),
                Value::Number(_) | Value::Bool(_) => put(key, text, &mut problems),
                Value::Array(items) if !indexed && items.iter().all(Value::is_string) => {
                    for (i, item) in items.iter().enumerate() {
                        if let Value::String(v) = item {
                            put(format!("{key}[{i}]"), v, &mut problems);
                        }
                    }
                }
                other => problems.push(format!(
                    "line {line_no}: {key} must be {} (got {})",
                    if indexed {
                        "a string, a number, a boolean, or null"
                    } else {
                        "a string, a number, a boolean, an array of strings, or null"
                    },
                    match other {
                        Value::Array(_) if indexed => "an array",
                        other => json_kind(other),
                    }
                )),
            }
        }
        records.push((identifier, fields));
    }

    if problems.is_empty() {
        Ok(records)
    } else {
        Err(problems_error(&problems))
    }
}

/// A JSON value's kind, for an error message.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array that holds more than strings",
        serde_json::Value::Object(_) => "an object",
    }
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
/// .jsonl) are parsed and the `identifier` column is extracted, with the
/// identifier rules of [`read_spreadsheet`]; other columns are not read, so
/// their values may be of any type. Unrecognized
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
        let records = read_records(path, Scope::IdentifierOnly)?;
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
    fn invalid_utf8_is_csv_error_not_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.csv");
        let mut bytes = b"identifier,title\nitem1,".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
        std::fs::write(&path, bytes).unwrap();

        let err = read_spreadsheet(&path).expect_err("invalid UTF-8 must error");
        assert!(
            matches!(err, IaError::Csv(_)),
            "expected Csv classification, got: {err:?}"
        );
    }

    #[test]
    fn missing_identifier_column_is_invalid_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noid.csv");
        std::fs::write(&path, "title,creator\nfoo,bar\n").unwrap();

        let err = read_spreadsheet(&path).expect_err("missing identifier column must error");
        assert!(
            matches!(err, IaError::InvalidArgument(_)),
            "expected InvalidArgument, got: {err:?}"
        );
    }

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

    // -- value types, identifier key and repeated headers (#154) --

    fn file(dir: &tempfile::TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// The reader's error for `content`, which must be InvalidArgument.
    fn refusal(name: &str, content: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let err = read_spreadsheet(&file(&dir, name, content)).expect_err("must be refused");
        assert!(
            matches!(err, IaError::InvalidArgument(_)),
            "expected InvalidArgument, got: {err:?}"
        );
        err.to_string()
    }

    fn read(name: &str, content: &str) -> Vec<SpreadsheetRecord> {
        let dir = tempfile::tempdir().unwrap();
        read_spreadsheet(&file(&dir, name, content)).unwrap()
    }

    #[test]
    fn jsonl_null_is_no_value() {
        let records = read(
            "a.jsonl",
            r#"{"identifier":"a","collection":null,"title":"T"}"#,
        );
        assert_eq!(records.len(), 1);
        assert!(!records[0].1.contains_key("collection"), "{records:?}");
        assert_eq!(records[0].1.get("title").unwrap(), "T");
    }

    #[test]
    fn jsonl_string_array_gives_indexed_values() {
        let records = read("a.jsonl", r#"{"identifier":"a","Subject":["x"," y "]}"#);
        let fields = &records[0].1;
        assert_eq!(fields.get("subject[0]").unwrap(), "x");
        assert_eq!(fields.get("subject[1]").unwrap(), "y");
        assert!(!fields.contains_key("subject"), "{fields:?}");
    }

    #[test]
    fn jsonl_strings_are_trimmed_and_blank_is_no_value() {
        let records = read(
            "a.jsonl",
            r#"{"identifier":" a ","title":" T ","date":"  "}"#,
        );
        assert_eq!(records[0].0, "a");
        assert_eq!(records[0].1.get("title").unwrap(), "T");
        assert!(!records[0].1.contains_key("date"));
    }

    #[test]
    fn jsonl_numbers_and_booleans_are_sent_as_written() {
        let records = read(
            "a.jsonl",
            r#"{"identifier":"a","year":1999,"price":1.50,"count":1e3,"priority":-5,"big":123456789012345678901234567890,"public":true,"draft":false,"subject[0]":7,"huge":1e400}"#,
        );
        let fields = &records[0].1;
        for (key, want) in [
            ("year", "1999"),
            ("price", "1.50"),
            ("count", "1e3"),
            ("priority", "-5"),
            ("big", "123456789012345678901234567890"),
            ("public", "true"),
            ("draft", "false"),
            ("subject[0]", "7"),
            ("huge", "1e400"),
        ] {
            assert_eq!(fields.get(key).map(String::as_str), Some(want), "{key}: {fields:?}");
        }
    }

    #[test]
    fn jsonl_key_repeated_in_the_same_case_keeps_its_last_value() {
        let records = read("a.jsonl", r#"{"identifier":"a","x":"1","x":2}"#);
        assert_eq!(records[0].1.get("x").unwrap(), "2");
    }

    #[test]
    fn jsonl_out_of_range_number_as_identifier_is_refused() {
        let msg = refusal("a.jsonl", r#"{"identifier":1e400}"#);
        assert!(
            msg.contains("line 1: identifier must be a string (got a number)"),
            "{msg}"
        );
        assert!(!msg.contains("no identifier"), "{msg}");
    }

    #[test]
    fn jsonl_objects_and_mixed_arrays_are_refused_together() {
        let msg = refusal(
            "a.jsonl",
            "{\"identifier\":\"c\",\"extra\":{\"k\":\"v\"}}\n\
             {\"identifier\":\"d\",\"subject\":[\"x\",1]}\n\
             {\"identifier\":\"e\",\"subject[0]\":{}}\n",
        );
        for want in [
            "line 1: extra must be a string, a number, a boolean, an array of strings, or null (got an object)",
            "line 2: subject must be",
            "(got an array that holds more than strings)",
            "line 3: subject[0] must be a string, a number, a boolean, or null (got an object)",
        ] {
            assert!(msg.contains(want), "missing {want:?} in: {msg}");
        }
    }

    #[test]
    fn jsonl_array_under_an_indexed_key_is_refused() {
        let msg = refusal("a.jsonl", r#"{"identifier":"a","subject[0]":["x"]}"#);
        assert!(
            msg.contains("line 1: subject[0] must be a string, a number, a boolean, or null (got an array)"),
            "{msg}"
        );
    }

    #[test]
    fn jsonl_identifier_key_is_case_insensitive() {
        let records = read("a.jsonl", r#"{"Identifier":"a","title":"T"}"#);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "a");
        assert!(!records[0].1.contains_key("identifier"));
    }

    #[test]
    fn jsonl_line_without_a_string_identifier_is_refused() {
        let msg = refusal(
            "a.jsonl",
            "{\"identifier\":7,\"title\":\"T\"}\n\
             {\"title\":\"no id\"}\n\
             {\"identifier\":\"  \"}\n\
             {\"identifier\":\"a\",\"IDENTIFIER\":\"b\"}\n\
             \n\
             [1,2]\n\
             {not json\n",
        );
        for want in [
            "line 1: identifier must be a string (got a number)",
            "line 2: no identifier",
            "line 3: no identifier",
            "line 4: identifier is given more than once",
            "line 6: not a JSON object",
            "line 7: not valid JSON",
        ] {
            assert!(msg.contains(want), "missing {want:?} in: {msg}");
        }
    }

    #[test]
    fn jsonl_key_given_twice_in_different_case_is_refused() {
        let msg = refusal("a.jsonl", r#"{"identifier":"a","title":"x","Title":"y"}"#);
        assert!(
            msg.contains("line 1: title is given more than once"),
            "{msg}"
        );
    }

    #[test]
    fn csv_repeated_header_is_refused() {
        let msg = refusal("a.csv", "identifier,title,Title \na,x,y\n");
        assert!(msg.contains("'title'"), "{msg}");
        assert!(msg.contains("more than once"), "{msg}");
        assert!(
            msg.contains("title[0]"),
            "should name the indexed form: {msg}"
        );
    }

    #[test]
    fn tsv_repeated_identifier_header_is_refused() {
        let msg = refusal("a.tsv", "identifier\tIdentifier\na\tb\n");
        assert!(msg.contains("'identifier'"), "{msg}");
    }

    #[test]
    fn csv_blank_headers_are_not_compared() {
        let records = read("a.csv", "identifier,title,,\na,T,,\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.get("title").unwrap(), "T");
    }

    #[test]
    fn xlsx_repeated_header_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet = workbook.add_worksheet();
        for (col, text) in ["identifier", "subject", "SUBJECT"].iter().enumerate() {
            sheet.write_string(0, col as u16, *text).unwrap();
            sheet.write_string(1, col as u16, "v").unwrap();
        }
        workbook.save(&path).unwrap();

        let err = read_spreadsheet(&path).expect_err("must be refused");
        assert!(matches!(err, IaError::InvalidArgument(_)), "{err:?}");
        assert!(err.to_string().contains("'subject'"), "{err}");
    }

    #[test]
    fn identifiers_from_jsonl_ignore_other_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            &dir,
            "ids.jsonl",
            "{\"Identifier\":\"a\",\"downloads\":12}\n{\"identifier\":\"b\",\"x\":true}\n",
        );
        assert_eq!(read_identifiers_from_file(&path).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn identifiers_from_jsonl_still_need_an_identifier() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "ids.jsonl", "{\"identifier\":3}\n");
        let err = read_identifiers_from_file(&path).expect_err("must be refused");
        assert!(
            err.to_string()
                .contains("line 1: identifier must be a string"),
            "{err}"
        );
    }

    #[test]
    fn jsonl_identifier_array_is_called_an_array() {
        let msg = refusal("a.jsonl", r#"{"identifier":["a"]}"#);
        assert!(
            msg.contains("line 1: identifier must be a string (got an array)"),
            "{msg}"
        );
    }

    #[test]
    fn identifiers_from_csv_refuse_a_repeated_identifier_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "ids.csv", "identifier,Identifier\na,b\n");
        let err = read_identifiers_from_file(&path).expect_err("must be refused");
        assert!(err.to_string().contains("'identifier'"), "{err}");
    }

    #[test]
    fn exported_read_keeps_the_case_of_headers() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            &dir,
            "out.csv",
            "Identifier,Title,title\na,Upper,lower\nb,B,\n",
        );
        let records = read_exported_spreadsheet(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "a");
        assert_eq!(records[0].1.get("Title").unwrap(), "Upper");
        assert_eq!(records[0].1.get("title").unwrap(), "lower");
        assert_eq!(records[1].1.get("Title").unwrap(), "B");
        assert!(!records[1].1.contains_key("title"));
    }

    #[test]
    fn exported_read_refuses_a_header_repeated_in_the_same_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "out.csv", "identifier,Title,Title\na,x,y\n");
        let err = read_exported_spreadsheet(&path).expect_err("must be refused");
        assert!(matches!(err, IaError::InvalidArgument(_)), "{err:?}");
        assert!(
            err.to_string()
                .contains("header 'Title' is given more than once"),
            "{err}"
        );
    }

    #[test]
    fn exported_read_keeps_the_case_of_jsonl_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            &dir,
            "out.jsonl",
            r#"{"IDENTIFIER":"a","Title":"Upper","title":"lower"}"#,
        );
        let records = read_exported_spreadsheet(&path).unwrap();
        assert_eq!(records[0].0, "a");
        assert_eq!(records[0].1.get("Title").unwrap(), "Upper");
        assert_eq!(records[0].1.get("title").unwrap(), "lower");
        assert_eq!(records[0].1.len(), 2);
    }

    #[test]
    fn identifiers_from_csv_ignore_repeated_other_headers() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "ids.csv", "identifier,title,title\na,x,y\n");
        assert_eq!(read_identifiers_from_file(&path).unwrap(), vec!["a"]);
    }
}
