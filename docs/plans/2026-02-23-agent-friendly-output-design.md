# Agent-Friendly Structured Output

**Date**: 2026-02-23
**Author**: Jake (jjjake)
**Status**: Approved
**Repo**: `internetarchivecanada/ia`

## Motivation

The `ia` CLI is not just a human interface — it's a **platform** for three classes of machine consumers:

1. **AI coding agents** (Claude Code, Cursor, Copilot) that shell out to CLI tools, read `--help`, construct commands, and parse stdout
2. **MCP tool servers** that wrap each CLI command as a typed tool with structured inputs/outputs
3. **Internal software** (book scanning pipelines, batch archiving tools) that calls `ia` programmatically

In the agentic coding paradigm, a well-designed CLI with structured output is a better integration surface than a language-specific library. The CLI encapsulates all IA API complexity (S3 quirks, rate limiting, retry logic, three search backends, metadata patch format) behind a stable, language-agnostic interface. `--help` is the documentation, flags are the parameters, and stdout is the return value.

This design establishes conventions that make every command machine-usable without sacrificing the human experience.

## Convention: `--json` Flag

Every command gets a `--json` flag as a **subcommand option** (not a global flag). This is because:

- The JSON output shape is fundamentally different per command (JSONL streaming for download, single object for status, field selection for search)
- Commands can be incrementally migrated — no confusion about unsupported global flags
- Matches the existing pattern (`ia search --json` already ships)

When `--json` is active on a given command:

- **stdout**: Data as JSON (single object) or JSONL (one object per line for streaming/batch)
- **stderr**: Errors as typed JSON objects
- **Exit codes**: Binary — `0` (success) or `1` (any failure). Error details live in structured stderr output.
- **No progress bars, no color, no decorative output**

The structured error serialization is shared infrastructure in `ia-core` — each command just checks its own `--json` arg to decide whether to use it.

When `--json` is not set, behavior is unchanged — human-friendly output with colors, progress bars, tables.

## Error Schema

When `--json` is active, errors on stderr use this shape:

```json
{"error": {"code": "rate_limited", "message": "Rate limited by server", "retry_after": 30}}
```

The `code` field is a stable string for programmatic matching. The `message` field is human-readable. Additional context fields vary by error type.

### Error Codes

These map 1:1 from existing `IaError` variants in `ia-core/src/error.rs`:

| Code | Description | Extra fields |
|------|-------------|--------------|
| `not_found` | Item does not exist | `identifier` |
| `http_error` | Non-200 HTTP response | `status`, `url` |
| `rate_limited` | 429 from server | `retry_after` (seconds) |
| `checksum_mismatch` | File hash doesn't match | `file`, `expected`, `actual` |
| `disk_full` | Disk has no free space | `path` |
| `no_disk_space` | Not enough space for file | `needed` (bytes) |
| `resume_failed` | Cannot resume partial download | `file`, `reason` |
| `config_error` | Configuration problem | — |
| `auth_error` | Authentication issue | — |
| `metadata_write` | Metadata modify failed | `identifier` |
| `network` | Connection/DNS/timeout | — |
| `io` | File system error | — |
| `json_parse` | Malformed JSON from API | — |

## Per-Command Output

### `ia search` (existing — no change needed)

Already has `--json` flag. JSONL to stdout, one result object per line:

```jsonl
{"identifier": "nasa", "title": "NASA Images", "downloads": 5000}
{"identifier": "gutenberg", "title": "Project Gutenberg", "downloads": 3200}
```

### `ia metadata <id>` (minor change)

Already outputs JSON by default. With `--json`: suppress `--pretty` formatting, add structured error on stderr for not-found items.

```json
{"identifier": "nasa", "metadata": {"title": "NASA Images", ...}, "files": [...]}
```

### `ia metadata --modify ... --json` (new)

JSONL streaming, one line per item as each completes:

```jsonl
{"item": "nasa", "status": "ok", "task_id": 12345}
{"item": "mit", "status": "error", "error": {"code": "metadata_write", "message": "Server rejected patch"}}
```

### `ia list <id> --json` (new — replaces `--all`)

JSONL, one file object per line:

```jsonl
{"name": "file.pdf", "size": 4200000, "format": "PDF", "md5": "abc123", "source": "original"}
{"name": "file_meta.xml", "size": 1200, "format": "Metadata", "source": "metadata"}
```

### `ia download ... --json` (new — biggest gap today)

JSONL streaming per file:

```jsonl
{"item": "nasa", "file": "photo.jpg", "status": "ok", "bytes": 4200000, "elapsed_ms": 2100}
{"item": "nasa", "file": "video.mp4", "status": "skipped", "reason": "already_exists"}
{"item": "nasa", "file": "thumb.jpg", "status": "error", "error": {"code": "checksum_mismatch", "file": "thumb.jpg", "expected": "abc", "actual": "def"}}
```

For batch mode with `--search` or `--itemlist`, item-level summary lines:

```jsonl
{"item": "nasa", "status": "ok", "files_ok": 12, "files_skipped": 3, "files_failed": 0, "bytes": 42000000, "elapsed_ms": 8500}
{"item": "broken-item", "status": "error", "error": {"code": "not_found", "message": "Item not found"}}
```

### `ia status --joblog <path> --json` (new)

Single JSON object with summary and failure details:

```json
{
  "total": 150,
  "succeeded": 140,
  "failed": 8,
  "skipped": 2,
  "failures": [
    {"item": "x", "file": "y.jpg", "error": "timeout after 12s"},
    {"item": "z", "file": "a.pdf", "error": "checksum mismatch"}
  ]
}
```

## Flag Interactions

| Combination | Behavior |
|------------|----------|
| `--json` alone | Structured output to stdout, structured errors to stderr |
| `--json --quiet` | `--json` wins — structured output always emitted |
| `--json --pretty` | Pretty-print the JSON (for human debugging) |
| `--json --joblog` | Both work — JSONL to stdout AND joblog to file |
| `--json --dashboard` | Mutually exclusive — error if both specified |
| `--json --dry-run` | Structured dry-run output (same shape, no side effects) |

## Implementation Notes

### Shared JSON Error Serialization

Add `Serialize` to `IaError` (or a wrapper) that produces the error schema above. This is a single implementation point — all commands use it.

### Stdout/Stderr Discipline

The CLI already follows good stdout/stderr separation (data to stdout, progress to stderr). The `--json` flag extends this: when active, errors also become structured JSON on stderr instead of plain text.

### JSONL for Streaming

Batch operations (download, metadata modify, search) produce one JSON object per line. This is:
- Parseable with `jq` line-by-line
- Processable while the command is still running
- Compatible with the existing joblog format

### Backward Compatibility

- `--json` is additive — no existing behavior changes
- `--all` on `ia list` continues to work (becomes an alias or is deprecated in favor of `--json`)
- Search `--json` is already implemented and unchanged

## Example Agent Workflow

```bash
# Search for items in a collection
ia search "collection:bookscanner AND mediatype:texts" --json \
  | jq -r '.identifier' \
  | while read -r item; do

    # Check metadata
    meta=$(ia metadata "$item" --json)
    title=$(echo "$meta" | jq -r '.metadata.title')

    # Download all original files
    ia download "$item" --format "PDF" --json \
      | while read -r line; do
          status=$(echo "$line" | jq -r '.status')
          if [ "$status" = "error" ]; then
            echo "FAILED: $item/$(echo "$line" | jq -r '.file')" >&2
          fi
        done

    # Update metadata after processing
    ia metadata --modify "ocr:true" "$item" --json
done
```
