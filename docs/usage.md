# Usage

## Quick start

```sh
# Download all files from an item
ia download nasa

# Search for items in a collection
ia search "collection:nasa AND mediatype:texts"

# List files in an item
ia list nasa

# View metadata for an item
ia metadata nasa
```

## Commands

### `ia download`

Download files from one or more Internet Archive items.

```sh
ia download <IDENTIFIER>... [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>...` | Item identifier(s) to download |
| `--itemlist <PATH>` | File containing identifiers (one per line) |
| `-s, --search <QUERY>` | Download items matching a search query |
| `-g, --glob <PATTERN>` | Filter files by glob pattern (pipe-separated: `"*.mp4\|*.webm"`) |
| `-e, --exclude <PATTERN>` | Exclude files matching pattern |
| `-f, --format <FORMAT>` | Filter by file format (repeatable) |
| `--source <TYPE>` | Filter by source type: `original`, `derivative`, `metadata` |
| `--exclude-source <TYPE>` | Exclude by source type |
| `--destdir <PATH>` | Destination directory (repeatable for disk pool, default: `.`) |
| `--no-directories` | Don't create item subdirectory |
| `-C, --checksum` | Verify checksums (slower, reads every local file) |
| `-R, --retries <N>` | Max retries per file (default: 5) |
| `--no-timestamps` | Don't set file modification times |
| `--dry-run` | Show what would be downloaded without downloading |
| `--items <N>` | Concurrent items for batch/search downloads (default: 2) |
| `--dashboard` | Full-screen dashboard mode |
| `--json` | Output results as JSONL (one object per line) |

#### Examples

```sh
# Download all files from an item
ia download nasa

# Download only JPEG files
ia download nasa --glob "*.jpg"

# Download only original files in MP4 format
ia download nasa --source original --format "MPEG4"

# Download multiple items
ia download item1 item2 item3

# Batch download from a search query with 4 concurrent items
ia download --search "collection:nasa AND mediatype:image" --items 4

# Batch download from a file of identifiers
ia download --itemlist items.txt

# Preview what would be downloaded
ia download nasa --dry-run

# Download with JSON output (for scripts/agents)
ia download nasa --json
```

### `ia search`

Search the Internet Archive. Uses the scrape API by default, or the full-text search backend with `--fts`.

```sh
ia search <QUERY> [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<QUERY>` | Search query |
| `--itemlist` | Output identifiers only (one per line) |
| `--num-found` | Print count of matching items only |
| `-s, --sort <FIELD>` | Sort field (repeatable, e.g., `"downloads desc"`) |
| `-f, --field <FIELD>` | Fields to return (repeatable, default: identifier) |
| `-n, --count <N>` | Maximum number of results |
| `-p, --parameters <K=V>` | Extra query parameters (repeatable) |
| `--timeout <SECS>` | Request timeout in seconds |
| `--fts` | Use full-text search backend |
| `--json` | Output results as JSONL (one object per line, returns all fields) |

#### Examples

```sh
# Search for items in a collection
ia search "collection:nasa AND mediatype:texts"

# Get identifiers only (useful for piping to ia download)
ia search "subject:mars" --itemlist

# Get the number of matching items
ia search "collection:opensource" --num-found

# Full-text search
ia search "apollo 11 transcript" --fts

# Return specific fields, sorted by downloads
ia search "mediatype:audio" --field identifier --field title --sort "downloads desc"

# Limit results and output as JSON
ia search "collection:nasa" --count 10 --json

# Pipe search results into download
ia search "collection:nasa" --itemlist | xargs ia download
```

### `ia list`

List files in an Internet Archive item. Alias: `ia ls`.

```sh
ia list <IDENTIFIER> [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Item identifier |
| `--columns <COLS>` | Columns to show (comma-separated: `name,size,format,source,md5,mtime`) |
| `-g, --glob <PATTERN>` | Filter files by glob pattern |
| `--source <TYPE>` | Filter by source type: `original`, `derivative`, `metadata` |
| `--location` | Print full download URLs |
| `-a, --all` | Show all file metadata as JSON |
| `-v, --verbose` | Print column headers |

#### Examples

```sh
# List files in an item
ia list nasa

# Show specific columns with headers
ia list nasa --columns name,size,format --verbose

# Show download URLs for original files
ia list nasa --source original --location

# Filter by glob pattern
ia list nasa --glob "*.pdf"

# Dump full file metadata as JSON
ia list nasa --all
```

### `ia metadata`

Read or modify item metadata.

```sh
ia metadata <IDENTIFIER>... [OPTIONS]
```

#### Reading metadata

By default, `ia metadata` displays the full metadata JSON for an item.

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Item identifier |
| `-e, --exists` | Check if item exists (exit code 0=yes, 1=no) |
| `-F, --formats` | List available file formats |
| `--pretty` | Pretty-print JSON output |

```sh
# View metadata for an item
ia metadata nasa

# Pretty-print metadata
ia metadata nasa --pretty

# Check if an item exists
ia metadata nasa --exists

# List file formats in an item
ia metadata nasa --formats
```

#### Writing metadata

Use write flags to modify metadata fields. Write flags are mutually exclusive (use one type per invocation).

| Flag | Description |
|------|-------------|
| `-m, --modify <K:V>` | Set field to value (repeatable) |
| `-a, --append <K:V>` | Append to string field (repeatable) |
| `-A, --append-list <K:V>` | Append to list field (repeatable) |
| `-I, --insert <K[N]:V>` | Insert at index in list field (repeatable) |
| `-r, --remove <K:V>` | Remove value from field (repeatable) |
| `-s, --spreadsheet <PATH>` | Bulk update from file (CSV, TSV, XLSX, ODS, JSONL) |

| Write option | Description |
|------|-------------|
| `--target <TARGET>` | Target: `metadata` (default) or `files/FILENAME` |
| `--expect <K:V>` | Optimistic concurrency check (fail if field doesn't match) |
| `--priority <N>` | Task priority (default: 0 single, -5 batch) |
| `--reduced-priority` | Accept reduced priority to reduce rate limiting |
| `--dry-run` | Show changes without writing |

| Bulk input | Description |
|------|-------------|
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | Use search results as input |

```sh
# Set a metadata field
ia metadata myitem --modify="description:Updated description"

