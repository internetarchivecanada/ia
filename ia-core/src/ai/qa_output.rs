//! QA result output writers — XLSX, CSV, TSV, and JSONL.
//!
//! Converts a collection of [`QaResult`]s into structured output files for
//! human review. XLSX produces a two-sheet workbook (Items summary + Fields
//! detail); CSV/TSV produces the Fields layout as a flat table.

use std::io::Write;
use std::path::Path;

use crate::ai::qa::{FieldVerdict, PageSent, QaResult, QaVerdict};
use crate::error::{IaError, Result};

// ── Public API ────────────────────────────────────────────────────────

/// Write QA results to a file, inferring format from the extension.
///
/// Supported extensions: `.xlsx`, `.jsonl`, `.csv`, `.tsv`.
pub fn write_results(results: &[QaResult], path: &Path) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "xlsx" => write_xlsx(results, path),
        "jsonl" => write_jsonl(results, path),
        "csv" => write_delimited(results, path, b','),
        "tsv" => write_delimited(results, path, b'\t'),
        _ => Err(IaError::Config(format!(
            "unsupported output format: .{ext} (expected .xlsx, .jsonl, .csv, or .tsv)"
        ))),
    }
}

/// Validate that an output path has a supported extension.
#[must_use]
pub fn is_supported_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "xlsx" | "jsonl" | "csv" | "tsv"
    )
}

// ── JSONL ─────────────────────────────────────────────────────────────

/// Write QA results as JSONL (one JSON object per line).
fn write_jsonl(results: &[QaResult], path: &Path) -> Result<()> {
    let file = std::fs::File::create(path).map_err(|e| IaError::Config(e.to_string()))?;
    let mut writer = std::io::BufWriter::new(file);

    for result in results {
        let line = serde_json::to_string(result)
            .map_err(|e| IaError::Config(format!("failed to serialize QaResult: {e}")))?;
        writeln!(writer, "{line}")
            .map_err(|e| IaError::Config(format!("failed to write JSONL: {e}")))?;
    }

    writer
        .flush()
        .map_err(|e| IaError::Config(format!("failed to flush JSONL: {e}")))?;
    Ok(())
}

/// Read QA results from a JSONL file (one `QaResult` per line).
pub fn read_jsonl(path: &Path) -> Result<Vec<QaResult>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| IaError::Config(format!("failed to read {}: {e}", path.display())))?;

    let mut results = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let result: QaResult = serde_json::from_str(trimmed).map_err(|e| {
            IaError::Config(format!("failed to parse QaResult on line {}: {e}", i + 1))
        })?;
        results.push(result);
    }

    Ok(results)
}

// ── CSV/TSV ───────────────────────────────────────────────────────────

/// CSV/TSV header for the Fields layout.
const FIELDS_HEADERS: &[&str] = &[
    "identifier",
    "field",
    "existing_value",
    "extracted_value",
    "verdict",
    "confidence",
    "suggested_correction",
    "note",
    "pages_sent",
];

/// Write QA results as CSV or TSV (Fields layout — one row per field per item).
fn write_delimited(results: &[QaResult], path: &Path, delimiter: u8) -> Result<()> {
    let file = std::fs::File::create(path).map_err(|e| IaError::Config(e.to_string()))?;
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(file);

    wtr.write_record(FIELDS_HEADERS)
        .map_err(|e| IaError::Config(format!("failed to write CSV header: {e}")))?;

    for result in results {
        let pages_sent_str = format_pages_sent(result.pages_sent.as_deref());
        let item_url = format!("https://archive.org/details/{}", result.identifier);

        for (field_name, field_result) in &result.fields {
            let existing = result
                .existing_metadata
                .as_ref()
                .and_then(|m| m.get(field_name))
                .map(flatten_value)
                .unwrap_or_default();

            let extracted = flatten_value(&field_result.extracted_value);

            let verdict = match field_result.verdict {
                FieldVerdict::Correct => "correct",
                FieldVerdict::Incorrect => "incorrect",
                FieldVerdict::Uncertain => "uncertain",
            };

            let confidence = format!("{:.0}%", field_result.confidence * 100.0);

            let correction = field_result
                .suggested_correction
                .as_ref()
                .map(flatten_value)
                .unwrap_or_default();

            let note = field_result.note.as_deref().unwrap_or("");

            wtr.write_record([
                item_url.as_str(),
                field_name.as_str(),
                &existing,
                &extracted,
                verdict,
                &confidence,
                &correction,
                note,
                &pages_sent_str,
            ])
            .map_err(|e| IaError::Config(format!("failed to write CSV row: {e}")))?;
        }
    }

    wtr.flush()
        .map_err(|e| IaError::Config(format!("failed to flush CSV: {e}")))?;
    Ok(())
}

