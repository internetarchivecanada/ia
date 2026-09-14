# AI QA Pipeline Implementation Plan

Design doc: `docs/plans/2026-03-24-ai-qa-pipeline-design.md`

## Phase 1: Foundation (ia-core) — COMPLETE

### 1. AI Config JSON (`ia-core/src/ai/ia_config.rs`)

Types matching the extraction system's format:

```rust
pub struct IaAiConfig { pub result: IaAiConfigResult }
pub struct IaAiConfigResult { model_name, prompt, page_info: Vec<PageInfo>, schema: SchemaWrapper }
pub struct PageInfo { page_type: PageType, count: Option<usize> }
pub enum PageType { Cover, Normal }
pub struct SchemaWrapper { format: SchemaFormat }
pub struct SchemaFormat { format_type, name, schema: serde_json::Value }
```

Operations:
- `fetch_ai_config(client, collection_id)` — GET file content from collection item
- `resolve_ai_config(client, item)` — walk item's collection chain, return most specific config + which collection
- `create_ai_config(client, collection_id, config)` — S3 PUT with format "AI Config JSON"
- `update_ai_config(client, collection_id, config)` — overwrite existing
- `delete_ai_config(client, collection_id)` — S3 DELETE
- `default_ai_config()` — gpt-5-nano, cover+5 pages, common book metadata schema
- `compute_page_count(page_info)` — total pages needed

Config file: `{collection_id}_ai_config.json` with header `x-archive-meta-format: AI Config JSON`.

### 2. Extracted Metadata JSON (`ia-core/src/ai/extracted_metadata.rs`)

```rust
pub struct ExtractedMetadata { pub result: ExtractedMetadataResult }
pub struct ExtractedMetadataResult { ai_request_info: AiRequestInfo, metadata: Map<String, Value> }
pub struct AiRequestInfo { model, input_tokens, output_tokens, total_tokens, input_cost, output_cost, total_cost }
```

Operations:
- `fetch_extracted_metadata(client, identifier)` — GET `{id}_extracted_metadata.json`
- `has_extracted_metadata(item)` — check file list for format "Extracted Metadata JSON"

### 3. Zip Member Downloads (`ia-core/src/ai/zip.rs`)

```rust
pub struct ZipEntry { path: String, size: Option<u64>, modified: Option<String> }
```

Operations:
- `list_zip_contents(client, identifier, zip_filename)` — parse HTML from `/download/{id}/{zip}/`
- `download_zip_member(client, identifier, zip_filename, member_path)` — raw download
- `download_zip_member_converted(client, identifier, zip_filename, member_path, ext)` — append `&ext=jpg`
- `select_pages(entries, page_info)` — cover=first image, normal=next N sequential
- `find_jp2_zip(identifier, files)` — prefer `_jp2.zip` over `_raw_jp2.zip`

### 4. LLM Client Vision Support (`ia-core/src/ai/client.rs`)

```rust
pub enum MessageContent { Text { text }, ImageBase64 { image_url: ImageUrlContent } }
pub struct ImageUrlContent { url: String }  // data:image/jpeg;base64,...
```

New: `chat_vision(system_prompt, user_content: Vec<MessageContent>)` using OpenAI vision format.
Refactored: `chat()` now delegates to shared `send_request()` retry loop.

## Phase 2: QA Pipeline (ia-core) — COMPLETE

### 5. QA Types + Pipeline (`ia-core/src/ai/qa.rs`)

```rust
pub struct QaResult { identifier, overall_confidence, verdict, extraction_model, qa_model, fields: IndexMap, token_usage, elapsed_ms }
pub enum QaVerdict { Pass, Fail, NeedsReview }
pub struct FieldQaResult { extracted_value, verdict, confidence, suggested_correction, note }
pub enum FieldVerdict { Correct, Incorrect, Uncertain }
pub struct QaOpts { model, temperature, max_tokens, confidence_threshold, min_field_confidence, dry_run }
```

QA prompt: system prompt instructs model to verify each field, respond with JSON verdicts.
Pipeline: `qa_item()` sends page images + extracted metadata to QA LLM, parses response.
Verdict logic: any Incorrect -> Fail, any Uncertain or low confidence -> NeedsReview, else Pass.