# Set multiple fields at once
ia metadata myitem --modify="title:New Title" --modify="subject:science"

# Append to a string field
ia metadata myitem --append="description: (updated 2026)"

# Add a value to a list field (e.g., add a subject tag)
ia metadata myitem --append-list="subject:astronomy"

# Insert at a specific index in a list
ia metadata myitem --insert="collection[0]:featured"

# Remove a value from a field
ia metadata myitem --remove="subject:outdated-tag"

# Preview changes without writing
ia metadata myitem --modify="title:New Title" --dry-run

# Modify file-level metadata
ia metadata myitem --target="files/image.jpg" --modify="title:Photo caption"

# Bulk update from a spreadsheet
ia metadata --spreadsheet updates.csv

# Bulk modify items from a search query
ia metadata --search "collection:mybooks" --modify="rights:public domain"

# Bulk modify items from a file of identifiers
ia metadata --itemlist items.txt --modify="subject:archived"
```

### `ia status`

Show a summary of a job log file. Displays total operations, success/failure/skip counts, and lists failed files with error messages.

```sh
ia status --joblog <PATH>
```

#### Flags

| Flag | Description |
|------|-------------|
| `--joblog <PATH>` | Path to job log file (required) |

#### Example

```sh
ia status --joblog downloads.jsonl
```

### `ia completions`

Generate shell completion scripts. Prints a completion script to stdout.

```sh
ia completions <SHELL> [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<SHELL>` | Shell to generate completions for: `bash`, `zsh`, `fish`, `elvish`, `powershell` |
| `--rename <NAME>` | Override the binary name in generated completions |

#### Examples

```sh
# Generate completions for fish
ia completions fish > ~/.config/fish/completions/ia.fish

# Generate completions for bash
ia completions bash > ~/.local/share/bash-completion/completions/ia

# Generate completions for zsh
ia completions zsh > ~/.local/share/zsh/site-functions/_ia
```

## Global options

These options can be used with any subcommand:

| Flag | Description |
|------|-------------|
| `-c, --config-file <PATH>` | Path to configuration file |
| `-j, --jobs <N>` | Concurrent file operations (default: 2) |
| `-i, --insecure` | Allow insecure (HTTP) connections |
| `-H, --host <HOST>` | Override the archive.org host |
| `--user-agent-suffix <STRING>` | Append to the default User-Agent |
| `--joblog <PATH>` | Write operation results to a JSONL log file |
| `--retry-failed` | Retry failed operations from a job log |
| `-q, --quiet` | Suppress output (repeat for more quiet: `-q` summary only, `-qq` silent) |
| `-l, --log` | Enable logging |
| `-d, --debug` | Enable debug output |

## Configuration

`ia` reads settings from an INI configuration file, compatible with the Python `ia` tool's format. The default location is `~/.config/internetarchive/ia.ini`.

```ini
[general]
host = archive.org
secure = true
```

Override the config file path with `--config-file`:

```sh
ia --config-file ~/my-ia.ini download nasa
```

## Advanced features

### Job logging

Track operations with `--joblog`. The log is a JSONL file (one JSON object per line) recording the outcome of each file operation.

```sh
# Download with job logging
ia download nasa --joblog downloads.jsonl

# View job log summary
ia status --joblog downloads.jsonl

# Retry failed downloads from the log
ia download nasa --joblog downloads.jsonl --retry-failed
```

### Disk pool

Distribute downloads across multiple disks by passing `--destdir` multiple times:

```sh
ia download --search "collection:nasa" --destdir /mnt/disk1 --destdir /mnt/disk2
```

Items are assigned to the disk with the most free space. If a disk fills up, downloads automatically fail over to the next available disk.

### Dashboard mode

A full-screen terminal dashboard for monitoring batch downloads, built with ratatui (ships by default):

```sh
ia download --search "collection:nasa" --dashboard
```

The dashboard shows panels for items, disks, errors, and throughput. Press `q` to quit.

Note: `--dashboard` and `--json` are mutually exclusive.

### JSON output for agents

Commands that support `--json` switch their stdout to structured JSON/JSONL output, making `ia` easy to use from scripts, AI agents, and MCP tool servers.

```sh
# Search results as JSONL (one JSON object per line)
ia search "collection:nasa" --json

# Download results as JSONL
ia download nasa --json

# Pipe search JSON into download
ia search "collection:nasa" --json | ia download --itemlist /dev/stdin
```

When `--json` is active:

- **stdout** emits JSON (single object) or JSONL (one object per line for streaming/batch operations)
- **stderr** emits structured error JSON: `{"error": {"code": "...", "message": "..."}}`
- Progress bars, color, and decorative output are suppressed
- Exit codes are binary: `0` for success, `1` for failure (details in stderr JSON)

Currently supported on: `ia download`, `ia search`.

## Architecture

The project is a Cargo workspace with three crates:

- **ia-core** -- Library crate with the client, API types, download engine, search backends, and utilities
- **ia-cli** -- Binary crate with the CLI interface, progress display, and TUI dashboard
- **ia-gui** -- Desktop GUI application built with Slint

See [the design doc](plans/2026-02-20-ia-rust-port-design.md) for full architectural details.
