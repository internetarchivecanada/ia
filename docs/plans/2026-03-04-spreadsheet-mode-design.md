# Metadata Spreadsheet Mode

**Date**: 2026-03-04
**Status**: Approved
**References**:
- `docs/plans/2026-02-22-metadata-write-design.md` — Core metadata write architecture
- `docs/plans/2026-03-03-cli-subcommand-restructuring-design.md` — CLI restructuring (export/import subcommands)
- Python `internetarchive` docs: https://archive.org/developers/internetarchive/cli.html

## Overview

Spreadsheet mode enables bulk metadata operations via structured files (CSV, TSV, XLSX, ODS, JSONL). It has two sides:

- **Export** (`ia metadata export -o file.csv`) — Read metadata from multiple items into a tabular file
- **Import** (`ia metadata import file.csv`) — Write metadata from a tabular file to multiple items

This document specifies the complete data pipeline for both directions, including multi-value field handling, column conventions, type coercion, and edge cases.

## Supported Formats

| Format | Extension | Read (Import) | Write (Export) | Crate |
|--------|-----------|:---:|:---:|-------|
| CSV | `.csv` | Yes | Yes | `csv` 1 |
| TSV | `.tsv` | Yes | Yes | `csv` 1 (tab delimiter) |
| XLSX | `.xlsx` | Yes | Yes | `calamine` (read), `rust_xlsxwriter` (write) |
| ODS | `.ods` | Yes | No | `calamine` (read only) |
| JSONL | `.jsonl` | Yes | Yes | `serde_json` |

ODS is read-only because `calamine` only reads spreadsheets; `rust_xlsxwriter` only writes XLSX. Exporting to ODS is not supported — use XLSX or CSV instead.

Format is auto-detected from the file extension.

---

## Export Pipeline

### Command

```
ia metadata export -o data.csv --search 'collection:nasa'
ia metadata export -o data.xlsx --itemlist ids.txt
ia metadata export --search 'query'                      # JSONL to stdout (default)
```

### Data Flow

```
Items (from --search / --itemlist / positional / stdin)
  │
  ▼
GET /metadata/{identifier} for each item
  │
  ▼
ItemMetadata.metadata (serde_json::Value object)
  │
  ▼
Flatten to tabular columns (file mode) or emit as JSON (JSONL mode)
  │
  ▼
Write to output file or stdout
```

### JSONL Mode (default, no `-o`)

Each item is emitted as a single JSON object on one line. The raw `metadata` object from the API is used directly — no transformation. Multi-value fields remain as JSON arrays.

```jsonl
{"identifier":"item1","title":"Apollo 11","subject":["science","nasa"],...}
{"identifier":"item2","title":"Gemini 4","subject":["space"],...}
```

### File Mode (CSV/TSV/XLSX)

Metadata is flattened into tabular columns. The `identifier` column is always first.

#### Multi-Value Field Handling

IA metadata fields can be strings, arrays, numbers, or null. The key challenge is representing arrays in a flat tabular format without data loss.

**Rules:**

1. **String fields** → bare column name, string value
   - `title: "Apollo 11"` → column `title`, value `Apollo 11`

2. **Single-element arrays** → bare column name, string value (unwrapped)
   - `subject: ["science"]` → column `subject`, value `science`
   - This matches the IA API convention where single-element arrays and strings are interchangeable

3. **Multi-element arrays** → indexed columns
   - `subject: ["science", "nasa", "space"]` → columns `subject[0]`, `subject[1]`, `subject[2]`
   - Values are the individual array elements as strings

4. **Null fields** → omitted (no column)

5. **Numeric/boolean fields** → bare column name, stringified value
   - `year: 1969` → column `year`, value `1969`

**Why indexed columns?** The previous approach (joining with `"; "` separator) was lossy — it destroyed the boundary between values and couldn't round-trip fields containing semicolons. Indexed columns preserve each value exactly.

**Why unwrap single-element arrays?** The IA API treats `"science"` and `["science"]` interchangeably for most fields. Single-element arrays appear as bare columns for simplicity. On re-import, the bare column becomes a string Set — which is equivalent.

#### Column Ordering

Columns are **not** guaranteed to be in a stable order across exports. The `identifier` column is always first; all other columns appear in iteration order of the metadata JSON object. Different items may have different columns — the union of all columns across all items determines the full column set.

#### Empty Values

Empty string values are omitted from the export. An empty cell in a spreadsheet means "no value for this field on this item." It does NOT mean "set this field to empty string" (use `REMOVE_TAG` to delete a field on import).

---

## Import Pipeline

### Command

```
ia metadata import data.csv
ia metadata import data.xlsx --dry-run
ia metadata import data.csv --priority -5 --reduced-priority
```

### Data Flow

