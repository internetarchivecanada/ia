# Metadata Write Support

**Date**: 2026-02-22
**Status**: Design approved
**Prerequisite for**: `ia ai` command (see `docs/plans/2026-02-21-ai-command-design.md`)

## Overview

Add metadata write support to `ia-core` and `ia metadata` CLI. This enables setting, appending, inserting, and removing metadata fields on Internet Archive items via the metadata write API (`POST /metadata/{identifier}`).

This is a prerequisite for the `ia ai` command, the GUI metadata editor, and any other feature that modifies item metadata.

## Architecture

### Diff-Based Approach

Matches the Python `internetarchive` library's proven approach:

1. Fetch current metadata via `GET /metadata/{identifier}`
2. Apply desired changes to a copy of current metadata
3. Diff current vs desired using `json-patch` crate (RFC 6902)
4. POST the computed JSON Patch to the metadata API
5. Auth credentials appended to POST body (not headers)

Callers provide simple intent ("set title to X") and the library handles the complexity.

## Core Data Types

### MetadataOp

```rust
/// How to apply metadata changes.
pub enum MetadataOp {
    /// Set field to value (replace if exists, add if new).
    /// CLI: --modify="title:New Title"
    /// To delete a field, set value to "REMOVE_TAG".
    Set,

    /// Append string to existing string field (space-separated).
    /// CLI: --append="description:More text"
    /// "Original" + "More text" -> "Original More text"
    /// Errors if target is a list field.
    Append,

    /// Append value to a list field.
    /// CLI: --append-list="subject:physics"
    /// ["math"] + "physics" -> ["math", "physics"]
    /// If field is a string, converts to list first.
    AppendList,

    /// Insert value at specific index in list field.
    /// CLI: --insert="collection[0]:featured"
    /// Shifts existing elements right. Deduplicates (removes existing
    /// occurrence before inserting at new position).
    Insert(usize),

    /// Remove a value from a field.
    /// CLI: --remove="subject:physics"
    /// If removing the last value, the field is deleted entirely.
    /// Special handling for collection (can't remove last collection)
    /// and subject (handles semicolon-delimited legacy strings).
    Remove,
}
```

### ModifyResponse

```rust
/// Response from the metadata write API.
pub struct ModifyResponse {
    pub success: bool,
    pub task_id: Option<u64>,
    pub log: Option<String>,
    pub error: Option<String>,
}
```

### New Error Variants

```rust
enum IaError {
    // ... existing variants ...

    /// Missing S3 credentials for write operations.
    Auth(String),

    /// Metadata write rejected by server or patch computation failed.
    MetadataWrite { identifier: String, message: String },
}
```

## The `modify()` Function

```rust
pub async fn modify(
    client: &IaClient,
    identifier: &str,
    metadata: &HashMap<String, serde_json::Value>,
    op: MetadataOp,
    target: &str,                              // "metadata" or "files/foo.txt"
    expect: Option<&HashMap<String, Value>>,   // optimistic concurrency checks
    priority: Option<i32>,                     // task priority (default 0 single, -5 batch)
    reduced_priority: bool,                    // X-Accept-Reduced-Priority header
) -> Result<ModifyResponse>
```

### Internal Flow

```
1. client.require_auth()?
   -> IaError::Auth if credentials missing

2. GET /metadata/{identifier}
   -> ItemMetadata

3. Extract source based on target:
   - "metadata" -> item.metadata as serde_json::Value (HashMap)
   - "files/foo.txt" -> find file in item.files by name

4. Apply changes to a copy of source:
   - Set:         dest[field] = new_value
   - Append:      dest[field] = format!("{} {}", src[field], new_value)
   - AppendList:  dest[field].push(new_value) (convert string to list if needed)
   - Insert(i):   remove existing occurrence, insert at index i
   - Remove:      filter out value (delete field if empty, REMOVE_TAG sentinel)
   - REMOVE_TAG:  delete key from dest entirely

5. json_patch::diff(source, destination) -> Vec<PatchOperation>

6. Prepend test ops from expect (if any):
   [{"op":"test","path":"/title","value":"Expected"}] ++ patch

7. POST /metadata/{identifier}
   Content-Type: application/x-www-form-urlencoded
   Body: -target={target}&-patch={json_patch}&priority={priority}&access={key}&secret={secret}
   Optional header: X-Accept-Reduced-Priority: 1

   For multi-target operations, use -changes format instead:
   Body: -changes=[{"target":"metadata","patch":[...]},{"target":"files/f","patch":[...]}]&...

8. Parse response -> ModifyResponse { success, task_id, log, error }
```