// ── XLSX ──────────────────────────────────────────────────────────────

/// Write QA results as an XLSX workbook with two sheets:
/// - "Items" — one row per item (summary)
/// - "Fields" — one row per field per item (detail)
fn write_xlsx(results: &[QaResult], path: &Path) -> Result<()> {
    use rust_xlsxwriter::{Color, Format, FormatBorder, FormatUnderline, Url, Workbook};

    let mut workbook = Workbook::new();

    // Shared formats
    let header_fmt = Format::new().set_bold();

    // Alternating row bands — background tint to group items visually.
    // Explicit thin borders preserve gridlines in Google Sheets (which
    // hides default gridlines when cells have a background color).
    let band_a = Color::RGB(0xE8EEF4); // blue-gray
    let band_b = Color::White;
    let grid = Color::RGB(0xD0D0D0); // light gray gridlines

    let band_a_fmt = Format::new()
        .set_background_color(band_a)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);
    let band_b_fmt = Format::new()
        .set_background_color(band_b)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);
    let band_a_pct = Format::new()
        .set_num_format("0%")
        .set_background_color(band_a)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);
    let band_b_pct = Format::new()
        .set_num_format("0%")
        .set_background_color(band_b)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);
    let band_a_link = Format::new()
        .set_font_color(Color::Blue)
        .set_underline(FormatUnderline::Single)
        .set_background_color(band_a)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);
    let band_b_link = Format::new()
        .set_font_color(Color::Blue)
        .set_underline(FormatUnderline::Single)
        .set_background_color(band_b)
        .set_border(FormatBorder::Thin)
        .set_border_color(grid);

    // ── Sheet 1: Items ────────────────────────────────────────────────
    let items_sheet = workbook.add_worksheet();
    items_sheet
        .set_name("Items")
        .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

    // Freeze header row
    items_sheet
        .set_freeze_panes(1, 0)
        .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

    let items_headers = [
        "identifier",
        "verdict",
        "confidence",
        "fields",
        "pass",
        "fail",
        "uncertain",
        "extraction_model",
        "qa_model",
        "elapsed_ms",
        "cover_link",
        "title_link",
        "other_pages",
    ];

    // Column widths for Items sheet
    let items_widths: &[f64] = &[
        24.0, // identifier
        12.0, // verdict
        12.0, // confidence
        8.0,  // fields
        8.0,  // pass
        8.0,  // fail
        10.0, // uncertain
        20.0, // extraction_model
        20.0, // qa_model
        12.0, // elapsed_ms
        12.0, // cover_link
        12.0, // title_link
        16.0, // other_pages
    ];
    for (col, &width) in items_widths.iter().enumerate() {
        items_sheet
            .set_column_width(col as u16, width)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
    }

    for (col, header) in items_headers.iter().enumerate() {
        items_sheet
            .write_string_with_format(0, col as u16, *header, &header_fmt)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
    }

    for (row_idx, result) in results.iter().enumerate() {
        let row = (row_idx + 1) as u32;
        let item_url = format!("https://archive.org/details/{}", result.identifier);
        let bg = if row_idx % 2 == 0 {
            &band_a_fmt
        } else {
            &band_b_fmt
        };
        let bg_pct = if row_idx % 2 == 0 {
            &band_a_pct
        } else {
            &band_b_pct
        };
        let bg_link = if row_idx % 2 == 0 {
            &band_a_link
        } else {
            &band_b_link
        };

        // identifier (hyperlink)
        items_sheet
            .write_url_with_text(row, 0, Url::new(&item_url), &result.identifier)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
        items_sheet
            .set_cell_format(row, 0, bg_link)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // verdict
        let verdict_str = match result.verdict {
            QaVerdict::Pass => "pass",
            QaVerdict::Fail => "fail",
            QaVerdict::NeedsReview => "needs_review",
        };
        items_sheet
            .write_string_with_format(row, 1, verdict_str, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // confidence (as fraction for % format)
        items_sheet
            .write_number_with_format(row, 2, result.overall_confidence, bg_pct)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // field counts
        let total = result.fields.len() as f64;
        let pass_count = result
            .fields
            .values()
            .filter(|f| f.verdict == FieldVerdict::Correct)
            .count() as f64;
        let fail_count = result
            .fields
            .values()
            .filter(|f| f.verdict == FieldVerdict::Incorrect)
            .count() as f64;
        let uncertain_count = result
            .fields
            .values()
            .filter(|f| f.verdict == FieldVerdict::Uncertain)
            .count() as f64;

        items_sheet
            .write_number_with_format(row, 3, total, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
        items_sheet
            .write_number_with_format(row, 4, pass_count, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
        items_sheet
            .write_number_with_format(row, 5, fail_count, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
        items_sheet
            .write_number_with_format(row, 6, uncertain_count, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // extraction_model, qa_model
        items_sheet
            .write_string_with_format(row, 7, &result.extraction_model, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
        items_sheet
            .write_string_with_format(row, 8, &result.qa_model, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // elapsed_ms
        items_sheet
            .write_number_with_format(row, 9, result.elapsed_ms as f64, bg)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

        // Page links from pages_sent
        if let Some(ref pages) = result.pages_sent {
            let cover = pages.iter().find(|p| p.page_type == "cover");
            let title = pages.iter().find(|p| p.page_type == "title");
            let others: Vec<String> = pages
                .iter()
                .filter(|p| p.page_type != "cover" && p.page_type != "title")
                .map(|p| format!("n{}", p.leaf_num))
                .collect();

            // cover_link
            if let Some(c) = cover {
                let url = c.page_url(&result.identifier);
                items_sheet
                    .write_url_with_text(row, 10, Url::new(&url), format!("n{}", c.leaf_num))
                    .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
                items_sheet
                    .set_cell_format(row, 10, bg_link)
                    .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
            }

            // title_link
            if let Some(t) = title {
                let url = t.page_url(&result.identifier);
                items_sheet
                    .write_url_with_text(row, 11, Url::new(&url), format!("n{}", t.leaf_num))
                    .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
                items_sheet
                    .set_cell_format(row, 11, bg_link)
                    .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
            }

            // other_pages
            if !others.is_empty() {
                items_sheet
                    .write_string_with_format(row, 12, others.join(", "), bg)
                    .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
            }
        }
    }

    // ── Sheet 2: Fields ───────────────────────────────────────────────
    let fields_sheet = workbook.add_worksheet();
    fields_sheet
        .set_name("Fields")
        .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

    // Freeze header row
    fields_sheet
        .set_freeze_panes(1, 0)
        .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

    // Column widths for Fields sheet
    let fields_widths: &[f64] = &[
        24.0, // identifier
        16.0, // field
        28.0, // existing_value
        28.0, // extracted_value
        12.0, // verdict
        12.0, // confidence
        28.0, // suggested_correction
        36.0, // note
        28.0, // pages_sent
    ];
    for (col, &width) in fields_widths.iter().enumerate() {
        fields_sheet
            .set_column_width(col as u16, width)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
    }

    for (col, header) in FIELDS_HEADERS.iter().enumerate() {
        fields_sheet
            .write_string_with_format(0, col as u16, *header, &header_fmt)
            .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
    }

    let mut field_row: u32 = 1;
    for (item_idx, result) in results.iter().enumerate() {
        let item_url = format!("https://archive.org/details/{}", result.identifier);
        let pages_sent_str = format_pages_sent(result.pages_sent.as_deref());
        // Band color alternates per item, not per row
        let bg = if item_idx % 2 == 0 {
            &band_a_fmt
        } else {
            &band_b_fmt
        };
        let bg_pct = if item_idx % 2 == 0 {
            &band_a_pct
        } else {
            &band_b_pct
        };
        let bg_link = if item_idx % 2 == 0 {
            &band_a_link
        } else {
            &band_b_link
        };

        for (field_name, field_result) in &result.fields {
            // identifier (hyperlink)
            fields_sheet
                .write_url_with_text(field_row, 0, Url::new(&item_url), &result.identifier)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;
            fields_sheet
                .set_cell_format(field_row, 0, bg_link)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // field
            fields_sheet
                .write_string_with_format(field_row, 1, field_name, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // existing_value
            let existing = result
                .existing_metadata
                .as_ref()
                .and_then(|m| m.get(field_name))
                .map(flatten_value)
                .unwrap_or_default();
            fields_sheet
                .write_string_with_format(field_row, 2, &existing, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // extracted_value
            let extracted = flatten_value(&field_result.extracted_value);
            fields_sheet
                .write_string_with_format(field_row, 3, &extracted, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // verdict
            let verdict = match field_result.verdict {
                FieldVerdict::Correct => "correct",
                FieldVerdict::Incorrect => "incorrect",
                FieldVerdict::Uncertain => "uncertain",
            };
            fields_sheet
                .write_string_with_format(field_row, 4, verdict, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // confidence (as fraction for % format)
            fields_sheet
                .write_number_with_format(field_row, 5, field_result.confidence, bg_pct)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // suggested_correction
            let correction = field_result
                .suggested_correction
                .as_ref()
                .map(flatten_value)
                .unwrap_or_default();
            fields_sheet
                .write_string_with_format(field_row, 6, &correction, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // note
            let note = field_result.note.as_deref().unwrap_or("");
            fields_sheet
                .write_string_with_format(field_row, 7, note, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            // pages_sent
            fields_sheet
                .write_string_with_format(field_row, 8, &pages_sent_str, bg)
                .map_err(|e| IaError::Config(format!("XLSX error: {e}")))?;

            field_row += 1;
        }
    }

    workbook
        .save(path)
        .map_err(|e| IaError::Config(format!("failed to save XLSX: {e}")))?;

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Flatten a JSON value to a display string.
///
/// Arrays are joined with `"; "` (semicolon-space). Objects and other types
/// use their JSON representation. Strings are returned directly.
pub fn flatten_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("; "),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Object(_) => value.to_string(),
    }
}

/// Format pages_sent as a human-readable string.
///
/// Groups by type: `"cover:n0, title:n3, normal:n5,n7,n9"`.
fn format_pages_sent(pages: Option<&[PageSent]>) -> String {
    let Some(pages) = pages else {
        return String::new();
    };
    if pages.is_empty() {
        return String::new();
    }

    // Group pages by type, preserving order of first appearance
    let mut groups: Vec<(&str, Vec<u32>)> = Vec::new();
    for page in pages {
        if let Some(group) = groups.iter_mut().find(|(t, _)| *t == page.page_type) {
            group.1.push(page.leaf_num);
        } else {
            groups.push((&page.page_type, vec![page.leaf_num]));
        }
    }

    groups
        .iter()
        .map(|(page_type, leaf_nums)| {
            let nums = leaf_nums
                .iter()
                .map(|n| format!("n{n}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("{page_type}:{nums}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::qa::{FieldQaResult, FieldVerdict, QaResult, QaVerdict};

    /// Build a test QaResult with enrichment fields populated.
    fn make_test_result(identifier: &str) -> QaResult {
        let mut fields = indexmap::IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("A Test Book"),
                verdict: FieldVerdict::Correct,
                confidence: 0.95,
                suggested_correction: None,
                note: None,
            },
        );
        fields.insert(
            "date".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("1987"),
                verdict: FieldVerdict::Incorrect,
                confidence: 0.9,
                suggested_correction: Some(serde_json::json!("1988")),
                note: Some("Year on cover is 1988".to_string()),
            },
        );
        fields.insert(
            "subjects".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!(["science", "nasa", "history"]),
                verdict: FieldVerdict::Uncertain,
                confidence: 0.4,
                suggested_correction: None,
                note: Some("not visible in images".to_string()),
            },
        );

        let mut existing = serde_json::Map::new();
        existing.insert("title".to_string(), serde_json::json!("A Test Book"));
        existing.insert("date".to_string(), serde_json::json!("1987"));
        existing.insert(
            "subjects".to_string(),
            serde_json::json!(["science", "space"]),
        );

        QaResult {
            identifier: identifier.to_string(),
            overall_confidence: 0.75,
            verdict: QaVerdict::Fail,
            extraction_model: "gpt-5-nano".to_string(),
            qa_model: "claude-sonnet-4-6".to_string(),
            fields,
            token_usage: None,
            elapsed_ms: 1500,
            existing_metadata: Some(existing),
            pages_sent: Some(vec![
                PageSent {
                    leaf_num: 0,
                    page_type: "cover".to_string(),
                },
                PageSent {
                    leaf_num: 3,
                    page_type: "title".to_string(),
                },
                PageSent {
                    leaf_num: 5,
                    page_type: "normal".to_string(),
                },
                PageSent {
                    leaf_num: 7,
                    page_type: "normal".to_string(),
                },
            ]),
        }
    }

    // ── flatten_value ─────────────────────────────────────────────────

    #[test]
    fn flatten_string() {
        assert_eq!(flatten_value(&serde_json::json!("hello")), "hello");
    }

    #[test]
    fn flatten_array_strings() {
        let val = serde_json::json!(["science", "nasa", "history"]);
        assert_eq!(flatten_value(&val), "science; nasa; history");
    }

    #[test]
    fn flatten_array_mixed() {
        let val = serde_json::json!(["text", 42, true]);
        assert_eq!(flatten_value(&val), "text; 42; true");
    }

    #[test]
    fn flatten_null() {
        assert_eq!(flatten_value(&serde_json::Value::Null), "");
    }

    #[test]
    fn flatten_number() {
        assert_eq!(flatten_value(&serde_json::json!(42)), "42");
    }

    #[test]
    fn flatten_bool() {
        assert_eq!(flatten_value(&serde_json::json!(true)), "true");
    }

    // ── format_pages_sent ─────────────────────────────────────────────

    #[test]
    fn format_pages_sent_groups_by_type() {
        let pages = vec![
            PageSent {
                leaf_num: 0,
                page_type: "cover".to_string(),
            },
            PageSent {
                leaf_num: 3,
                page_type: "title".to_string(),
            },
            PageSent {
                leaf_num: 5,
                page_type: "normal".to_string(),
            },
            PageSent {
                leaf_num: 7,
                page_type: "normal".to_string(),
            },
            PageSent {
                leaf_num: 9,
                page_type: "normal".to_string(),
            },
        ];
        assert_eq!(
            format_pages_sent(Some(&pages)),
            "cover:n0, title:n3, normal:n5,n7,n9"
        );
    }

    #[test]
    fn format_pages_sent_none() {
        assert_eq!(format_pages_sent(None), "");
    }

    #[test]
    fn format_pages_sent_empty() {
        assert_eq!(format_pages_sent(Some(&[])), "");
    }

    // ── page_url ──────────────────────────────────────────────────────

    #[test]
    fn page_sent_url_construction() {
        let page = PageSent {
            leaf_num: 5,
            page_type: "normal".to_string(),
        };
        assert_eq!(
            page.page_url("my-item"),
            "https://archive.org/details/my-item/page/n5/mode/2up"
        );
    }

    #[test]
    fn page_sent_url_leaf_zero() {
        let page = PageSent {
            leaf_num: 0,
            page_type: "cover".to_string(),
        };
        assert_eq!(
            page.page_url("test123"),
            "https://archive.org/details/test123/page/n0/mode/2up"
        );
    }

    // ── PageSent serde ────────────────────────────────────────────────

    #[test]
    fn page_sent_serde_roundtrip() {
        let page = PageSent {
            leaf_num: 5,
            page_type: "title".to_string(),
        };
        let json = serde_json::to_string(&page).unwrap();
        let parsed: PageSent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, page);
    }

    // ── Backward compat ───────────────────────────────────────────────

    #[test]
    fn qa_result_without_enrichment_deserializes() {
        // Simulate old JSONL format: no existing_metadata or pages_sent
        let json = r#"{
            "identifier": "old-item",
            "overall_confidence": 0.9,
            "verdict": "pass",
            "extraction_model": "gpt-5-nano",
            "qa_model": "claude-sonnet-4-6",
            "fields": {
                "title": {
                    "extracted_value": "Old Book",
                    "verdict": "correct",
                    "confidence": 0.95
                }
            },
            "token_usage": null,
            "elapsed_ms": 500
        }"#;

        let result: QaResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.identifier, "old-item");
        assert!(result.existing_metadata.is_none());
        assert!(result.pages_sent.is_none());
        assert_eq!(result.fields.len(), 1);
    }

    #[test]
    fn qa_result_with_enrichment_roundtrip() {
        let result = make_test_result("test-item");
        let json = serde_json::to_string(&result).unwrap();
        let parsed: QaResult = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.identifier, "test-item");
        assert!(parsed.existing_metadata.is_some());
        assert_eq!(parsed.existing_metadata.as_ref().unwrap().len(), 3);
        assert!(parsed.pages_sent.is_some());
        assert_eq!(parsed.pages_sent.as_ref().unwrap().len(), 4);
    }

    // ── JSONL round-trip ──────────────────────────────────────────────

    #[test]
    fn jsonl_write_and_read_back() {
        let results = vec![make_test_result("item-1"), make_test_result("item-2")];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.jsonl");

        write_jsonl(&results, &path).unwrap();
        let read_back = read_jsonl(&path).unwrap();

        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].identifier, "item-1");
        assert_eq!(read_back[1].identifier, "item-2");
        assert!(read_back[0].existing_metadata.is_some());
        assert!(read_back[0].pages_sent.is_some());
    }

    // ── XLSX write + read back with calamine ──────────────────────────

    #[test]
    fn xlsx_write_and_read_back() {
        use calamine::{open_workbook, Reader, Xlsx};

        let results = vec![make_test_result("item-1"), make_test_result("item-2")];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.xlsx");

        write_xlsx(&results, &path).unwrap();

        // Read back with calamine
        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let sheets = wb.sheet_names().to_vec();
        assert_eq!(sheets, vec!["Items", "Fields"]);

        // ── Items sheet ───────────────────────────────────────────────
        let items = wb.worksheet_range("Items").unwrap();
        // Header + 2 data rows
        assert_eq!(items.rows().count(), 3);

        let header: Vec<String> = items
            .rows()
            .next()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(header[0], "identifier");
        assert_eq!(header[1], "verdict");
        assert_eq!(header[2], "confidence");
        assert_eq!(header[3], "fields");
        assert_eq!(header[4], "pass");
        assert_eq!(header[5], "fail");
        assert_eq!(header[6], "uncertain");
        assert_eq!(header[7], "extraction_model");
        assert_eq!(header[8], "qa_model");
        assert_eq!(header[9], "elapsed_ms");
        assert_eq!(header[10], "cover_link");
        assert_eq!(header[11], "title_link");
        assert_eq!(header[12], "other_pages");

        // Row 1 data
        let row1: Vec<String> = items
            .rows()
            .nth(1)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        // identifier (calamine may show the display text or the URL)
        assert!(row1[0].contains("item-1"));
        assert_eq!(row1[1], "fail");
        // confidence: 0.75 → "0.75" (calamine reads as number)
        assert!(row1[2].starts_with("0.75"));
        // field counts: 3 total, 1 pass, 1 fail, 1 uncertain
        assert_eq!(row1[3], "3");
        assert_eq!(row1[4], "1");
        assert_eq!(row1[5], "1");
        assert_eq!(row1[6], "1");
        // models
        assert_eq!(row1[7], "gpt-5-nano");
        assert_eq!(row1[8], "claude-sonnet-4-6");
        // elapsed_ms
        assert_eq!(row1[9], "1500");
        // other_pages
        assert_eq!(row1[12], "n5, n7");

        // ── Fields sheet ──────────────────────────────────────────────
        let fields = wb.worksheet_range("Fields").unwrap();
        // Header + 3 fields × 2 items = 7 rows
        assert_eq!(fields.rows().count(), 7);

        let fheader: Vec<String> = fields
            .rows()
            .next()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(fheader[0], "identifier");
        assert_eq!(fheader[1], "field");
        assert_eq!(fheader[2], "existing_value");
        assert_eq!(fheader[3], "extracted_value");
        assert_eq!(fheader[4], "verdict");
        assert_eq!(fheader[5], "confidence");
        assert_eq!(fheader[6], "suggested_correction");
        assert_eq!(fheader[7], "note");
        assert_eq!(fheader[8], "pages_sent");

        // First field row: title
        let frow1: Vec<String> = fields
            .rows()
            .nth(1)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert!(frow1[0].contains("item-1"));
        assert_eq!(frow1[1], "title");
        assert_eq!(frow1[2], "A Test Book"); // existing
        assert_eq!(frow1[3], "A Test Book"); // extracted
        assert_eq!(frow1[4], "correct");
        assert!(frow1[5].starts_with("0.95"));

        // Second field row: date (incorrect, has correction + note)
        let frow2: Vec<String> = fields
            .rows()
            .nth(2)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(frow2[1], "date");
        assert_eq!(frow2[2], "1987"); // existing
        assert_eq!(frow2[3], "1987"); // extracted
        assert_eq!(frow2[4], "incorrect");
        assert_eq!(frow2[6], "1988"); // correction
        assert_eq!(frow2[7], "Year on cover is 1988"); // note

        // Third field row: subjects (multi-value)
        let frow3: Vec<String> = fields
            .rows()
            .nth(3)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(frow3[1], "subjects");
        assert_eq!(frow3[2], "science; space"); // existing (array flattened)
        assert_eq!(frow3[3], "science; nasa; history"); // extracted (array flattened)
        assert_eq!(frow3[4], "uncertain");

        // pages_sent on fields sheet
        assert_eq!(frow1[8], "cover:n0, title:n3, normal:n5,n7");
    }

    // ── CSV write + read back ─────────────────────────────────────────

    #[test]
    fn csv_write_and_read_back() {
        let results = vec![make_test_result("csv-item")];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.csv");

        write_delimited(&results, &path, b',').unwrap();

        let mut rdr = csv::Reader::from_path(&path).unwrap();
        let headers = rdr.headers().unwrap().clone();
        assert_eq!(headers.get(0).unwrap(), "identifier");
        assert_eq!(headers.get(1).unwrap(), "field");
        assert_eq!(headers.get(2).unwrap(), "existing_value");

        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 3); // 3 fields

        // First row: title
        assert!(records[0].get(0).unwrap().contains("archive.org")); // plain URL in CSV
        assert_eq!(records[0].get(1).unwrap(), "title");
        assert_eq!(records[0].get(3).unwrap(), "A Test Book");
        assert_eq!(records[0].get(4).unwrap(), "correct");

        // Third row: subjects (multi-value flattened)
        assert_eq!(records[2].get(1).unwrap(), "subjects");
        assert_eq!(records[2].get(3).unwrap(), "science; nasa; history");
    }

    // ── TSV write + read back ─────────────────────────────────────────

    #[test]
    fn tsv_write_and_read_back() {
        let results = vec![make_test_result("tsv-item")];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.tsv");

        write_delimited(&results, &path, b'\t').unwrap();

        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(b'\t')
            .from_path(&path)
            .unwrap();

        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].get(1).unwrap(), "title");
    }

    // ── write_results dispatches by extension ─────────────────────────

    #[test]
    fn write_results_xlsx() {
        let results = vec![make_test_result("dispatch-test")];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.xlsx");
        write_results(&results, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn write_results_csv() {
        let results = vec![make_test_result("dispatch-test")];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        write_results(&results, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn write_results_unsupported_extension() {
        let results = vec![make_test_result("dispatch-test")];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        let err = write_results(&results, &path).unwrap_err();
        assert!(err.to_string().contains("unsupported output format"));
    }

    #[test]
    fn is_supported_extension_checks() {
        assert!(is_supported_extension(Path::new("out.xlsx")));
        assert!(is_supported_extension(Path::new("out.jsonl")));
        assert!(is_supported_extension(Path::new("out.csv")));
        assert!(is_supported_extension(Path::new("out.tsv")));
        assert!(!is_supported_extension(Path::new("out.pdf")));
        assert!(!is_supported_extension(Path::new("out.txt")));
        assert!(!is_supported_extension(Path::new("noext")));
    }

    // ── Empty results ─────────────────────────────────────────────────

    #[test]
    fn xlsx_empty_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.xlsx");
        write_xlsx(&[], &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn jsonl_empty_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        write_jsonl(&[], &path).unwrap();
        let read_back = read_jsonl(&path).unwrap();
        assert!(read_back.is_empty());
    }

    // ── No enrichment fields (columns blank) ──────────────────────────

    #[test]
    fn xlsx_without_enrichment() {
        use calamine::{open_workbook, Reader, Xlsx};

        let mut result = make_test_result("no-enrich");
        result.existing_metadata = None;
        result.pages_sent = None;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-enrich.xlsx");
        write_xlsx(&[result], &path).unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();

        // Items sheet: cover_link, title_link, other_pages should be empty
        let items = wb.worksheet_range("Items").unwrap();
        let row1: Vec<String> = items
            .rows()
            .nth(1)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        // Columns 10-12 should be empty (calamine returns "" for empty cells)
        assert_eq!(row1.get(10).map(|s| s.as_str()).unwrap_or(""), "");
        assert_eq!(row1.get(11).map(|s| s.as_str()).unwrap_or(""), "");
        assert_eq!(row1.get(12).map(|s| s.as_str()).unwrap_or(""), "");

        // Fields sheet: existing_value and pages_sent should be empty
        let fields = wb.worksheet_range("Fields").unwrap();
        let frow: Vec<String> = fields
            .rows()
            .nth(1)
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(frow[2], ""); // existing_value
        assert_eq!(frow[8], ""); // pages_sent
    }

    // ── from-results round-trip: QaResult → JSONL → XLSX ──────────────

    #[test]
    fn from_results_roundtrip_jsonl_to_xlsx() {
        use calamine::{open_workbook, Reader, Xlsx};

        let results = vec![make_test_result("rt-1"), make_test_result("rt-2")];

        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("results.jsonl");
        let xlsx_path = dir.path().join("results.xlsx");

        // Write JSONL
        write_jsonl(&results, &jsonl_path).unwrap();

        // Read back (simulating --from-results)
        let loaded = read_jsonl(&jsonl_path).unwrap();
        assert_eq!(loaded.len(), 2);

        // Write XLSX from loaded results
        write_xlsx(&loaded, &xlsx_path).unwrap();

        // Verify XLSX
        let mut wb: Xlsx<_> = open_workbook(&xlsx_path).unwrap();
        let items = wb.worksheet_range("Items").unwrap();
        assert_eq!(items.rows().count(), 3); // header + 2 items
    }
}
