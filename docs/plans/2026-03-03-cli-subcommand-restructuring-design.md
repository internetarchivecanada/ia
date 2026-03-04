# CLI Subcommand Restructuring

**Date**: 2026-03-03
**Status**: Approved

## Overview

Restructure `ia` commands to use sub-subcommands where distinct operations are currently crammed behind flags. The `ia config` command established the sub-subcommand pattern — this design extends it to `metadata`, `search`, and `ai`.

### Goals

- Clearer help menus — each subcommand documents only its own options
- Fewer flag conflicts and mutually exclusive groups
- Cleaner interface for machines (`--json` on every leaf)
- Leverage the Rust port to clean up the CLI rather than carrying Python baggage

### Principles

- **Subcommands for distinct operations**, flags for modifiers on those operations
- **Soft migration** — existing bare forms (`ia metadata ID`, `ia search 'query'`, `ia ai ID`) still work as defaults
- **No second-level options** — options live at the leaf subcommand only (never `ia metadata --opt subcmd --opt`)
- **`--json` on every leaf** — consistent agent-friendly output
- **`-m, --metadata`** — consistent field:value flag name across all metadata write subcommands

### Commands NOT Changing

| Command | Reason |
|---------|--------|
| `download` | Single operation with modifiers. No distinct sub-operations. |
| `list` / `ls` | Single operation with output format flags. |
| `status` | Simple: joblog path + `--json`. |
| `completions` | Simple: shell positional. |
| `update` | Simple: `--check` + `--json`. |
| `config` | Already restructured with sub-subcommands (login, show, check, whoami, print-cookies, print-auth). |

---

## Metadata (`ia metadata`)

### Current Problems

- Read and write crammed together with implicit mode detection (`is_write` checks if any write flags have values)
- 5 mutually exclusive write flags (`-m`, `-a`, `-A`, `-I`, `-r`) sharing one command
- `--spreadsheet` should work for both export (read) and import (write) but can't as a single flag
- `--exists` and `--formats` are meaningless in write mode
- Write-only options (`--target`, `--expect`, `--priority`, `--dry-run`) clutter read help

### New Structure

#### Read (bare command, soft migration)

```
ia metadata ID                              # show metadata (default)
ia metadata ID --exists                     # check existence (exit code 0/1)
ia metadata ID --formats                    # list file formats
ia metadata ID --json                       # JSON output
ia metadata ID --pretty                     # pretty-print
```

Bare `ia metadata ID` works because clap treats the first positional arg as an identifier when it isn't a known subcommand name.

#### Export (bulk read)

```
ia metadata export --search 'query'         # bulk read → JSONL stdout
ia metadata export --itemlist ids.txt       # bulk read from file
ia metadata export -o data.csv              # bulk read → spreadsheet file
ia metadata export --search 'q' -o out.xlsx # bulk read → XLSX
```

Options:
- `-o, --output <FILE>` — output file (format inferred from extension: .csv, .tsv, .xlsx, .jsonl)
- `--json` — JSONL to stdout (default when no `-o`)
- Batch input: `--search`, `--itemlist`, multiple positional identifiers, stdin

#### Write Subcommands

```
ia metadata modify ID -m title:X            # set/replace field(s)
ia metadata modify ID -m title:X -m date:Y  # multiple fields
ia metadata append ID -m description:more   # append to string field(s)
ia metadata append-list ID -m subject:new   # append to list field(s)
ia metadata insert ID -m 'subject[0]:first' # insert at index
ia metadata remove ID -m subject:old        # remove value(s)
```

Shared options on each write subcommand:
- `-m, --metadata <FIELD:VALUE>` — repeatable field:value pairs
- `--target <TARGET>` — "metadata" (default) or "files/FILENAME"
- `--expect <FIELD:VALUE>` — optimistic concurrency check
- `--priority <N>` — task priority
- `--reduced-priority` — accept reduced priority
- `--dry-run` — preview without writing
- `--json` — structured output
- Batch input: `--search`, `--itemlist`, multiple positional identifiers, stdin

#### Import (bulk write from file)

```
ia metadata import data.csv                 # bulk write from file
ia metadata import data.csv --dry-run       # preview bulk write
ia metadata import data.xlsx                # supports CSV/TSV/XLSX/ODS/JSONL
```

