# `ia metadata schema` Implementation Plan

**Goal:** Add an `ia metadata schema` subcommand that fetches, filters, and displays the IA metadata schema in table or detail format.

**Architecture:** New `schema.rs` module in `ia-core/src/metadata/` for types and fetching. CLI args and display logic added to `ia-cli/src/commands/metadata.rs`. Live fetch from `archive.org/download/ia-metadata/ia-metadata_schema.json`, no caching.

**Tech Stack:** serde (deserialize), reqwest via IaClient (fetch), comfy-table (table display), clap derive (CLI args), wiremock (tests)

---

### Task 1: Schema Types and Deserialization (ia-core)

**Files:**
- Create: `ia-core/src/metadata/schema.rs`
- Modify: `ia-core/src/metadata/mod.rs`

**Step 1: Write the failing test**

Add to `ia-core/src/metadata/schema.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema_json() -> &'static str {
        r#"{
            "metadata_schema": [
                {
                    "field": "title",
                    "label": "Title",
                    "required": "Recommended",
                    "repeatable": "No",
                    "internal use only": "No",
                    "defined by": "uploader",
                    "edit access": "uploader",
                    "definition": "Title of media",
                    "accepted values": "String, plain text",
                    "usage notes": "All alphabets supported",
                    "example": ["San Francisco (1955)"]
                },
                {
                    "field": "scanner",
                    "label": "Scanner",
                    "required": "No",
                    "repeatable": "No",
                    "internal use only": "Yes",
                    "defined by": "IA software",
                    "edit access": "IA admin",
                    "definition": "Scanner used to digitize",
                    "accepted values": "String"
                }
            ],
            "files_schema": [
                {
                    "field": "name",
                    "label": "File Name",
                    "required": "Yes",
                    "repeatable": "No",
                    "internal use only": "No",
                    "defined by": "uploader",
                    "edit access": "not editable",
                    "definition": "Name of the file"
                }
            ]
        }"#
    }

    #[test]
    fn deserialize_schema_data() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        assert_eq!(data.metadata_schema.len(), 2);
        assert_eq!(data.files_schema.len(), 1);
    }

    #[test]
    fn schema_field_has_all_properties() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let title = &data.metadata_schema[0];
        assert_eq!(title.field, "title");
        assert_eq!(title.label, "Title");
        assert_eq!(title.required, "Recommended");
        assert_eq!(title.repeatable, "No");
        assert_eq!(title.internal_use_only, "No");
        assert_eq!(title.defined_by, "uploader");
        assert_eq!(title.edit_access, "uploader");
        assert_eq!(title.definition, "Title of media");
        assert_eq!(title.accepted_values, "String, plain text");
        assert_eq!(title.usage_notes, "All alphabets supported");
        assert_eq!(title.example, vec!["San Francisco (1955)"]);
    }

    #[test]
    fn missing_optional_fields_default_empty() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let scanner = &data.metadata_schema[1];
        assert_eq!(scanner.usage_notes, "");
        assert!(scanner.example.is_empty());
    }

    #[test]
    fn schema_field_serializes_to_json() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let json = serde_json::to_value(&data.metadata_schema[0]).unwrap();
        assert_eq!(json["field"], "title");
        assert_eq!(json["label"], "Title");
        // Verify serde rename works for output
        assert_eq!(json["internal_use_only"], "No");
        assert_eq!(json["defined_by"], "uploader");
        assert_eq!(json["edit_access"], "uploader");
        assert_eq!(json["accepted_values"], "String, plain text");
        assert_eq!(json["usage_notes"], "All alphabets supported");
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core schema -- --nocapture 2>&1 | head -20`
Expected: compilation errors — `SchemaData` and `SchemaField` don't exist yet

**Step 3: Write the types**

At the top of `ia-core/src/metadata/schema.rs`:

