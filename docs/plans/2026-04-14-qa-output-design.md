# QA Structured Output & Result Re-processing

**Date:** 2026-04-14
**Status:** Design
**Related:** `docs/plans/2026-03-24-ai-qa-pipeline-design.md`, PR #291

## Problem

The QA pipeline produces per-field verdicts with confidence scores, but the
output options are limited:

- **Default:** Human-readable stderr display (verdict icons, confidence %).
- **`--json`:** Full `QaResult` as JSONL to stdout.
- **`--joblog`:** Operation log (identifier, status, tokens) — no field-level detail.

Users need:
1. **Spreadsheet output** for human review of QA results — curators reviewing
   verdicts in Google Sheets, with clickable links to items and pages.
2. **Result re-processing** — re-format existing results (e.g., fix an XLSX
   formatting bug) without re-calling the LLM ($0.02–0.05/item).

## Design

### Output flags

```
# Multiple -o flags: write both formats in one run
ia ai qa --search "collection:theses" -o results.xlsx -o results.jsonl

# --json to stdout (existing, unchanged)
ia ai qa --search "collection:theses" --json

# Both coexist: JSONL to stdout + XLSX to file
ia ai qa --search "collection:theses" --json -o results.xlsx

# Re-process: format cached JSONL into XLSX without LLM calls
ia ai qa --from-results results.jsonl -o results.xlsx
```

- **Multiple `-o <file>` flags** supported. Format inferred from extension:
  `.xlsx`, `.jsonl`, `.csv`, `.tsv`.
- **`--json`** remains unchanged (JSONL to stdout). Independent of `-o`.
- **`--from-results <file>`** reads a JSONL of `QaResult` objects and formats
  them into `-o` outputs. No LLM calls, no network access. This replaces the
  need for a separate cache system.
- `-o` writes are batched (all items complete before writing) since XLSX needs
  all data upfront. JSONL `-o` could theoretically stream, but batch-write
  keeps it consistent and simple.

### XLSX structure

Two sheets in a single workbook, optimized for human review in Google Sheets.

#### Sheet 1: "Items" — one row per item

| Column | Type | Content |
|---|---|---|
| `identifier` | Hyperlink | Display text is the identifier; links to `https://archive.org/details/{id}` |
| `verdict` | Text | `pass` / `fail` / `needs_review` |
| `confidence` | Percentage | Overall confidence (e.g., `92%`) |
| `fields` | Number | Total field count |
| `pass` | Number | Count of correct fields |
| `fail` | Number | Count of incorrect fields |
| `uncertain` | Number | Count of uncertain fields |
| `extraction_model` | Text | Model that performed original extraction |
| `qa_model` | Text | Model used for QA verification |
| `elapsed_ms` | Number | Wall-clock time for QA call |
| `cover_link` | Hyperlink | Link to cover page (`/page/n{N}/mode/2up`), blank if no cover |
| `title_link` | Hyperlink | Link to title page (`/page/n{N}/mode/2up`), blank if no title |
| `other_pages` | Text | Remaining pages as text: `n5, n7, n9` |

#### Sheet 2: "Fields" — one row per field per item

| Column | Type | Content |
|---|---|---|
| `identifier` | Hyperlink | Display text is the identifier; links to `https://archive.org/details/{id}` |
| `field` | Text | Metadata field name |
| `existing_value` | Text | Current value on the archive.org item |
| `extracted_value` | Text | Value the AI extraction model produced |
| `verdict` | Text | `correct` / `incorrect` / `uncertain` |
| `confidence` | Percentage | Per-field confidence (e.g., `95%`) |
| `suggested_correction` | Text | QA model's correction (if verdict is `incorrect`) |
| `note` | Text | QA model's explanatory note |
| `pages_sent` | Text | Pages sent to QA model: `cover:n0, title:n3, normal:n5,n7,n9` |

**Multi-value fields** (arrays) are flattened with `; ` (semicolon-space)
separator in all value columns. Example: `science; nasa; history`.

### Page URL construction

Archive.org BookReader URLs use 0-based leaf numbers from scandata.xml:

