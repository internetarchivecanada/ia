# `ia ai` — AI-Assisted Metadata Cleanup

**Date**: 2026-02-21
**Status**: Design approved, awaiting metadata write support
**Prerequisite**: Metadata write support in ia-core (Phase 0)

## Overview

A new `ia ai` command that uses LLMs to suggest and apply metadata cleanup for Internet Archive items. The LLM analyzes item metadata and suggests improvements: fixing typos, normalizing dates, filling missing fields, ensuring schema conformance, and more.

Designed for all users — from casual contributors cleaning up a few items to power curators processing thousands.

## Architecture

### Pipeline Design

Four-stage async pipeline with bounded channels between stages:

```
┌─────────────┐    bounded     ┌─────────────┐    bounded     ┌─────────────┐    bounded     ┌─────────────┐
│   Source     │───channel────▶│  Analyzer    │───channel────▶│  Reviewer    │───channel────▶│   Writer    │
│             │   (prefetch)   │             │   (prefetch)   │             │   (32)         │             │
│ fetch meta  │               │ LLM calls   │               │ TUI/headless│               │ IA writes   │
│ from IA API │               │ parse JSON   │               │ user I/O    │               │ + joblog    │
└─────────────┘               └─────────────┘               └─────────────┘               └─────────────┘
```

**Stage 1 — Source**: Collects identifiers from positional args, `--itemlist`, stdin, or `--search`. Fetches `ItemMetadata` via `client.get_item(id)`. Bounded channel (capacity = `--prefetch`, default 5) provides backpressure.

**Stage 2 — Analyzer**: Calls LLM API with item metadata + focus config. Parses structured JSON response into `Vec<MetadataChange>`. Supports `--ai-jobs` for concurrent LLM requests (default 1).

**Stage 3 — Reviewer**: Interactive TUI, headless auto-accept, or record-only mode. User accepts/rejects/edits suggestions per field. Sends approved changes to writer.