## Authentication

### Loading

`IaClient` already loads `s3_access` and `s3_secret` from `IaConfig` (ia.ini `[s3]` section or `IA_ACCESS_KEY_ID`/`IA_SECRET_ACCESS_KEY` env vars).

### Enforcement

```rust
impl IaClient {
    /// Get S3 credentials, or error if not configured.
    fn require_auth(&self) -> Result<(&str, &str)> {
        match (&self.config.s3_access, &self.config.s3_secret) {
            (Some(a), Some(s)) => Ok((a, s)),
            _ => Err(IaError::Auth(
                "S3 credentials required. Run `ia configure` or set \
                 IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY.".into()
            )),
        }
    }
}
```

Auth is checked lazily on first write attempt. Read operations remain unauthenticated.

## Rate Limiting (429 Handling)

When a `429 Too Many Requests` response is received:

1. **All concurrent writes pause** — the catalog task queue on archive.org needs to drain. Hammering other items won't help.
2. **Read `Retry-After` header** — if present, wait that many seconds. If absent, use exponential backoff starting at 30s.
3. **Print status while blocked**:
   ```
   ! Rate limited by archive.org -- catalog queue draining
     Waiting 60s before retrying (Retry-After: 60)
     Items completed: 42/150 | Errors: 2 | Elapsed: 5m 12s
   ```
4. **Resume all writes** after the wait period.
5. `X-Accept-Reduced-Priority: 1` header (via `--reduced-priority` flag) tells IA we're OK being deprioritized, reducing 429 likelihood.

Implementation: shared pause signal via `tokio::sync::Notify` or `AtomicBool` that all concurrent tasks check before proceeding.

## Priority

- Default priority for single items: `0` (let server decide)
- Default priority for batch operations: `-5` (lower priority, matching Python)
- Override with `--priority N` CLI flag
- Passed as `priority` field in POST body

## CLI Interface

### Extended `ia metadata` Command

```
ia metadata [OPTIONS] [IDENTIFIERS]...

Read (existing):
    (no flag)               Print full metadata as JSON
    --exists                Check if item exists (exit code)
    --formats               List available file formats

Write (new, mutually exclusive):
    -m, --modify <K:V>...      Set field to value (repeatable)
    -a, --append <K:V>...      Append to string field (repeatable)
    -A, --append-list <K:V>... Append to list field (repeatable)
    -I, --insert <K[N]:V>...   Insert at index in list (repeatable)
    -r, --remove <K:V>...      Remove value from field (repeatable)

Write options:
    --target <TARGET>       Target: "metadata" (default) or "files/filename"
    --expect <K:V>...       Server-side concurrency check
    --priority <N>          Task priority (default: 0, batch: -5)
    --reduced-priority      Accept reduced priority to avoid rate limiting

Bulk input:
    --spreadsheet <FILE>    Bulk from file (CSV, TSV, XLSX, ODS, JSONL)
    (--itemlist, --search, stdin inherited from existing batch support)

Safety:
    --dry-run               Show changes without writing
```

Short flags `-m`, `-a`, `-A`, `-I`, `-r` don't conflict with reserved globals (`-c`, `-l`, `-d`, `-i`, `-H`, `-j`, `-q`).

### Key:Value Parsing

Same format as Python: `field:value`. Colon separates field name from value. For insert: `field[index]:value`.

```bash
ia metadata nasa --modify="title:NASA Image Archive"
ia metadata nasa --append-list="subject:astronomy"
ia metadata nasa --remove="subject:old_tag"
ia metadata nasa --insert="collection[0]:featured"
ia metadata nasa --modify="bad_field:REMOVE_TAG"       # deletes field
ia metadata nasa --target="files/photo.jpg" --modify="title:Apollo Photo"
ia metadata nasa --expect="title:Old Title" --modify="title:New Title"
```

### Dry-Run Output

```
Dry run -- no changes will be applied

  nasa
    title: "nasa images" -> "NASA Image Archive"        [set]
    subject: ["space"] -> ["space", "astronomy"]         [append-list]
    bad_field: "typo value" -> (removed)                 [set REMOVE_TAG]

  1 item, 3 changes
```