### 6. Metadata Promotion (`ia-core/src/ai/promote.rs`)

```rust
pub struct PromoteOpts { confidence_threshold, min_field_confidence, fields: Option<Vec<String>>, dry_run }
pub struct PromoteResult { identifier, fields_promoted, fields_skipped, task_id, dry_run }
```

Logic:
1. Filter fields by allowed list and confidence thresholds
2. Correct fields -> use extracted value
3. Incorrect fields with correction -> use correction (if confidence meets threshold)
4. Uncertain fields -> skip
5. Build ModifyRequest with MetadataOp::Set, call metadata::modify()

### `ia ai qa --json` output format (JSONL, one object per item per line)

```json
{
  "identifier": "my-item",
  "overall_confidence": 0.92,
  "verdict": "pass",
  "extraction_model": "gpt-5-nano",
  "qa_model": "claude-sonnet-4-6",
  "fields": {
    "title": {
      "extracted_value": "Studies on an Example Topic",
      "verdict": "correct",
      "confidence": 0.95
    },
    "creator": {
      "extracted_value": ["Example, A."],
      "verdict": "correct",
      "confidence": 0.90
    },
    "date": {
      "extracted_value": "1987",
      "verdict": "incorrect",
      "confidence": 0.90,
      "suggested_correction": "1988",
      "note": "Year on cover page reads 1988, not 1987"
    },
    "department": {
      "extracted_value": "Example Faculty",
      "verdict": "uncertain",
      "confidence": 0.60,
      "note": "Department name not clearly visible in provided images"
    }
  },
  "token_usage": {
    "prompt_tokens": 15000,
    "completion_tokens": 500
  },
  "elapsed_ms": 3200
}
```

### Zip member download in `ia download`

General-purpose zip member access lives in `download/zip.rs` (not in the AI module).
The AI module's `ai/zip.rs` re-exports from `download/zip` and adds only
AI-specific `select_pages()`.

CLI flags:
```
ia download <IDENTIFIER> --zip-list <ZIPFILE>
ia download <IDENTIFIER> --zip-member <ZIPFILE/MEMBER>
ia download <IDENTIFIER> --zip-member <ZIPFILE/MEMBER> --zip-convert jpg
```

URL construction:
- List: `GET /download/{identifier}/{zip_filename}/` → HTML directory listing
- Download: `GET /download/{identifier}/{zip_filename}/{member_path}`
- Convert: `GET /download/{identifier}/{zip_filename}/{member_path}&ext=jpg`

## Phase 3: Feature Flags + CLI (ia-cli) — COMPLETE

### 7. Feature Flag Restructuring

```toml
[features]
default = ["tui"]
tui = ["dep:ratatui", "dep:crossterm", "dep:open"]
self-update = []
alpha = ["ai-qa"]          # alpha now means QA + config
ai-qa = []                 # QA pipeline + config CRUD
ai-analyze = []            # shelved: original analyze mode
```

Gating:
- `ai/ia_config.rs`, `ai/extracted_metadata.rs`, `ai/qa.rs`, `ai/promote.rs`, `ai/zip.rs` → `#[cfg(feature = "ai-qa")]`
- `ai/pipeline.rs`, `ai/prompt.rs`, `ai/undo.rs` → `#[cfg(feature = "ai-analyze")]`
- `ai/client.rs`, `ai/types.rs` → shared (both features)

### 8. CLI: `ia ai config`

```
ia ai config <COMMAND>

Commands:
  show <COLLECTION>         Display AI config for a collection [--json]
  list                      List collections with AI configs [--json]
  create <COLLECTION>       Create a new AI config
  edit <COLLECTION>         Edit an existing AI config

create options:
  --from-file <FILE>        Create from existing JSON file
  --model <MODEL>           LLM model name [default: gpt-5-nano]
  --prompt <TEXT>            Extraction prompt
  --prompt-file <FILE>      Read prompt from file
  --pages <SPEC>            Page specification [default: "cover,normal:5"]
  --schema-file <FILE>      JSON Schema file for response format
  --dry-run                 Show the config JSON without uploading

edit options:
  --editor                  Open in $EDITOR
  --set-model <MODEL>       Update model name
  --set-prompt <TEXT>       Update prompt
  --set-prompt-file <FILE>  Update prompt from file
  --set-pages <SPEC>        Update page specification
```