**Stage 4 — Writer**: Applies metadata changes via ia-core write functions (fire-and-forget from user's perspective). Logs every result to joblog. Surfaces failures asynchronously.

### Code Organization

All core logic lives in `ia-core` for reuse by both CLI (TUI) and future GUI (Slint):

- `ia-core/src/ai/mod.rs` — Module root
- `ia-core/src/ai/types.rs` — MetadataChange, ItemAnalysis, FocusConfig, AiConfig
- `ia-core/src/ai/client.rs` — LlmClient (OpenAI-compatible HTTP)
- `ia-core/src/ai/prompt.rs` — System prompt builder with IA schema rules
- `ia-core/src/ai/pipeline.rs` — Pipeline orchestrator

TUI lives in `ia-cli`:

- `ia-cli/src/tui/ai.rs` — AI review TUI dashboard
- `ia-cli/src/commands/ai.rs` — CLI argument parsing and orchestration

## Data Model

```rust
/// A single suggested change to one metadata field
struct MetadataChange {
    field: String,
    old_value: Option<serde_json::Value>,
    new_value: serde_json::Value,
    reason: String,
    category: ChangeCategory,
    status: ChangeStatus,
}

enum ChangeCategory { Schema, Content, CrossField, MissingField }
enum ChangeStatus { Pending, Accepted, Rejected, Edited(serde_json::Value) }

/// All suggestions for one item
struct ItemAnalysis {
    identifier: String,
    metadata: ItemMetadata,
    changes: Vec<MetadataChange>,
    token_usage: Option<TokenUsage>,
    analyzed_at: chrono::DateTime<Utc>,
}

/// Token tracking (best-effort, None for local LLMs)
struct TokenUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    cost_estimate_usd: Option<f64>,
}

/// Result of applying changes to an item
struct ApplyResult {
    identifier: String,
    changes_applied: Vec<MetadataChange>,
    status: ApplyStatus,
    error: Option<String>,
}
```

## LLM Integration

### Backend

OpenAI-compatible API (works with OpenAI, Anthropic via proxy, Ollama, LM Studio, vLLM, llama.cpp, etc.).

Standard `/chat/completions` endpoint. System prompt + user message → structured JSON response.

### Configuration

Sources (priority: CLI flags > env vars > ia.ini):

- **ia.ini**: `[ai]` section with `base_url`, `api_key`, `model`, `max_tokens`, `temperature`
- **Env vars**: `IA_AI_BASE_URL`, `IA_AI_API_KEY`, `IA_AI_MODEL`
- **CLI flags**: `--base-url`, `--api-key`, `--model`, `--temperature`, `--max-tokens`

### Prompt Design

System prompt embeds IA metadata schema rules:

- **Role**: Internet Archive metadata specialist
- **Output format**: JSON array of `{field, old_value, new_value, reason, category}`
- **Schema rules**: Required/recommended fields, ISO 8601 dates, ISO 639-2/B language codes, valid mediatypes, subject formatting
- **Cleanup rules**:
  - Extract dates from titles → date field (always)
  - Fix capitalization (title case, not ALL CAPS)
  - Fix obvious typos
  - Normalize dates to ISO 8601
  - Normalize language codes
  - Suggest missing required/recommended fields
  - Generate descriptions from available context
  - Suggest subjects/tags from title + description + collection
  - Never invent information — only infer from available context
- **Constraints**: Never modify identifier/mediatype, never remove valid data, be conservative
- **Focus injection**: Narrowed instructions when --focus flags are set
- **Custom rules**: Injected from --prompt-file if provided

User message contains the full `ItemMetadata` JSON (metadata fields + files array).

### Token/Cost Tracking

Track tokens and cost when the API reports usage info (most cloud APIs do). Gracefully degrade for local LLMs that don't report usage. Display running totals in TUI status bar. Support `--max-tokens-budget` to set a ceiling.

## TUI Dashboard

### Layout

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ia ai  ■■■■■■■■■■░░░░░░░░░░  12/50 items   ⏱ 3m   $0.42   ⏳ 3 ready     │
├────────────────────────────────┬─────────────────────────────────────────────┤
│  Item: nasa_photo_apollo11    │  Suggested Changes (4)                      │
│                               │                                             │
│  title: nasa photo from       │  ✏ title                                    │
│         1969-07-20            │    "nasa photo from 1969-07-20"             │
│  date: (empty)                │    → "NASA Apollo 11 Photo"                 │
│  description: (empty)         │    Reason: Fixed capitalization, removed    │
│  mediatype: image             │           date (moved to date field)        │
│  collection: nasa             │                                             │
│  subject: (empty)             │  ✚ date                                     │
│  language: (empty)            │    (empty)                                  │
│  creator: (empty)             │    → "1969-07-20"                           │
│                               │    Reason: Extracted ISO date from title    │
│  [f] toggle files             │                                             │
│                               │  ✚ description                             │
│                               │    (empty)                                  │
│                               │    → "Photograph from the Apollo 11..."     │
│                               │    Reason: Generated from title/collection  │
│                               │                                             │
│                               │  ✏ subject                                  │
│                               │    (empty)                                  │
│                               │    → ["Apollo 11", "NASA", "Moon", "1969"]  │
│                               │    Reason: Inferred from title/collection   │
├──────────────────────────────────────────────────────────────────────────────┤
│ [A]ccept All  [a]ccept  [r]eject  [e]dit  [s]kip item  [q]uit   ↑↓ navigate│
└──────────────────────────────────────────────────────────────────────────────┘
```

### Panels

- **Header**: Progress bar (items reviewed / total), elapsed time, token cost (if available), prefetch queue status
- **Left panel**: Current item metadata fields (read-only). Files hidden by default, toggle with `f`
- **Right panel**: Scrollable list of suggested changes. Each shows icon (✏ modify / ✚ add), field name, old → new values, reason, and status indicator (✓ accepted / ✗ rejected / ✎ edited / ○ pending)
- **Status bar**: Context-sensitive keybinding help

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `A` | Accept all changes for this item, advance to next |
| `a` | Accept highlighted change |
| `r` | Reject highlighted change |
| `e` | Edit highlighted change (inline text input) |
| `s` | Skip item entirely (no changes applied) |
| `↑/↓` or `j/k` | Navigate between suggested changes |
| `Tab` | Switch focus between left/right panels |
| `Enter` | Confirm current item (apply accepted changes), advance |
| `f` | Toggle file list in left panel |
| `q` | Quit (with confirmation if pending changes exist) |

### Mouse Support

Keyboard-first, mouse optional. Click on fields/buttons works but isn't required.

## Operating Modes

### Interactive TUI (default)

Full TUI dashboard. User reviews each item, accepts/rejects/edits suggestions. Accepted changes are applied via metadata write API and logged to joblog.

### Record-Only (`--record-only`)

TUI dashboard for review, but instead of writing to IA, saves accepted changes to a local JSON file (`--output <file>`). Can be applied later via `ia metadata` batch commands.

### Headless (`--headless`)

No TUI. Auto-accepts all LLM suggestions. Outputs changes as JSONL to stdout. Applies changes (unless `--dry-run`). Useful for CI/scripting with trusted prompts.

### Dry Run (`--dry-run`)

Analyze and display suggestions without applying anything. Works with all modes.

## CLI Interface

```
ia ai [OPTIONS] [IDENTIFIERS]...

Input Sources:
    --itemlist <FILE>       Read identifiers from file (one per line)
    --search <QUERY>        Use search results as input

Modes:
    --headless              Auto-accept all, output to stdout/joblog (no TUI)
    --record-only           TUI review, save to local JSON instead of writing
    --dry-run               Show suggestions without applying

LLM Configuration:
    --base-url <URL>        LLM API base URL
    --api-key <KEY>         LLM API key
    --model <MODEL>         Model name
    --temperature <FLOAT>   Temperature (default: 0.2)
    --max-tokens <N>        Max response tokens (default: 4096)

Focus (category flags, combinable):
    --dates-only            Only date-related changes
    --titles-only           Only title changes
    --descriptions-only     Only description changes
    --missing-fields        Only fill empty fields
    --schema-fix            Only schema conformance fixes
    --typos                 Only typo fixes

Field-level control:
    --only-fields <F,F,...>     Only these fields
    --exclude-fields <F,F,...>  Never these fields

Prompt:
    --prompt-file <FILE>    Custom/additional prompt rules
    --system-prompt <TEXT>  Override system prompt entirely

Performance:
    --ai-jobs <N>           Concurrent LLM requests (default: 1)
    --prefetch <N>          Items to prefetch ahead (default: 5)
    --max-tokens-budget <N> Stop after this many total tokens

Undo:
    --undo <JOBLOG>         Reverse changes recorded in a joblog file

Output:
    --output <FILE>         Write accepted changes to JSON file (record-only)
```

Global flags (`-j`, `--joblog`, `-q`, `-H`, etc.) inherited from parent CLI.

## Joblog Integration

### Entry Format

```jsonl
{"ts":"2026-03-15T10:30:00Z","op":"ai","item":"nasa_photo","status":"ok","changes":[{"field":"date","old":null,"new":"1969-07-20"},{"field":"title","old":"nasa photo from 1969-07-20","new":"NASA Apollo 11 Photo"}],"tokens":{"prompt":1200,"completion":300},"elapsed_ms":2100}
{"ts":"2026-03-15T10:30:05Z","op":"ai","item":"bad_item","status":"error","error":"metadata write failed: 403","changes":[],"elapsed_ms":500}
{"ts":"2026-03-15T10:30:10Z","op":"ai","item":"skipped_item","status":"skipped","changes":[],"elapsed_ms":0}
```

### Undo Support

`ia ai --undo <joblog>`:
1. Read joblog, filter for `op: "ai"` and `status: "ok"` entries
2. For each entry, reverse changes: swap old ↔ new values
3. Apply reversals via the same core metadata write function
4. Log undo operations with `op: "ai-undo"`

The `ia status` command must be updated to understand `ai` and `ai-undo` operations.

## Completion Summary

On exit, display:
- Items reviewed / total
- Items accepted / skipped / errored
- Changes applied / rejected
- Total tokens used / estimated cost (if available)
- Elapsed time

All data also written to joblog for auditing.

## Dependencies

### New Crate Dependencies (ia-core)

- `reqwest` — already present, reuse for LLM API calls (separate client instance)
- `serde_json` — already present

No new external crates required for MVP. The OpenAI-compatible API is simple enough to call with raw reqwest.

### Prerequisite

**Metadata write support** must be implemented first. The `ia ai` command depends on a core `metadata::modify()` function in ia-core that both `ia metadata --modify` and `ia ai` will use. See separate design doc for metadata write.

## Phasing

| Phase | Scope | Depends on |
|-------|-------|------------|
| 0 | Metadata write support (separate design) | — |
| 1 | Core AI infrastructure (types, LLM client, prompt builder, config) | Phase 0 |
| 2 | Pipeline & headless/record-only modes | Phase 1 |
| 3 | Interactive TUI dashboard | Phase 2 |
| 4 | Polish: undo, custom prompts, ia status update, file content analysis | Phase 3 |
| Future | GUI integration via shared ia-core types | Phase 1+ |

## Future Extensions

- **File content analysis**: Download and send OCR text, captions, etc. to the LLM for richer inference (opt-in via `--use-files`)
- **GUI integration**: Slint desktop app consumes same ia-core AI types and pipeline
- **Per-collection prompt templates**: Custom prompt snippets stored per collection
- **Batch undo**: Reverse all changes from a session in one operation
- **Confidence scoring**: LLM rates confidence per suggestion, auto-accept high-confidence changes