```rust
use serde::{Deserialize, Serialize};

/// A single field definition from the IA metadata schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaField {
    /// Machine name (e.g. "title", "creator")
    pub field: String,
    /// Human-readable label (e.g. "Title", "Creator/Author")
    pub label: String,
    /// Whether required: "Yes", "No", "Recommended", or "Deprecated"
    pub required: String,
    /// Whether the field accepts multiple values: "Yes" or "No"
    pub repeatable: String,
    /// Whether this is an internal-only field: "Yes" or "No"
    #[serde(
        rename(deserialize = "internal use only"),
        alias = "internal_use_only"
    )]
    pub internal_use_only: String,
    /// Who defines this field: "uploader", "IA admin", "IA software", "user admin"
    #[serde(rename(deserialize = "defined by"), alias = "defined_by")]
    pub defined_by: String,
    /// Who can edit: "uploader", "IA admin", "IA software", "user admin", "not editable"
    #[serde(rename(deserialize = "edit access"), alias = "edit_access")]
    pub edit_access: String,
    /// Description of the field
    #[serde(default)]
    pub definition: String,
    /// What values are accepted
    #[serde(
        rename(deserialize = "accepted values"),
        alias = "accepted_values",
        default
    )]
    pub accepted_values: String,
    /// Additional usage guidance
    #[serde(
        rename(deserialize = "usage notes"),
        alias = "usage_notes",
        default
    )]
    pub usage_notes: String,
    /// Example values
    #[serde(default)]
    pub example: Vec<String>,
}

/// The complete schema data from the ia-metadata item.
#[derive(Debug, Deserialize)]
pub struct SchemaData {
    /// Item-level metadata field definitions
    pub metadata_schema: Vec<SchemaField>,
    /// File-level metadata field definitions
    pub files_schema: Vec<SchemaField>,
}
```

**Step 4: Wire up the module**

In `ia-core/src/metadata/mod.rs`, add:

```rust
pub mod schema;
```

And add to the `pub use` block:

```rust
pub use schema::{SchemaData, SchemaField};
```

**Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-core schema`
Expected: all 4 tests pass

**Step 6: Commit**

```
git add ia-core/src/metadata/schema.rs ia-core/src/metadata/mod.rs
git commit -m "feat(schema): add SchemaField and SchemaData types with deserialization

Add serde types for the IA metadata schema stored in the ia-metadata
item. SchemaField captures all 11 properties (field, label, required,
repeatable, internal_use_only, defined_by, edit_access, definition,
accepted_values, usage_notes, example) with serde rename for the
space-separated JSON keys. SchemaData wraps both metadata_schema and
files_schema arrays."
```

---

### Task 2: fetch_schema Function (ia-core)

**Files:**
- Modify: `ia-core/src/metadata/schema.rs`

**Step 1: Write the failing integration test**

Add to the `tests` module in `schema.rs`:

```rust
    #[tokio::test]
    async fn fetch_schema_returns_both_schemas() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/ia-metadata/ia-metadata_schema.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sample_schema_json()),
            )
            .mount(&mock_server)
            .await;

        let client = crate::IaClient::new_for_test(&mock_server.uri());
        let data = fetch_schema(&client).await.unwrap();
        assert_eq!(data.metadata_schema.len(), 2);
        assert_eq!(data.files_schema.len(), 1);
    }

    #[tokio::test]
    async fn fetch_schema_returns_error_on_404() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/ia-metadata/ia-metadata_schema.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = crate::IaClient::new_for_test(&mock_server.uri());
        let result = fetch_schema(&client).await;
        assert!(result.is_err());
    }
```

Note: Check if `IaClient::new_for_test` exists. If it's called something else (e.g. `IaClient::new_with_base_url`), use that. Look at other integration tests in `ia-core` (e.g. `download.rs` or `metadata/read.rs`) to find the test client constructor pattern.

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core fetch_schema`
Expected: compilation error — `fetch_schema` doesn't exist

**Step 3: Implement fetch_schema**

Add to `ia-core/src/metadata/schema.rs` (above the tests module):