## Batch Operations

### Input Sources

All the same sources supported by download and other commands:

- **Positional args**: `ia metadata item1 item2 --modify="title:New"`
- **`--itemlist FILE`**: Read identifiers from file
- **`--search "QUERY"`**: Use search results
- **stdin**: Pipe identifiers
- **`--spreadsheet FILE`**: CSV/TSV/XLSX/ODS/JSONL with identifier column + field columns

### Spreadsheet Formats

| Format | Extension | Crate | Notes |
|--------|-----------|-------|-------|
| CSV | `.csv` | `csv` | Default, matches Python `ia` |
| TSV | `.tsv` | `csv` (delimiter config) | Tab-separated |
| XLSX | `.xlsx` | `calamine` | Excel read-only |
| ODS | `.ods` | `calamine` | LibreOffice |
| JSONL | `.jsonl` | `serde_json` (existing) | One JSON object per line |

Format auto-detected by file extension.

All spreadsheet formats follow the same schema:
- **Required column**: `identifier`
- **All other columns**: metadata field names -> values
- Empty cells are skipped
- First row is header (CSV/TSV/XLSX/ODS) or each line is a JSON object (JSONL)

### Concurrency

Batch writes respect the global `-j` / `--jobs` semaphore (default 2). When a 429 hits, all jobs pause via shared signal.

### Joblog Integration

Metadata writes log with `op: "modify"`:

```jsonl
{"ts":"2026-02-22T10:00:00Z","op":"modify","item":"nasa","file":"","status":"ok","task_id":12345,"elapsed_ms":800}
{"ts":"2026-02-22T10:00:01Z","op":"modify","item":"bad","file":"","status":"error","error":"403 Forbidden","elapsed_ms":200}
```

The `file` field is empty for item-level metadata, or the filename for `--target files/foo.txt`.

`--retry-failed` reads the joblog, finds `status: "error"` entries with `op: "modify"`, and re-runs them.

## Safety

1. **No credentials -> no writes**: `require_auth()` enforced on every write
2. **`--dry-run`**: Shows computed patch without sending
3. **Immutable field warnings**: Warn if user tries to modify identifier, addeddate, publicdate, uploader, or mediatype (only IA admin can change mediatype)
4. **XML field name validation**: Metadata keys must be valid XML tags
5. **No confirmation prompts**: The CLI executes what's asked (power tool). Confirmation lives in higher-level consumers (ia ai TUI, GUI)
6. **CLAUDE.md update**: When this ships, the "NEVER write" safety rule gets updated to allow writes with explicit user action

## Subject Field: Semicolon Legacy

The `subject` field has legacy behavior where subjects are stored as a semicolon-delimited string (e.g., `"space;nasa;apollo"`). The details page and search engine split these into individual subjects.

New subjects are generally stored as arrays (`["space", "nasa", "apollo"]`). Our `--remove` operation must handle both forms:
- Array form: filter the value out of the array
- Semicolon string: split, filter, rejoin (or convert to array)

## Compatibility with `ia ai`

The `ia ai` command (see `docs/plans/2026-02-21-ai-command-design.md`) consumes `metadata::modify()` for:

- **Applying accepted changes**: `MetadataOp::Set` with field -> new_value
- **Undo**: `MetadataOp::Set` with field -> old_value (swap old/new from joblog)
- **Removing added fields**: `MetadataOp::Set` with field -> `"REMOVE_TAG"`
- **Batch**: Multiple fields on one item in a single call
- **Fire-and-forget**: ia ai sends writes and moves to next item; result logged to joblog
- **Error reporting**: `ModifyResponse.error` captured in joblog for status display

## New Dependencies

| Crate | Purpose | Notes |
|-------|---------|-------|
| `json-patch` | RFC 6902 diff/apply | ~800K downloads, serde_json-based |
| `calamine` | Read XLSX/ODS files | Well-maintained, read-only |
| `csv` | Read/write CSV/TSV | Rust ecosystem standard |

## Testing Strategy

**All tests use wiremock. Zero live requests to archive.org. Ever.**

### Three Test Layers

