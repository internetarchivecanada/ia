# ia ai Redesign: QA Pipeline + Config Compatibility

## Context

A separate team at archive.org has built an AI metadata extraction system:
- **AI Config JSON** — stored in collection items, defines LLM model, prompt, page selection, and response schema
- **Extracted Metadata JSON** — stored per-item, contains extracted metadata + provenance (model, tokens, cost)
- **AI Metadata Extractor derive module** — runs as IA task during republishing, sends page images to LLM
- Internal web UIs exist for editing configs and for reviewing/promoting extracted metadata

The `ia ai` command needs to be **recentered around that system** — using the same config format, reading extraction results, and providing CLI-native QA + review capabilities. The existing standalone analyze mode gets shelved behind a feature flag for future "live extraction" work.

**Top priorities:**
1. AI agent QA of extraction results (two-model consensus: extraction LLM + QA LLM)
2. Config compatibility — read, use, create, edit AI Config JSON via CLI

## Architecture

### Command Structure

```
ia ai <COMMAND>

Commands:
  qa      Verify AI-extracted metadata using vision-based LLM QA
  config  Manage AI extraction configurations for collections
```

Existing analyze mode + undo shelved behind `ai-analyze` feature flag (preserved for future live extraction).

### Data Flow: `ia ai qa`

```
Input (identifiers / --search query / --itemlist / stdin)
  |
  +- Fetch item metadata (GET /metadata/{id})
  +- Fetch Extracted Metadata JSON (GET /download/{id}/{id}_extracted_metadata.json)
  +- Resolve AI Config JSON (walk collection chain, fetch from most specific collection)
  +- Fetch page images from {id}_jp2.zip (per config pageInfo)
      |
      v
  Build QA prompt: system prompt + page images + extracted metadata + schema
      |
      v
  Send to QA LLM (different model than extraction model)
      |
      v
  Parse structured response: per-field verdicts + confidence scores
      |
      v
  Output: --json (JSONL) or interactive TUI dashboard
      |
      v
  Optional: --promote writes confirmed metadata to item (via metadata modify API)
```

All network fetches (metadata, extracted metadata, config, images) are concurrent.

## New Modules (ia-core)

### `ai/ia_config.rs` — AI Config JSON types + CRUD

Types matching the extraction system's format: `IaAiConfig`, `IaAiConfigResult`, `PageInfo`, `PageType`, `SchemaWrapper`, `SchemaFormat`.

Operations: `fetch_ai_config`, `resolve_ai_config` (collection chain walk), `create_ai_config`, `update_ai_config`, `delete_ai_config`, `default_ai_config`.

### `ai/extracted_metadata.rs` — Extracted Metadata JSON types + fetch

Types: `ExtractedMetadata`, `ExtractedMetadataResult`, `AiRequestInfo`.

Operations: `fetch_extracted_metadata`, `has_extracted_metadata`.

### `ai/zip.rs` — Zip member listing + download

Types: `ZipEntry`. Functions: `list_zip_contents` (HTML parsing), `download_zip_member`, `download_zip_member_converted`, `select_pages` (pageInfo -> file list), `find_jp2_zip`.

### `ai/qa.rs` — QA pipeline

Types: `QaResult`, `QaVerdict`, `FieldQaResult`, `FieldVerdict`, `QaOpts`.

Core: `qa_item` (single item), `build_qa_user_message`, `parse_qa_response`, `compute_verdict`.

### `ai/promote.rs` — Metadata promotion

Types: `PromoteOpts`, `PromoteResult`.

Core: `promote_metadata` — filters fields by confidence, writes via metadata modify API.

### `ai/client.rs` — Vision message support (extension)

Added `MessageContent` enum (Text, ImageBase64), `ImageUrlContent`, `chat_vision` method. Existing `chat()` refactored to use shared `send_request` retry loop.

## CLI Interface

### `ia ai qa`

```
ia ai qa [OPTIONS] [IDENTIFIERS]...

Input:  --search <QUERY>, --itemlist <FILE>, stdin
QA:     --model, --base-url, --api-key, --temperature, --confidence, --min-field-confidence
Output: --json (JSONL), --promote, --dry-run, --dashboard
```

### `ia ai config`

```
ia ai config show <COLLECTION>    [--json]
ia ai config list                 [--json]
ia ai config create <COLLECTION>  [--from-file, --model, --prompt, --pages, --schema-file, --dry-run]
ia ai config edit <COLLECTION>    [--editor, --set-model, --set-prompt, --set-pages]
```

## Feature Flags

```toml
alpha = ["ai-qa"]          # alpha now means QA + config
ai-qa = []                 # QA pipeline + config CRUD
ai-analyze = []            # shelved: original analyze mode
```

## Implementation Phases

1. **Foundation** — ia_config, extracted_metadata, zip, client vision support
2. **QA Pipeline** — qa types, prompt builder, pipeline, confidence scoring
3. **Promotion + CLI** — promote, feature flags, CLI subcommands
4. **TUI + Polish** — QA dashboard, integration tests, docs
