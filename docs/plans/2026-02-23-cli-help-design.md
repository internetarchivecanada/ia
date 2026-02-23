# CLI Help Text Redesign

## Problem

The CLI help text is functional but sparse. Every command has only a one-line `about`, no examples, no long descriptions, and no color. The audience includes non-technical librarians, archivists, and students alongside developers — the help text needs to serve both.

## Approach

**Approach A: Pure clap attributes** — use clap's built-in `long_about`, `after_long_help`, and `Styles` API. Color structural elements via `Styles`, and use the `color-print` crate for tasteful color in example sections. No custom templates, no external rendering crates.

## Color Scheme

Shared `clap::builder::Styles` defined once, applied to the root command (inherits to subcommands):

| Element | Style |
|---------|-------|
| Section headers (Usage, Options, Examples) | Bold green |
| Flag/option names | Bold cyan |
| Placeholders/value names | Cyan (not bold) |
| Default values | Dim |
| Example commands (in after_help) | Bold white |
| Example descriptions (in after_help) | Dim |

## Help Text Layering

Two tiers using clap's standard convention:

- **`-h` (short):** Existing one-line `about` + terse option descriptions. Quick reference for power users.
- **`--help` (long):** Adds `long_about` (2-3 sentence explanation) + `after_long_help` (2-3 real-world examples). Fuller picture for newcomers.

## Main Command

**long_about:**
> A command-line tool for interacting with the Internet Archive (archive.org). Download files, search for items, view and edit metadata, and list file contents.

**Improved subcommand one-liners:**

| Command | Current | Proposed |
|---------|---------|----------|
| download | Download files from an item | Download files from one or more items |
| list | List files in an item | List files in an item with filtering and formatting |
| metadata | Display item metadata | Read or modify item metadata |
| search | Search the Internet Archive | Search the Internet Archive |
| status | Show job log summary | Show job log summary and failed operations |
| completions | Generate shell completions | Generate shell completions for bash, zsh, fish, etc. |

## Subcommand Help Text

### download

**long_about:** Download files from the Internet Archive. Downloads all files from one or more items, with options to filter by format, glob pattern, or source type. Supports batch downloads via search queries or item lists.

**Examples:**
```
# Download all files from an item
$ ia download nasa

# Download only MP4 files
$ ia download nasa --glob "*.mp4"

# Batch download items matching a search query
$ ia download --search "collection:nasa AND mediatype:movies"
```

### search

**long_about:** Search the Internet Archive. Returns matching items using the scrape API by default, or the full-text search backend with --fts. Results can be formatted as JSON, filtered to specific fields, or output as a plain identifier list.

**Examples:**
```
# Search for items in a collection
$ ia search "collection:nasa"

# Get just the identifiers (useful for piping)
$ ia search "mediatype:audio" --itemlist

# Full-text search with JSON output
$ ia search "apollo 11" --fts --json
```

### metadata

**long_about:** Read or modify item metadata. By default, displays the full metadata JSON for an item. Use --modify, --append, --remove, and related flags to update metadata fields. Supports bulk operations via --itemlist, --search, or --spreadsheet.

**Examples:**
```
# View metadata for an item
$ ia metadata nasa

# Set a metadata field
$ ia metadata nasa --modify="description:Updated description"

# Bulk update from a spreadsheet
$ ia metadata --spreadsheet updates.csv
```

### list

**long_about:** List files in an Internet Archive item. Displays a table of files with name, size, and format by default. Use --columns to customize output, --glob to filter, or --all for full file metadata as JSON.

**Examples:**
```
# List files in an item
$ ia list nasa

# Show only original files with download URLs
$ ia list nasa --source original --location
```

### status

**long_about:** Show a summary of a job log file. Displays total operations, success/failure/skip counts, and lists any failed files with error messages.

**Examples:**
```
# View job log summary
$ ia status --joblog downloads.jsonl
```

### completions

**long_about:** Generate shell completion scripts. Prints a completion script to stdout — redirect it to the appropriate file for your shell.

**Examples:**
```
# Generate completions for fish
$ ia completions fish > ~/.config/fish/completions/ia.fish

# Generate completions for bash
$ ia completions bash > ~/.local/share/bash-completion/completions/ia
```

## Option Description Improvements

Terse option help strings to improve:

- **`--source` / `--exclude-source`** (download, list): Add `(original, derivative, metadata)` to the help text
- **`--items`** (download): Clarify this controls concurrent items, vs `--jobs` for concurrent files
- **`--search`** (download, metadata): Mention it queries archive.org and processes each result
- **`--expect`** (metadata): Explain this is an optimistic concurrency check
- **`--target`** (metadata): List the two valid values explicitly (`"metadata"` or `"files/FILENAME"`)

## New Dependencies

- `color-print` — compile-time ANSI code insertion for colored example text in `after_long_help`

## Maintenance Convention

Add to CLAUDE.md development workflow:

> **ALWAYS update help text**: When adding or modifying CLI flags, subcommands, or behaviors, update the corresponding `about`, `long_about`, `after_long_help`, and option-level help strings. Help text is user-facing documentation — it must stay accurate.