Column convention: default operation is `modify`. Column prefixes override per-column:
- `append:description` — append to string field
- `append-list:subject` — append to list field
- `insert:subject[0]` — insert at index
- `remove:subject` — remove value

Options:
- Positional: `<FILE>` path (required)
- `--target <TARGET>` — "metadata" (default) or "files/FILENAME"
- `--expect <FIELD:VALUE>` — optimistic concurrency check
- `--priority <N>` — task priority
- `--reduced-priority` — accept reduced priority
- `--dry-run` — preview without writing
- `--json` — structured output

### Help Text

`ia metadata --help`:
```
Read or modify Internet Archive item metadata

Usage: ia metadata [IDENTIFIER] [COMMAND]

Commands:
  export       Bulk export metadata to stdout or file
  modify       Set or replace metadata field values
  append       Append to string metadata fields
  append-list  Append to list metadata fields
  insert       Insert values at a position in list fields
  remove       Remove values from metadata fields
  import       Bulk write metadata from a spreadsheet or data file

Arguments:
  [IDENTIFIER]  Item identifier (shows metadata as JSON)

Options:
      --exists   Check if item exists (exit code 0/1)
      --formats  List available file formats
      --json     Output as JSON
      --pretty   Pretty-print JSON output
```

---

## Search (`ia search`)

### Current Problems

- `--fts` silently swaps the backend with different capabilities
- Users don't know which backend they're hitting or what it supports
- Advanced search auto-paginates when it shouldn't (page-based by nature)
- `-n, --count` naming is confusing alongside `--num-found`
- FTS is missing `!L` prefix for non-DSL queries (bug)

### New Structure

```
ia search 'query'                           # default → scrape
ia search scrape 'query'                    # explicit scrape
ia search scrape 'query' --sort 'downloads desc'
ia search scrape 'query' --itemlist         # identifiers only
ia search scrape 'query' -n                 # count only

ia search advanced 'query'                  # page-based, no auto-pagination
ia search advanced 'query' --sort 'date desc'

ia search fts 'query'                       # full-text search, scroll-based
ia search fts 'query' --dsl                 # raw Elasticsearch DSL mode
ia search fts 'query' --scope index_name    # scope filter
```

Bare `ia search 'query'` works because when the first positional arg isn't `scrape`, `advanced`, or `fts`, it defaults to scrape.

### Backend Capabilities

| Option | `scrape` | `advanced` | `fts` |
|--------|----------|------------|-------|
| `-n, --num-found` | Yes | Yes | Yes |
| `--itemlist` | Yes | Yes | Yes |
| `--json` | Yes | Yes | Yes |
| `-p, --parameters` | Yes | Yes | Yes |
| `--timeout` | Yes | Yes | Yes |
| `-s, --sort` | Yes | Yes | No |
| `-f, --field` / `--fields` | Yes | Yes | No |
| `--dsl` | No | No | Yes |
| `--scope` | No | No | Yes |
| `--size` | No | No | Yes |
| `--from` | No | No | Yes |
| Auto-pagination | Yes (cursor) | No (single page) | Yes (scroll) |

### Bug Fix: FTS `!L` Prefix

The Python `internetarchive` library prepends `!L` to non-DSL FTS queries for literal text search. The Rust implementation currently sends raw queries without this prefix. Fix: add `!L` prefix by default, skip when `--dsl` is passed.

### Help Text

`ia search --help`:
```
Search the Internet Archive

Uses the scrape API by default. Choose a subcommand for a different backend.

Usage: ia search [QUERY] [COMMAND]

Commands:
  scrape    Search via scrape API (cursor-based, default)
  advanced  Search via advanced search API (page-based, single page)
  fts       Full-text search (scroll-based)

Arguments:
  [QUERY]  Search query (uses scrape API by default)

Examples:
  $ ia search "collection:nasa"            # scrape (default)
  $ ia search scrape "collection:nasa"     # explicit
  $ ia search advanced "collection:nasa"   # page-based
  $ ia search fts "apollo 11"              # full-text
```

---

## AI (`ia ai`)

### Current Problems

