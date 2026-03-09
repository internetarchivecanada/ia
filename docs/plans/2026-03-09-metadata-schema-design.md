# Design: `ia metadata schema` Command

**Date:** 2026-03-09
**Status:** Approved

## Overview

Add an `ia metadata schema` subcommand that provides a human-friendly interface to the Internet Archive metadata schema stored in the `ia-metadata` item. The schema defines all recognized metadata fields for both items and files, including their types, requirements, editability, and usage guidance.

## Data Source

The schema is a JSON file at `archive.org/download/ia-metadata/ia-metadata_schema.json`. Structure:

```json
{
  "metadata_schema": [ ... 107 item-level field definitions ... ],
  "files_schema": [ ... 107 file-level field definitions ... ]
}
```

Each field entry has these properties:
- `field` — machine name (e.g. `title`, `creator`)
- `label` — human name (e.g. `Title`, `Creator/Author`)
- `required` — `Yes`, `No`, `Recommended`, or `Deprecated`
- `repeatable` — `Yes` or `No`
- `internal use only` — `Yes` or `No`
- `defined by` — `uploader`, `IA admin`, `IA software`, or `user admin`
- `edit access` — `uploader`, `IA admin`, `IA software`, `user admin`, or `not editable`
- `definition` — description of the field
- `accepted values` — what values are valid
- `usage notes` — additional guidance
- `example` — array of example values

The schema is fetched live from archive.org on every invocation. No caching.

## Command Interface

```
ia metadata schema [FIELD] [FLAGS]
```

### Modes

1. **Table listing** (default): compact table of all user-facing fields
2. **Single-field detail**: `ia metadata schema title` — detailed card with all properties
3. **JSON output**: `--json` for machine-readable output

### Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--files` | `-f` | Show files_schema instead of metadata_schema |
| `--internal` | | Include internal-use-only fields (hidden by default) |
| `--required` | | Only show required or recommended fields |
| `--repeatable` | | Only show repeatable fields |
| `--defined-by <WHO>` | | Filter by definer: uploader, ia-admin, ia-software, user-admin |
| `--edit-access <WHO>` | | Filter by edit access: uploader, ia-admin, ia-software, user-admin, not-editable |
| `--json` | | Output as JSON (array for listing, object for single field) |

Filters are combinable (AND logic). The `--internal` flag is additive — it includes internal fields on top of the default user-facing set. When combined with other filters, internal fields matching those filters are included.

### Examples

```bash
# List all user-facing metadata fields
ia metadata schema

# Look up a specific field
ia metadata schema title

# Show file-level schema
ia metadata schema --files

# Find required fields
ia metadata schema --required

# Fields editable by uploaders
ia metadata schema --edit-access uploader

# Machine-readable output
ia metadata schema --json

# Combine filters
ia metadata schema --required --repeatable
```

## Architecture

### ia-core: `metadata/schema.rs`

New module in the existing `metadata/` module directory.

**Types:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaField {
    pub field: String,
    pub label: String,
    pub required: String,        // "Yes", "No", "Recommended", "Deprecated"
    pub repeatable: String,      // "Yes", "No"
    #[serde(rename = "internal use only")]
    pub internal_use_only: String, // "Yes", "No"
    #[serde(rename = "defined by")]
    pub defined_by: String,
    #[serde(rename = "edit access")]
    pub edit_access: String,
    pub definition: String,
    #[serde(rename = "accepted values", default)]
    pub accepted_values: String,
    #[serde(rename = "usage notes", default)]
    pub usage_notes: String,
    #[serde(default)]
    pub example: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SchemaData {
    pub metadata_schema: Vec<SchemaField>,
    pub files_schema: Vec<SchemaField>,
}
```

**Functions:**

- `fetch_schema(client: &IaClient) -> Result<SchemaData>` — GET the schema JSON and deserialize
- Filtering is done by the caller (CLI layer) since it's presentation logic

### ia-cli: additions to `commands/metadata.rs`

**Args:**

```rust
#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// Field name to look up (shows detailed info)
    pub field: Option<String>,

    /// Show file-level schema instead of item-level
    #[arg(short = 'f', long)]
    pub files: bool,

    /// Include internal-use-only fields
    #[arg(long)]
    pub internal: bool,

    /// Only show required or recommended fields
    #[arg(long)]
    pub required: bool,

    /// Only show repeatable fields
    #[arg(long)]
    pub repeatable: bool,

    /// Filter by who defines the field
    #[arg(long, value_enum)]
    pub defined_by: Option<DefinedBy>,

    /// Filter by who can edit the field
    #[arg(long, value_enum)]
    pub edit_access: Option<EditAccess>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

Add `Schema(SchemaArgs)` variant to `MetadataCommand` enum. Add dispatch in the `run()` match.

**Display — table mode:**

Uses `comfy-table` (already a dependency). Columns: Field, Label, Required, Repeatable.

**Display — detail mode:**

Indented key-value card format:

```
title
  Label:           Title
  Required:        Recommended
  Repeatable:      No
  Internal:        No
  Defined by:      uploader
  Edit access:     uploader
  Definition:      Title of media
  Accepted values: String, plain text; no HTML or HTML entities
  Usage notes:     All alphabets are supported
  Example:         San Francisco (1955 Cinemascope film)
```

**Display — JSON mode:**

- Table mode: JSON array of matching `SchemaField` objects
- Detail mode: single `SchemaField` JSON object
- Errors follow standard `{"error": {"code": "...", "message": "..."}}` pattern

### Error Handling

- Network/HTTP errors propagate as `IaError`
- Unknown field name: print available field names, suggest close matches (Levenshtein or simple prefix match — keep it simple, no new deps)
- Invalid enum values for `--defined-by`/`--edit-access`: handled by clap's `ValueEnum` derive

## Testing

- **Unit tests** (ia-core): deserialize sample schema JSON, verify `SchemaField` parsing
- **Integration tests** (ia-core): `fetch_schema` with wiremock serving a fixture
- **CLI tests** (ia-cli): `assert_cmd` tests for table output, detail output, `--json`, filter flags, unknown field error