1. **Unit tests (ia-core)** — Patch computation, metadata merging, REMOVE_TAG handling, key:value parsing. No HTTP.
2. **Integration tests (ia-core)** — Full `modify()` flow with wiremock: mock GET to return fake metadata, mock POST to capture and verify the patch. Assert auth, headers, body format.
3. **CLI integration tests (ia-cli)** — `assert_cmd` + wiremock for end-to-end CLI with all flags.

### Mock Metadata Fixtures

Rich set of **fake** items covering IA's real-world chaos:

| Fixture | Corner cases |
|---------|-------------|
| `clean_item` | Well-formed, all fields populated (baseline) |
| `minimal_item` | Only identifier + mediatype, everything else missing |
| `string_arrays_item` | subject/collection/creator as single strings (should be arrays) |
| `mega_collections_item` | 15+ collections (fav-*, real, parent auto-populated) |
| `unicode_item` | Tibetan/CJK/emoji in title, accented chars in creator |
| `html_description_item` | Raw HTML in description (`<br>`, `<a href>`, `<b>`) |
| `date_chaos_item` | date as "circa 1920", "1943", "20140925130256", "[n.d.]" |
| `duplicate_values_item` | Same subject twice, same collection twice |
| `numeric_strings_item` | Fields that look numeric: ppi, scanfee, imagecount as strings |
| `empty_strings_item` | Fields set to "" (empty string, not null/missing) |
| `null_fields_item` | Fields explicitly set to null |
| `extra_fields_item` | 30+ custom fields (scanningcenter, camera, ppi, barcode, etc.) |
| `file_level_meta_item` | Files with custom metadata (rotation, OCR fields) |
| `dark_item` | is_dark: true, limited metadata |
| `semicolon_subjects_item` | subject as "space;nasa;apollo" (legacy format) |
| `description_array_item` | description as array of strings |
| `scan_metadata_item` | scandate, scanner, ppi, camera, operator, republisher fields |

### Test Cases per Operation

**Set (--modify)**:
- Set existing field to new value
- Set new field that doesn't exist
- Set field to REMOVE_TAG (delete)
- Set field on item with string-typed "should be array" field
- Set field with unicode value
- Set field with empty string
- Set multiple fields at once
- Overwrite list field with scalar
- Overwrite scalar field with list
- Try to set immutable field (identifier, mediatype) -> warning

**Append (--append)**:
- Append to existing string field
- Append to empty/missing field (should set, not error)
- Append to list field (should error)

**AppendList (--append-list)**:
- Append to existing list field
- Append to string field (convert to list)
- Append to missing field (create single-element list)
- Append duplicate value (allow dupes, matching Python)

**Insert (--insert)**:
- Insert at index 0 (prepend)
- Insert at end
- Insert with deduplication (value already exists)
- Insert into string field (convert to list)
- Insert with out-of-bounds index

**Remove (--remove)**:
- Remove value from list (leaves remaining)
- Remove only value from list (field deleted)
- Remove scalar field (value matches -> deleted)
- Remove value that doesn't exist
- Remove from collection (can't remove last)
- Remove from semicolon-delimited subject

**Expect (--expect)**:
- Expect matches -> succeeds
- Expect doesn't match -> server rejects (mock 400)
- Expect on missing field

**Target (--target)**:
- Item-level metadata (default)
- File-level: existing file
- File-level: file not found (error)
- Multi-target: item + file in one call (-changes format)

**Edge cases**:
- Zero-change patch ("no changes to xml" response)
- 429 rate limiting -> pause and retry
- 401/403 -> auth error
- 500 -> retry with backoff
- Network timeout -> error in joblog
- Huge metadata (100+ fields)

**Batch/Spreadsheet**:
- CSV with header + multiple items
- CSV with BOM (Excel export)
- CSV with empty rows / empty identifier
- CSV with missing/extra columns
- XLSX with multiple sheets (use first)
- ODS format
- TSV format
- JSONL format
- Batch with --dry-run
- Batch with --retry-failed from joblog
- Batch with --search as input source
- Batch with --itemlist
- Batch from stdin

## Phasing

This design is Phase 0 from the AI command design doc:

| Phase | Scope |
|-------|-------|
| This design | Core `metadata::modify()` + CLI `--modify/--append/--append-list/--insert/--remove` + batch + spreadsheet + tests |
| AI Phase 1+ | `ia ai` command consumes `metadata::modify()` |
| GUI | Slint metadata editor consumes `metadata::modify()` |