```
https://archive.org/details/{identifier}/page/n{leaf_num}/mode/2up
```

The leaf number maps directly to the JP2 zip member filename:
`item_0005.jp2` → leaf 5 → `page/n5/mode/2up`.

Page type labels come from scandata `pageType` attributes (cover, title,
normal, other). The QA pipeline already has this data in the `selected` pages
vector: `Vec<(String, Option<ScandataPage>)>`.

### JSONL structure

Same as current `--json` output — one JSON object per line, full `QaResult`
serialized. This is the lossless format that `--from-results` reads back.

### Enriching `QaResult` for self-contained JSONL

For `--from-results` to work offline (no network), the JSONL must contain
everything needed to produce XLSX output. Two fields are missing from the
current `QaResult`:

1. **`existing_metadata`** — the item's current metadata values for the
   fields that were QA'd.
2. **`pages_sent`** — the pages sent to the QA model, with types and leaf
   numbers.

Add these as optional fields to `QaResult`:

```rust
/// Existing metadata values from the item (for comparison in reports).
#[serde(skip_serializing_if = "Option::is_none")]
pub existing_metadata: Option<serde_json::Map<String, serde_json::Value>>,

/// Pages sent to the QA model: Vec of (leaf_num, page_type).
#[serde(skip_serializing_if = "Option::is_none")]
pub pages_sent: Option<Vec<PageSent>>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSent {
    pub leaf_num: u32,
    pub page_type: String,  // "cover", "title", "normal", "other"
}
```

These fields are `Option` so existing serialized `QaResult` values (from
current `--json` output) remain deserializable. When present, `--from-results`
uses them directly. When absent, the columns are left blank in XLSX output.

### `--from-results` behavior

1. Read JSONL file, deserialize each line as `QaResult`.
2. Apply any `-o` output formatting.
3. No LLM calls, no network, no `--joblog` interaction.
4. Default stderr display still shows per-item verdicts (useful for quick
   review of cached results).
5. `--from-results` is mutually exclusive with `--search`, `--itemlist`,
   positional identifiers, and `--promote`.

### CSV/TSV output

When `-o results.csv` or `-o results.tsv` is used, the output is the
**Fields sheet layout** (one row per field per item) since that's the most
useful flat format. The Items summary is a `GROUP BY` away in any spreadsheet
tool. Hyperlinks are written as plain text URLs in CSV/TSV.

### What doesn't change

- **Default stderr display** — verdict icons, confidence %, notes. Untouched.
- **`--joblog`** — operation log for resume/retry tracking. Complementary to
  `-o`, not redundant. `--joblog` tracks "did we process this item?" while
  `-o` captures "what were the results?"
- **`--json` to stdout** — existing behavior preserved exactly.
- **`--promote`** — works independently of `-o`.
- **`--dry-run`**, **`--print-prompt`**, **`--estimate`** — unchanged.

## Flag interaction matrix

| Flag combo | Behavior |
|---|---|
| `--json` | JSONL to stdout (existing) |
| `-o foo.xlsx` | XLSX to file |
| `-o foo.jsonl` | JSONL to file |
| `-o foo.xlsx -o foo.jsonl` | Both files written |
| `--json -o foo.xlsx` | JSONL to stdout + XLSX to file |
| `--from-results in.jsonl -o out.xlsx` | Read JSONL, write XLSX, no LLM |
| `--from-results in.jsonl --json` | Read JSONL, write to stdout |
| `--from-results in.jsonl` (no -o, no --json) | Show stderr display only |
| `--from-results` + `--search` | Error: mutually exclusive |
| `--from-results` + `--promote` | Error: mutually exclusive |

## Testing

- Unit tests for XLSX generation (write + read back with calamine).
- Unit tests for CSV/TSV generation (write + read back).
- Unit tests for `QaResult` enrichment (existing_metadata, pages_sent).
- Unit tests for `--from-results` deserialization of both old (without
  enrichment) and new (with enrichment) JSONL formats.
- Unit tests for page URL construction from leaf numbers.
- CLI integration tests for `-o`, `--from-results`, flag conflicts.
- Multi-value field flattening (array → semicolon-separated).