```rust
use crate::IaClient;

/// Fetch the metadata schema from the ia-metadata item on archive.org.
///
/// Downloads and parses `ia-metadata_schema.json` which contains both
/// item-level (`metadata_schema`) and file-level (`files_schema`) field
/// definitions.
pub async fn fetch_schema(client: &IaClient) -> crate::Result<SchemaData> {
    let url = client.url("/download/ia-metadata/ia-metadata_schema.json");
    let resp = client.http().get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(crate::IaError::Http {
            status: status.as_u16(),
            message: format!("failed to fetch metadata schema: {status}"),
        });
    }
    let body = resp.text().await.map_err(|e| crate::IaError::Http {
        status: status.as_u16(),
        message: format!("failed to read schema response body: {e}"),
    })?;
    let data: SchemaData = serde_json::from_str(&body)?;
    Ok(data)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core fetch_schema`
Expected: both tests pass

**Step 5: Export the function**

Add `fetch_schema` to the re-exports in `ia-core/src/metadata/mod.rs`:

```rust
pub use schema::{fetch_schema, SchemaData, SchemaField};
```

**Step 6: Commit**

```
git add ia-core/src/metadata/schema.rs ia-core/src/metadata/mod.rs
git commit -m "feat(schema): add fetch_schema to download schema from archive.org

Fetches ia-metadata_schema.json via IaClient, handles HTTP errors,
and deserializes into SchemaData. Tested with wiremock mocks."
```

---

### Task 3: CLI Args and Subcommand Wiring (ia-cli)

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Add SchemaArgs struct and enum variants**

Add the `SchemaArgs` struct near the other args structs (after `ImportArgs`). Use clap derive patterns matching the existing code:

```rust
/// Filter values for --defined-by flag
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DefinedByFilter {
    Uploader,
    #[value(name = "ia-admin")]
    IaAdmin,
    #[value(name = "ia-software")]
    IaSoftware,
    #[value(name = "user-admin")]
    UserAdmin,
}

/// Filter values for --edit-access flag
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum EditAccessFilter {
    Uploader,
    #[value(name = "ia-admin")]
    IaAdmin,
    #[value(name = "ia-software")]
    IaSoftware,
    #[value(name = "user-admin")]
    UserAdmin,
    #[value(name = "not-editable")]
    NotEditable,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// Field name to look up (shows detailed view)
    pub field: Option<String>,

    /// Show file-level schema instead of item-level
    #[arg(short = 'f', long)]
    pub files: bool,

    /// Include internal-use-only fields (hidden by default)
    #[arg(long)]
    pub internal: bool,

    /// Only show required or recommended fields
    #[arg(long)]
    pub required: bool,

    /// Only show repeatable fields
    #[arg(long)]
    pub repeatable: bool,

    /// Filter by who defines the field
    #[arg(long)]
    pub defined_by: Option<DefinedByFilter>,

    /// Filter by who can edit the field
    #[arg(long)]
    pub edit_access: Option<EditAccessFilter>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

**Step 2: Add Schema variant to MetadataCommand enum**

Add to the `MetadataCommand` enum (after the `Import` variant):

```rust
    /// Look up Internet Archive metadata field definitions
    #[command(
        long_about = "Look up Internet Archive metadata field definitions. Shows a table of \
            all user-facing fields by default, or detailed info for a specific field.\n\n\
            The schema is fetched live from the ia-metadata item on archive.org.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># List all user-facing metadata fields</dim>\n  <bold>$ ia metadata schema</bold>\
             \n\n  <dim># Look up a specific field</dim>\n  <bold>$ ia metadata schema title</bold>\
             \n\n  <dim># Show file-level schema</dim>\n  <bold>$ ia metadata schema --files</bold>\
             \n\n  <dim># Show required fields only</dim>\n  <bold>$ ia metadata schema --required</bold>\
             \n\n  <dim># Include internal fields</dim>\n  <bold>$ ia metadata schema --internal</bold>\
             \n\n  <dim># Machine-readable output</dim>\n  <bold>$ ia metadata schema --json</bold>\n"
        ),
    )]
    Schema(SchemaArgs),
```

**Step 3: Add dispatch in run() match**

Add a new arm in the `match args.command` block (before the `None` arm):

```rust
        Some(MetadataCommand::Schema(sub)) => {
            if continuations.is_some() {
                bail!("compound operations (+) cannot be used with schema");
            }
            run_schema(client, sub).await
        }