```
Read spreadsheet file
  │
  ▼
Parse into Vec<(identifier, HashMap<column_name, value>)>
  │
  ▼
Pre-process: merge indexed columns (subject[0], subject[1] → array)
  │
  ▼
Parse column names for operation prefixes
  │
  ▼
Group changes by operation type
  │
  ▼
For each item, for each group:
  GET current metadata → apply changes → compute JSON Patch → POST
```

### Column Conventions

The first column must be `identifier`. All other columns are metadata field specifications.

#### Default Operation: Modify (Set)

Bare column names default to `MetadataOp::Set` — replace the field value entirely.

| Column | Value | Effect |
|--------|-------|--------|
| `title` | `Apollo 11` | Set title to "Apollo 11" |
| `subject` | `science` | Set subject to "science" (string) |
| `date` | `1969-07-20` | Set date to "1969-07-20" |
| `bad_field` | `REMOVE_TAG` | Delete the field entirely |

#### Indexed Columns (Multi-Value Fields)

Bare columns with `[N]` suffix specify individual values of a multi-value field. They are merged into a single operation before being sent to the API.

| Columns | Values | Effect |
|---------|--------|--------|
| `subject[0]`, `subject[1]` | `science`, `nasa` | Set subject to ["science", "nasa"] |
| `subject[0]` (alone) | `science` | Set subject to "science" (scalar, same as bare `subject`) |
| `subject[0]`, `subject[1]` | `science`, `REMOVE_TAG` | Set subject to ["science"] (REMOVE_TAG filtered) |
| `subject[0]`, `subject[1]` | `REMOVE_TAG`, `REMOVE_TAG` | Delete subject field entirely |

**Merging rules:**

1. All `field[N]` columns for the same base field are collected
2. Sorted by index (so `subject[2]`, `subject[0]`, `subject[1]` → ordered correctly)
3. `REMOVE_TAG` values are filtered out
4. If one value remains → scalar Set (bare field)
5. If multiple values remain → array Set
6. If zero values remain (all REMOVE_TAG) → field deletion via REMOVE_TAG sentinel

**Important:** Indexed columns perform **whole-field replacement**, not per-index modification. `subject[0]=x` on an item with `subject: ["a", "b", "c"]` replaces the entire subject with `["x"]` (if only `subject[0]` is present) or `"x"` (scalar). This differs from Python's `ia` which modifies individual indices. For per-index operations, use the `insert:` prefix (see below).

#### Operation Prefixes

Column names can include a prefix to override the default operation:

| Prefix | Operation | Example Column | Effect |
|--------|-----------|---------------|--------|
| *(none)* | `MetadataOp::Set` | `title` | Replace field value |
| `append:` | `MetadataOp::Append` | `append:description` | Append string to field (space-separated) |
| `append-list:` | `MetadataOp::AppendList` | `append-list:subject` | Append value to array field |
| `insert:` | `MetadataOp::Insert(N)` | `insert:subject[0]` | Insert at index N in array |
| `remove:` | `MetadataOp::Remove` | `remove:subject` | Remove value from field |

Prefixed columns are NOT subject to indexed merging — `insert:subject[0]` is passed through as-is and uses the `insert:` prefix handler, not the bare indexed merge logic.

#### Mixing Operations

A single CSV can mix operations across columns:

```csv
identifier,title,append-list:subject,remove:subject
item1,New Title,astronomy,old_tag
```

This applies three changes to `item1`:
1. Set title to "New Title"
2. Append "astronomy" to subject list
3. Remove "old_tag" from subject list

Changes are grouped by operation type for efficiency. Consecutive columns with the same operation are batched into a single API call. Different operations result in separate API calls (each fetches current metadata, applies, computes patch, POSTs).

### Empty Cells

Empty cells in the spreadsheet are **skipped entirely**. An empty value for a column means "don't change this field for this item." This is critical for bulk operations where different items need different fields modified.

To explicitly delete a field, use the `REMOVE_TAG` sentinel value.

### Type Coercion

All spreadsheet values are strings. The metadata write API handles type coercion on the server side. The Rust client sends string values in the JSON Patch and the server interprets them correctly.

Exception: when indexed columns are merged into an array, the result is a JSON array of strings (e.g., `["science", "nasa"]`), which is sent as-is in the patch.

---

## Round-Trip Behavior

The export → edit → import cycle should be lossless for common metadata patterns:

| IA Metadata | Export (CSV) | Import (CSV) | Result |
|-------------|-------------|--------------|--------|
| `title: "Apollo 11"` | `title` = `Apollo 11` | Set title to "Apollo 11" | Identical |
| `subject: ["science"]` | `subject` = `science` | Set subject to "science" | String (was single-element array) — IA treats these equivalently |
| `subject: ["science", "nasa"]` | `subject[0]` = `science`, `subject[1]` = `nasa` | Set subject to ["science", "nasa"] | Identical |
| `year: 1969` | `year` = `1969` | Set year to "1969" | String (was number) — IA handles coercion |
| *(field absent)* | *(no column)* | *(no change)* | Identical |