- `--undo` conflicts with all other flags (headless, record-only, dry-run)
- Undo is a fundamentally different operation — no LLM, no focus flags, no pipeline

### New Structure

```
ia ai ID                                    # interactive TUI review (default)
ia ai ID --headless                         # auto-accept, JSONL to stdout
ia ai ID --record-only -o changes.json      # save suggestions locally
ia ai ID --dry-run                          # preview only
ia ai --search 'query' --headless           # batch

ia ai undo session.jsonl                    # reverse previous changes
ia ai undo session.jsonl --dry-run          # preview undo
ia ai undo session.jsonl --json             # structured output
```

Bare `ia ai ID` works — `undo` is the only subcommand, so any positional arg that isn't `undo` is treated as an identifier.

### Main Command Options

Unchanged from current design. Modes stay as flags (same pipeline, different review strategy):

- **Input sources**: positional identifiers, `--itemlist`, `--search`, stdin
- **Modes**: `--headless`, `--record-only`, `--dry-run`
- **LLM config**: `--base-url`, `--api-key`, `--model`, `--temperature`, `--max-tokens`
- **Focus**: `--dates-only`, `--titles-only`, `--descriptions-only`, `--missing-fields`, `--schema-fix`, `--typos`, `--only-fields`, `--exclude-fields`
- **Prompt**: `--prompt-file`, `--system-prompt`
- **Performance**: `--ai-jobs`, `--prefetch`, `--max-tokens-budget`
- **Output**: `-o, --output`, `--json`

### `undo` Subcommand Options

Focused, minimal — no LLM config, no focus flags, no pipeline options:

- Positional: `<JOBLOG>` path (required)
- `--dry-run` — preview reversals without applying
- `--json` — structured output

---

## Implementation Notes

### Clap Pattern for Optional Subcommands

For soft migration (bare `ia metadata ID`, `ia search 'query'`, `ia ai ID`), use clap's `subcommand_required = false` with a fallback positional argument:

```rust
#[derive(Parser)]
#[command(subcommand_required = false)]
struct MetadataArgs {
    /// Item identifier (bare read mode)
    #[arg()]
    identifier: Option<String>,

    // ... read-mode flags ...

    #[command(subcommand)]
    command: Option<MetadataCommand>,
}

enum MetadataCommand {
    Export(ExportArgs),
    Modify(ModifyArgs),
    Append(AppendArgs),
    AppendList(AppendListArgs),
    Insert(InsertArgs),
    Remove(RemoveArgs),
    Import(ImportArgs),
}
```

### Shared Write Args

Use a shared struct with `#[command(flatten)]` to avoid duplicating write options across modify/append/append-list/insert/remove:

```rust
#[derive(Args)]
struct WriteOpts {
    #[arg(short = 'm', long)]
    metadata: Vec<String>,

    #[arg(long, default_value = "metadata")]
    target: String,

    #[arg(long)]
    expect: Vec<String>,

    #[arg(long)]
    priority: Option<i32>,

    #[arg(long)]
    reduced_priority: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    json: bool,
}
```

### Shared Batch Input Args

Similarly, share batch input sources across subcommands:

```rust
#[derive(Args)]
struct BatchInput {
    #[arg()]
    identifiers: Vec<String>,

    #[arg(long)]
    itemlist: Option<PathBuf>,

    #[arg(long)]
    search: Option<String>,
}
```

---

## Summary

| Command | Change | Subcommands |
|---------|--------|-------------|
| **metadata** | Major restructure | `export`, `modify`, `append`, `append-list`, `insert`, `remove`, `import` + bare read |
| **search** | Split by backend | `scrape` (default), `advanced`, `fts` + bare defaults to scrape |
| **ai** | Extract undo | `undo` + bare runs analysis pipeline |
| config | Already done | login, show, check, whoami, print-cookies, print-auth |
| download | No change | — |
| list | No change | — |
| status | No change | — |
| completions | No change | — |
| update | No change | — |

### Cross-Cutting

- `--json` on every leaf subcommand
- No second-level options — options live at the leaf only
- Soft migration — bare forms preserved as defaults
- FTS bug fix: add `!L` prefix for non-DSL queries
- `-n, --num-found` — consistent naming (dropped confusing `--count` alias)