```

**Step 4: Add stub run_schema function**

```rust
async fn run_schema(client: &IaClient, args: SchemaArgs) -> Result<()> {
    let _ = (client, args);
    bail!("schema command not yet implemented")
}
```

**Step 5: Verify it compiles and the subcommand is recognized**

Run: `cargo check -p ia-cli`
Expected: compiles successfully

Run: `cargo run -- metadata schema --help 2>&1 | tail -20`
Expected: help text showing the schema subcommand flags

**Step 6: Commit**

```
git add ia-cli/src/commands/metadata.rs
git commit -m "feat(schema): wire up 'ia metadata schema' subcommand with args

Add SchemaArgs, DefinedByFilter, EditAccessFilter structs and Schema
variant to MetadataCommand. Subcommand is recognized and shows help
text but is not yet implemented (stub returns error)."
```

---

### Task 4: Table Display (listing mode)

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Write CLI integration test for table output**

Add to `ia-cli/tests/` (find the existing metadata CLI test file or create `ia-cli/tests/cli_schema.rs`). Check how other CLI tests are structured — look at `ia-cli/tests/cli_metadata.rs` or similar for the pattern with `assert_cmd` and wiremock.

```rust
use assert_cmd::Command;
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path};

fn sample_schema_json() -> &'static str {
    // Same fixture as Task 1 — use the two-field metadata_schema + one-field files_schema
    r#"{
        "metadata_schema": [
            {
                "field": "title",
                "label": "Title",
                "required": "Recommended",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "uploader",
                "definition": "Title of media",
                "accepted values": "String, plain text",
                "usage notes": "All alphabets supported",
                "example": ["San Francisco (1955)"]
            },
            {
                "field": "scanner",
                "label": "Scanner",
                "required": "No",
                "repeatable": "No",
                "internal use only": "Yes",
                "defined by": "IA software",
                "edit access": "IA admin",
                "definition": "Scanner used to digitize",
                "accepted values": "String"
            }
        ],
        "files_schema": [
            {
                "field": "name",
                "label": "File Name",
                "required": "Yes",
                "repeatable": "No",
                "internal use only": "No",
                "defined by": "uploader",
                "edit access": "not editable",
                "definition": "Name of the file"
            }
        ]
    }"#
}

#[tokio::test]
async fn schema_table_hides_internal_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema"])
        .assert()
        .success()
        .stdout(predicates::str::contains("title"))
        .stdout(predicates::str::contains("Title"))
        .stdout(predicates::str::contains("scanner").not());
}

#[tokio::test]
async fn schema_table_shows_internal_with_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "--internal"])
        .assert()
        .success()
        .stdout(predicates::str::contains("title"))
        .stdout(predicates::str::contains("scanner"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli schema_table`
Expected: fails because `run_schema` returns an error

**Step 3: Implement table display in run_schema**

Replace the `run_schema` stub:

```rust
use ia_core::metadata::{fetch_schema, SchemaField};
use comfy_table::{Cell, Color, Table};

impl DefinedByFilter {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Uploader => value == "uploader",
            Self::IaAdmin => value == "IA admin",
            Self::IaSoftware => value == "IA software",
            Self::UserAdmin => value == "user admin",
        }
    }
}

impl EditAccessFilter {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Uploader => value == "uploader",
            Self::IaAdmin => value == "IA admin",
            Self::IaSoftware => value == "IA software",
            Self::UserAdmin => value == "user admin",
            Self::NotEditable => value == "not editable",
        }
    }
}

fn filter_schema_fields(fields: &[SchemaField], args: &SchemaArgs) -> Vec<&SchemaField> {
    fields
        .iter()
        .filter(|f| args.internal || f.internal_use_only != "Yes")
        .filter(|f| !args.required || f.required == "Yes" || f.required == "Recommended")
        .filter(|f| !args.repeatable || f.repeatable == "Yes")
        .filter(|f| {
            args.defined_by
                .as_ref()
                .map_or(true, |db| db.matches(&f.defined_by))
        })
        .filter(|f| {
            args.edit_access
                .as_ref()
                .map_or(true, |ea| ea.matches(&f.edit_access))
        })
        .collect()
}