### 9. CLI: `ia ai qa`

```
ia ai qa [OPTIONS] [IDENTIFIERS]...

Arguments:
  [IDENTIFIERS]...          Items to QA (also accepts stdin)

Input options:
  --search <QUERY>          QA items matching search query
  --itemlist <FILE>         Read identifiers from file

QA options:
  --model <MODEL>           QA LLM model [default: from [ai] config]
  --base-url <URL>          LLM API base URL
  --api-key <KEY>           LLM API key
  --temperature <FLOAT>     LLM temperature [default: 0.2]
  --confidence <FLOAT>      Min overall confidence for promotion [default: 0.8]
  --min-field-confidence <FLOAT>  Min per-field confidence [default: 0.6]

Output options:
  --json                    Output results as JSONL
  --promote                 Write confirmed metadata to items
  --dry-run                 Show what would be done without making changes
  --dashboard               Interactive TUI for review

General:
  -j, --jobs <N>            Concurrent items [default: 2]
  --joblog <FILE>           Append results to joblog
```

Default mode: concise summary per item to stdout.
`--json`: full QA results as JSONL.
`--dashboard`: interactive TUI.
`--promote`: write confirmed fields to items.

## Phase 4: TUI + Polish (ia-cli)

### 10. QA TUI Dashboard (`ia-cli/src/tui/ai_qa.rs`)

Three-panel layout using shared TUI framework (Dashboard trait):

```
+- QA Dashboard -----------------------------------------------------------+
| Item 3/47 | ########.... 6% | Pass: 2  Needs Review: 1  Fail: 0         |
| Model: gpt-5-nano -> claude-sonnet-4-6 (QA) | Cost: $0.12               |
+- Extracted Metadata ---------------+- QA Results ------------------------+
|                                    |                                     |
| title: "Studies on an Exam..."     | V title         0.95  correct      |
| creator: ["Example, A."]           | V creator       0.90  correct      |
| date: "1987"                       | V date          0.98  correct      |
| institution: "Example Univers..."  | V institution   0.92  correct      |
| department: "Example Faculty"      | ? department    0.60  uncertain    |
| language: "German"                 | V language       0.99  correct      |
+------------------------------------+-------------------------------------+
| [a]ccept all [p]romote [e]dit [s]kip [j/k] navigate [q]uit              |
+--------------------------------------------------------------------------+
```

Keyboard: a=accept all, p=promote, e=edit, s=skip, r=reject, j/k=navigate, Tab=switch panel, q=quit.

### 11. Integration Tests

Wiremock-based tests for:
- `ia ai config show` — mock metadata + download APIs
- `ia ai config list` — mock search API
- `ia ai qa --json` — mock all endpoints (metadata, extracted, zip, LLM), verify JSONL
- `ia ai qa --promote --dry-run` — verify no write calls
- `ia ai qa --promote` — verify metadata modify calls

### 12. Error Handling

New IaError variants:
- `AiConfigNotFound { collection }` — no AI config in collection chain
- `ExtractedMetadataNotFound { identifier }` — item has no extraction results
- `NoPageImages { identifier }` — no JP2 zip or empty zip

Update `error.rs`: add variants, `is_retryable()`, `to_json_error()`.

### 13. Documentation

- Update `docs/usage.md` with `ia ai qa` and `ia ai config` sections
- Update CLI help text (`about`, `long_about`, `after_long_help`)
- Update README.md command list

## Verification

### Unit tests (ia-core)
- AI Config JSON serde roundtrip (9 tests)
- Extracted Metadata JSON serde roundtrip (5 tests)
- Zip HTML parsing + page selection (10 tests)
- QA result confidence calculation (10 tests)
- Promotion field filtering (8 tests)
- Vision client message format (existing tests + new)

### Integration tests (ia-cli, wiremock)
- Config show/list mock tests
- QA --json pipeline mock test
- QA --promote dry-run test
- QA --promote write verification

### Manual testing
- Run against real items with extracted metadata
- Verify config resolution walks collection chain correctly
- Test with different LLM providers (OpenAI, Anthropic)
- Test zip member download with various JP2 archives