### Known Asymmetries

1. **Single-element arrays become strings.** `subject: ["science"]` exports as bare `subject=science`, imports as string Set. The IA API treats these equivalently for most fields, but the internal representation changes from array to string.

2. **Numeric fields become strings.** `year: 1969` exports as `year=1969`, imports as `year="1969"`. Server-side coercion handles this.

3. **Field ordering is not preserved.** JSON objects are unordered. Export column order and import field order may differ from the original.

4. **Nested objects are stringified.** If a metadata field contains a nested JSON object (rare), it's stringified via `serde_json::to_string()` on export. Re-import sets it as a string, not a nested object.

---

## Differences from Python `ia`

| Behavior | Python `ia` | Rust `ia` |
|----------|------------|-----------|
| Bare indexed columns (`subject[0]`) | Per-index modification (only changes that index) | Whole-field replacement (all indexed values merged into new array) |
| Column prefixes | Not supported | `append:`, `append-list:`, `insert:`, `remove:` |
| REMOVE_TAG in indexed columns | Not supported | Filters out REMOVE_TAG entries; if all removed, deletes field |
| Default operation | `--modify` flag required | Default (bare columns = Set) |
| ODS export | Supported | Read-only (import only) |

The per-index vs whole-field difference is the most significant. In Python, `subject[0]=x` modifies only index 0 and preserves other indices. In Rust, it replaces the entire field. For per-index operations in Rust, use the `insert:` prefix.

---

## Error Handling

### Parse Errors

- Missing `identifier` column → error before any API calls
- Invalid operation prefix (e.g., `foo:subject`) → passes through as field name `foo:subject` with Set operation (colons in field names are legal but unusual)
- Empty field after known prefix (e.g., `append:`) → error

### API Errors

- Missing credentials → error on first write attempt
- 429 rate limit → all concurrent writers pause, retry after `Retry-After` seconds
- Server error → logged to joblog, continues with next item
- Zero-change patch (no actual diff) → error, logged, continues

### Immutable Fields

Columns targeting immutable fields (`identifier`, `addeddate`, `publicdate`, `uploader`) are rejected with a warning. Admin-only fields (`mediatype`, `noindex`) produce a warning but are attempted (will fail server-side for non-admin users).

---

## Implementation Details

### Key Functions

| Function | Location | Purpose |
|----------|----------|---------|
| `merge_indexed_columns()` | `ia-cli/src/commands/metadata.rs` | Pre-process: collect `field[N]` columns into arrays |
| `parse_column_op()` | `ia-cli/src/commands/metadata.rs` | Parse column prefix → `(MetadataOp, field_name)` |
| `parse_indexed_key()` | `ia-core/src/metadata/write.rs` | Parse `field[N]` → `(field, index)` |
| `prepare_metadata()` | `ia-core/src/metadata/write.rs` | Apply changes to metadata copy (Set/Append/etc.) |
| `compute_patch()` | `ia-core/src/metadata/write.rs` | Diff current vs desired → RFC 6902 JSON Patch |
| `read_spreadsheet()` | `ia-core/src/spreadsheet.rs` | Read any supported format → `Vec<(identifier, fields)>` |
| `write_spreadsheet()` | `ia-core/src/spreadsheet.rs` | Write tabular data to CSV/TSV/XLSX |

### Import Change Grouping

Import groups consecutive same-operation columns into a single API call for efficiency. This reduces the number of GET+POST round-trips per item.

```
CSV columns: identifier, title, date, append-list:subject, append-list:collection, remove:old_tag
                         ├── Set group ──┤  ├── AppendList group ──────────────┤  ├── Remove ──┤
```

Each group results in one `modify()` call: fetch current metadata, apply all changes in the group, compute patch, POST.

### Concurrency

Import uses the global `-j`/`--jobs` semaphore (default 2 concurrent items). Within a single item, change groups are applied sequentially (each group depends on the previous group's result). Across items, work proceeds concurrently up to the semaphore limit.

Rate limiting (429) pauses all concurrent workers via a shared `RateLimiter` (AtomicBool + Notify).

### Dry Run

`--dry-run` shows the computed patch for each item without sending POST requests. It still performs GET requests to fetch current metadata for accurate diff computation.

```
Dry run -- no changes will be applied

  nasa
    title: "old title" -> "NASA Image Archive"         [set]
    subject: ["space"] -> ["space", "astronomy"]       [append-list]

  2 item(s), 2 change(s)
```