fn print_schema_table(fields: &[&SchemaField]) {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(vec![
        Cell::new("FIELD").fg(Color::Cyan),
        Cell::new("LABEL").fg(Color::Cyan),
        Cell::new("REQUIRED").fg(Color::Cyan),
        Cell::new("REPEATABLE").fg(Color::Cyan),
    ]);
    for f in fields {
        table.add_row(vec![
            &f.field,
            &f.label,
            &f.required,
            &f.repeatable,
        ]);
    }
    println!("{table}");
}

async fn run_schema(client: &IaClient, args: SchemaArgs) -> Result<()> {
    let data = fetch_schema(client).await?;
    let source = if args.files {
        &data.files_schema
    } else {
        &data.metadata_schema
    };

    // Single-field detail mode handled in Task 5
    if let Some(ref field_name) = args.field {
        let _ = field_name;
        bail!("single-field lookup not yet implemented");
    }

    let filtered = filter_schema_fields(source, &args);

    if args.json {
        let json = serde_json::to_string_pretty(&filtered)?;
        println!("{json}");
    } else {
        print_schema_table(&filtered);
    }

    Ok(())
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli schema_table`
Expected: both tests pass

**Step 5: Commit**

```
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli_schema.rs
git commit -m "feat(schema): implement table display with filtering

Table output uses comfy-table with FIELD, LABEL, REQUIRED, REPEATABLE
columns. Internal fields hidden by default. Supports --internal,
--required, --repeatable, --defined-by, --edit-access filters and
--json output."
```

---

### Task 5: Detail Display (single-field lookup)

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/tests/cli_schema.rs`

**Step 1: Write CLI integration test for detail output**

```rust
#[tokio::test]
async fn schema_detail_shows_all_properties() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "title"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("title"));
    assert!(stdout.contains("Label:"));
    assert!(stdout.contains("Title"));
    assert!(stdout.contains("Required:"));
    assert!(stdout.contains("Recommended"));
    assert!(stdout.contains("Definition:"));
    assert!(stdout.contains("Example:"));
}

#[tokio::test]
async fn schema_detail_unknown_field_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not found"));
}

#[tokio::test]
async fn schema_detail_json_outputs_single_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "title", "--json"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["field"], "title");
    assert!(v.is_object(), "single field should be an object, not array");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli schema_detail`
Expected: fails — detail mode returns "not yet implemented" error

**Step 3: Implement detail display**

Replace the single-field block in `run_schema`:

```rust
    if let Some(ref field_name) = args.field {
        let found = source.iter().find(|f| f.field == *field_name);
        match found {
            Some(field) => {
                if args.json {
                    let json = serde_json::to_string_pretty(field)?;
                    println!("{json}");
                } else {
                    print_schema_detail(field);
                }
            }
            None => {
                // Find close matches for suggestion
                let available: Vec<&str> = source.iter().map(|f| f.field.as_str()).collect();
                let suggestions: Vec<&str> = available
                    .iter()
                    .filter(|f| f.contains(field_name.as_str()) || field_name.contains(**f))
                    .copied()
                    .take(5)
                    .collect();
                let mut msg = format!("field '{}' not found in schema", field_name);
                if !suggestions.is_empty() {
                    msg.push_str(&format!(". Did you mean: {}?", suggestions.join(", ")));
                }
                bail!("{msg}");
            }
        }
        return Ok(());
    }
```

Add the `print_schema_detail` function:

```rust
fn print_schema_detail(field: &SchemaField) {
    println!("{}", field.field);
    println!("  Label:           {}", field.label);
    println!("  Required:        {}", field.required);
    println!("  Repeatable:      {}", field.repeatable);
    println!("  Internal:        {}", field.internal_use_only);
    println!("  Defined by:      {}", field.defined_by);
    println!("  Edit access:     {}", field.edit_access);
    if !field.definition.is_empty() {
        println!("  Definition:      {}", field.definition);
    }
    if !field.accepted_values.is_empty() {
        println!("  Accepted values: {}", field.accepted_values);
    }
    if !field.usage_notes.is_empty() {
        println!("  Usage notes:     {}", field.usage_notes);
    }
    if !field.example.is_empty() {
        println!("  Example:         {}", field.example.join(", "));
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli schema_detail`
Expected: all 3 tests pass

**Step 5: Commit**

```
git add ia-cli/src/commands/metadata.rs ia-cli/tests/cli_schema.rs
git commit -m "feat(schema): implement single-field detail display

Shows all properties for a specific field in an indented card format.
Unknown field names produce an error with substring-match suggestions.
JSON mode outputs a single object instead of an array."
```

---

### Task 6: Filter Flag Tests

**Files:**
- Modify: `ia-cli/tests/cli_schema.rs`

**Step 1: Write tests for remaining filter flags**

```rust
#[tokio::test]
async fn schema_required_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "--required"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("title")); // Recommended
    // scanner is "No" required AND internal — should not appear
}

#[tokio::test]
async fn schema_files_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "--files"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("name"));
    assert!(stdout.contains("File Name"));
    // Should not contain metadata-only fields
    assert!(!stdout.contains("title"));
}

#[tokio::test]
async fn schema_json_outputs_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args(["--host", &host, "--insecure", "metadata", "schema", "--json"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(v.is_array(), "listing mode should output JSON array");
}

#[tokio::test]
async fn schema_defined_by_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/ia-metadata/ia-metadata_schema.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sample_schema_json()))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = Command::cargo_bin("ia")
        .unwrap()
        .args([
            "--host", &host, "--insecure",
            "metadata", "schema", "--internal", "--defined-by", "ia-software",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("scanner")); // defined by IA software
    assert!(!stdout.contains("title")); // defined by uploader
}
```

**Step 2: Run tests to verify they pass**

Run: `cargo test -p ia-cli schema_`
Expected: all tests pass (filters already implemented in Task 4)

**Step 3: Commit**

```
git add ia-cli/tests/cli_schema.rs
git commit -m "test(schema): add CLI integration tests for filter flags

Tests --required, --files, --json array output, and --defined-by
filter. Verifies filters correctly narrow results."
```

---

### Task 7: Update MetadataArgs Help Text and Final Polish

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`

**Step 1: Update MetadataArgs after_long_help**

Add a schema example to the `after_long_help` string in `MetadataArgs`:

```
\n\n  <dim># Browse metadata field definitions</dim>\n  <bold>$ ia metadata schema</bold>\
\n  <bold>$ ia metadata schema title</bold>\n
```

**Step 2: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: all tests pass

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: zero warnings

**Step 3: Commit**

```
git add ia-cli/src/commands/metadata.rs
git commit -m "docs(schema): add schema examples to metadata help text"
```

---

### Task 8: Update MEMORY.md and Design Doc Status

**Files:**
- Modify: `MEMORY.md`

**Step 1: Update MEMORY.md**

Add to the Key Modules section:
- `metadata/schema.rs` — Schema lookup: fetch, types, SchemaField/SchemaData

Add to Implementation Status:
- **Metadata Schema: COMPLETE** — `ia metadata schema` command with table/detail display, filtering

Add `schema` to the `commands/metadata.rs` description in Key CLI Files.

**Step 2: Commit**

```
git add MEMORY.md
git commit -m "docs: update MEMORY.md with metadata schema command"
```

---

## Summary

| Task | Description | Tests |
|------|-------------|-------|
| 1 | Schema types + deserialization | 4 unit tests |
| 2 | fetch_schema function | 2 integration tests (wiremock) |
| 3 | CLI args + subcommand wiring | compile check + help output |
| 4 | Table display + filtering | 2 CLI integration tests |
| 5 | Detail display (single field) | 3 CLI integration tests |
| 6 | Filter flag tests | 4 CLI integration tests |
| 7 | Help text polish | full test suite + clippy |
| 8 | MEMORY.md update | — |

**Total: 8 tasks, ~15 tests, 2 files created, 3 files modified**
