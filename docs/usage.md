# Usage

For a quick tour of highlights, see the [Showcase](./showcase.md).

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
ia download [IDENTIFIER] [FILES]... [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `[IDENTIFIER]` | Item identifier to download. Omit it in batch mode (`--search`, `--itemlist`, or identifiers piped on stdin) |
| `[FILES]...` | Specific files to download. In batch mode, applied to every item; `{identifier}` is substituted per item |
| `--itemlist <PATH>` | File containing identifiers (one per line) |
| `-s, --search <QUERY>` | Download items matching a search query |
| `--search-parameter <K=V>` | Extra search parameters (repeatable, used with `--search`) |
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
| `--count-views` | Increment archive.org's public view counter (off by default) |
| `--zip-list <ZIPFILE>` | List files inside a ZIP archive (e.g., `"item_jp2.zip"`) |
| `--zip-member <ZIPFILE/MEMBER>` | Download a single file from inside a ZIP archive |
| `--zip-convert <EXT>` | Convert format when downloading a zip member (e.g., `jpg` for JP2 → JPEG; requires `--zip-member`) |
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

# Download several items (one identifier per line on stdin)
printf 'item1\nitem2\nitem3\n' | ia download

# Batch download from a search query with 16 parallel downloads
ia download --search "collection:nasa AND mediatype:image" --jobs 16

# Batch download from a file of identifiers
ia download --itemlist items.txt

# Preview what would be downloaded
ia download nasa --dry-run

# List the contents of a ZIP archive without downloading it
ia download myitem --zip-list myitem_jp2.zip

# Extract a single page image from inside a ZIP, converting JP2 to JPEG
ia download myitem --zip-member "myitem_jp2.zip/myitem_jp2/myitem_0001.jp2" --zip-convert jpg

# Download with JSON output (for scripts/agents)
ia download nasa --json
```

> **Note:** All download requests (single-file, batch, zip listings, scandata,
> and AI-config fetches) include `cnt=0` as a query parameter so they do not
> increment the public view counter on archive.org. Pass `--count-views` on
> `ia download` to omit the parameter and have your downloads counted toward
> public view statistics — archive.org only records a view when `cnt` is
> absent entirely; `cnt=1` (or any other value) also suppresses counting.

### `ia search`

Search the Internet Archive. Three backends are available as subcommands; the bare command defaults to the scrape backend.

```sh
ia search <QUERY> [OPTIONS]            # scrape backend (default)
ia search scrape <QUERY> [OPTIONS]     # scrape API (cursor-based, auto-paginates)
ia search advanced <QUERY> [OPTIONS]   # advanced search API (single page)
ia search fts <QUERY> [OPTIONS]        # full-text search (scroll-based, auto-paginates)
```

#### Shared flags (all backends)

| Flag | Description |
|------|-------------|
| `<QUERY>` | Search query |
| `--itemlist` | Output identifiers only (one per line) |
| `-n, --num-found` | Print result count only |
| `-p, --parameters <K=V>` | Extra query parameters (`key:value` or `key=value`, repeatable) |
| `--timeout <SECS>` | Request timeout in seconds |
| `--json` | Output results as JSONL (one object per line) |

#### `scrape` and `advanced` flags

| Flag | Description |
|------|-------------|
| `-s, --sort <FIELD>` | Sort field (repeatable, e.g., `"downloads desc"`) |
| `-f, --field <FIELD>` | Fields to return (alias: `--fields`; repeatable, default: all) |
| `-r, --rows <N>` | Results per page (`advanced` only, default: 50) |

#### `fts` flags

| Flag | Description |
|------|-------------|
| `--dsl` | Treat the query as raw Elasticsearch DSL |
| `--scope <SCOPE>` | Index/scope filter |
| `--size <N>` | Results per scroll batch (default: 1000) |
| `--from <N>` | Starting offset |

#### Examples

```sh
# Search for items in a collection (scrape backend)
ia search "collection:nasa AND mediatype:texts"

# Get identifiers only (useful for piping to ia download)
ia search "subject:mars" --itemlist

# Get the number of matching items
ia search "collection:opensource" --num-found

# Single page of results via the advanced search API
ia search advanced "mediatype:texts" --rows 10

# Full-text search (searches file contents, not just metadata)
ia search fts "apollo 11 transcript"

# Full-text search with raw Elasticsearch DSL
ia search fts --dsl '{"match": {"text": "moon landing"}}'

# Return specific fields, sorted by downloads
ia search "mediatype:audio" --field identifier --field title --sort "downloads desc"

# Output as JSONL
ia search "collection:nasa" --json

# Pipe search results into download
ia search "collection:nasa" --itemlist | ia download
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
| `-V, --headers` | Print column headers |
| `--json` | Output results as JSON (one object per line) |

#### Examples

```sh
# List files in an item
ia list nasa

# Show specific columns with headers
ia list nasa --columns name,size,format --headers

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
| `-p, --parameters <K=V>` | Extra query parameters sent with each metadata request (`key:value` or `key=value`, repeatable; e.g. `-p dark_ok=1` to read dark items) |

```sh
# View metadata for an item
ia metadata nasa

# Pretty-print metadata
ia metadata nasa --pretty

# Check if an item exists
ia metadata nasa --exists

# List file formats in an item
ia metadata nasa --formats

# Read a dark item (requires auth + dark_ok=1)
ia metadata DARK-item -p dark_ok=1
```

#### Writing metadata

Metadata writes use subcommands. Each takes `-m/--metadata <FIELD:VALUE>` pairs (repeatable):

| Subcommand | Description |
|------------|-------------|
| `modify` | Set fields to new values (replaces existing values) |
| `append` | Append text to string fields |
| `append-list` | Append values to list fields (e.g., `subject`, `collection`) |
| `insert` | Insert at an index in list fields (`field[N]:value` syntax) |
| `remove` | Remove values from fields |

Passing `-m` on the bare command (`ia metadata <ID> -m <K:V>`) is shorthand for `ia metadata modify`.

Chain multiple operations with `+` to apply them in a single request. Valid operations after `+`: `modify`, `append`, `append-list`, `insert`, `remove`. Shared options (`--target`, `--dry-run`, `--json`, etc.) go before the first `+`.

| Write option | Description |
|------|-------------|
| `--target <TARGET>` | Target: `metadata` (default) or `files/FILENAME` |
| `--expect <K:V>` | Optimistic concurrency check (fail if field doesn't match) |
| `--priority <N>` | Task priority (default: 0 single, -5 batch) |
| `--reduced-priority` | Accept reduced priority to reduce rate limiting |
| `--dry-run` | Show changes without writing |

| Bulk input | Description |
|------|-------------|
| `--spreadsheet <PATH>` | Bulk update from file (CSV, TSV, XLSX, ODS, JSONL) — bare command only |
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | Use search results as input |
| `--search-parameter <K=V>` | Extra search parameters for `--search` (`key:value` or `key=value`, repeatable; e.g. `--search-parameter sorts='addeddate desc'`) |

```sh
# Set a metadata field (shorthand for 'ia metadata modify')
ia metadata myitem -m "description:Updated description"

# Set multiple fields at once
ia metadata modify myitem -m "title:New Title" -m "subject:science"

# Append to a string field
ia metadata append myitem -m "description: (updated 2026)"

# Add a value to a list field (e.g., add a subject tag)
ia metadata append-list myitem -m "subject:astronomy"

# Insert at a specific index in a list
ia metadata insert myitem -m "collection[0]:featured"

# Remove a value from a field
ia metadata remove myitem -m "subject:outdated-tag"

# Compound: set the title and remove a subject in one request
ia metadata modify myitem -m "title:New" + remove -m "subject:old-tag"

# Preview changes without writing
ia metadata modify myitem -m "title:New Title" --dry-run

# Modify file-level metadata
ia metadata modify myitem --target "files/image.jpg" -m "title:Photo caption"

# Bulk update from a spreadsheet
ia metadata --spreadsheet updates.csv

# Bulk modify items from a search query
ia metadata --search "collection:mybooks" -m "rights:public domain"

# Bulk modify items from a file of identifiers
ia metadata --itemlist items.txt -m "subject:archived"
```

#### `ia metadata export`

Bulk-export metadata for many items. Reads identifiers from files (CSV, TSV, XLSX, ODS, JSONL, or plain text with one ID per line), `--itemlist`, `--search`, or stdin. Outputs JSONL to stdout by default, or writes to a file with `-o` (format inferred from extension). In file mode, multi-value fields expand into indexed columns: `subject[0]`, `subject[1]`, etc.

| Flag | Description |
|------|-------------|
| `[FILES]...` | Input files containing identifiers |
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | Use search results as input |
| `--search-parameter <K=V>` | Extra search parameters for `--search` (`key:value` or `key=value`, repeatable; e.g. `--search-parameter sorts='addeddate desc'`) |
| `-p, --parameters <K=V>` | Extra query parameters sent with each metadata request (`key:value` or `key=value`, repeatable; e.g. `-p dark_ok=1` to export dark items) |
| `-o, --output <PATH>` | Output file (`.csv`, `.tsv`, `.xlsx`, `.jsonl`) |
| `--pretty` | Pretty-print JSON output |

```sh
# Export search results as JSONL
ia metadata export --search "collection:nasa"

# Sort search results (pass any scrape parameter via --search-parameter)
ia metadata export --search "collection:nasa" --search-parameter sorts="addeddate desc"

# Export dark items (requires auth + dark_ok=1 on each metadata request)
ia metadata export --itemlist dark-ids.txt -p dark_ok=1

# Export to XLSX for editing, then re-import
ia metadata export --search "collection:nasa" -o data.xlsx
ia metadata --spreadsheet data.xlsx --dry-run

# Pipe identifiers from another command
ia search "collection:nasa" -f identifier | ia metadata export
```

#### `ia metadata audit`

Audit item metadata against the live Internet Archive schema. Reports type mismatches, missing required fields, deprecated fields, and repeatability violations.

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>...` | Item identifier(s) |
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | Use search results as input |
| `--search-parameter <K=V>` | Extra search parameters for `--search` (`key:value` or `key=value`, repeatable; e.g. `--search-parameter sorts='addeddate desc'`) |
| `--field <FIELD>` | Only check specific field(s) (repeatable) |
| `--required-only` | Only report missing required fields |
| `-o, --output <PATH>` | Output file (`.csv`, `.tsv`, `.xlsx`, `.jsonl`) |
| `--json` | Output as JSONL |

```sh
# Audit a single item
ia metadata audit myitem

# Audit search results, machine-readable
ia metadata audit --search "collection:test" --json
```

#### `ia metadata schema`

Look up Internet Archive metadata field definitions. Shows a table of all user-facing fields by default, or detailed info for a specific field. The schema is fetched live from archive.org.

| Flag | Description |
|------|-------------|
| `[FIELD]` | Field name to look up (shows detailed view) |
| `-f, --files` | Show file-level schema instead of item-level |
| `--internal` | Include internal-use-only fields (hidden by default) |
| `--required` | Only show required or recommended fields |
| `--repeatable` | Only show repeatable fields |
| `--defined-by <WHO>` | Filter by who defines the field |
| `--edit-access <WHO>` | Filter by who can edit the field |
| `--json` | Output as JSON |

```sh
# List all user-facing metadata fields
ia metadata schema

# Look up a specific field
ia metadata schema title

# Show the file-level schema
ia metadata schema --files
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
| `--failed-items` | Print only identifiers that never succeeded (one per line); exits 1 if any |
| `--json` | Output as JSON |

#### Examples

```sh
# View job log summary
ia status --joblog downloads.jsonl

# List identifiers that never succeeded
ia status --joblog uploads.jsonl --failed-items

# Pipe failed identifiers into another command
ia status --joblog uploads.jsonl --failed-items | ia metadata export
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

### `ia man`

Generate roff man pages from the command tree. One page per command, named the
way `git` and `cargo` name theirs — so `man ia-cli-metadata-modify` works.

Pre-built pages are attached to each [release](https://github.com/internetarchivecanada/ia/releases)
as `ia-cli-man.tar.gz`; this command regenerates them from the binary you have.

```sh
ia man [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `--out-dir <DIR>` | Write one page per command into this directory (created if absent). Without it, the top-level page is printed to stdout |
| `--rename <NAME>` | Override the command name used in generated pages |

Page names are hyphenated (`ia-cli-metadata-modify.1`) while the SYNOPSIS shows
the real invocation (`ia-cli metadata modify`), matching `git-rebase(1)`.

#### Examples

```sh
# Write the full page tree to a directory
ia man --out-dir man/

# Install system-wide
sudo ia man --out-dir /usr/local/share/man/man1/

# Print just the top-level page
ia man > ia-cli.1
```

### `ia upload`

Upload files to the Internet Archive. Supports single-file, multi-file, directory, and stdin uploads. Batch uploads from spreadsheets and template generation are available as subcommands.

```sh
ia upload <IDENTIFIER> <FILES>... [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Item identifier |
| `<FILES>...` | Files or directories to upload |
| `-m, --metadata <K:V>` | Set metadata field (repeatable) |
| `--header <K:V>` | Additional HTTP header (repeatable) |
| `--remote-name <NAME>` | Explicit remote filename (required for stdin) |
| `--remote-dir <PATH>` | Prepend path prefix to remote filenames |
| `--keep-directories` | Preserve relative path structure |
| `--clobber` | Force re-upload even when remote file has matching MD5 |
| `--checksums <PATH>` | Path to pre-computed MD5 checksums file |
| `--delete-after-upload` | Delete local file after verified upload |
| `--no-verify` | Skip Content-MD5 verification |
| `--no-derive` | Skip derivative generation |
| `--no-backup` | Don't keep old file versions |
| `--no-auto-make-bucket` | Error if item doesn't already exist |
| `--no-collection-check` | Skip collection existence check |
| `--no-size-hint` | Don't send x-archive-size-hint header |
| `--test-item` | Upload to test_collection (auto-removed after 30 days) |
| `--open-after-upload` | Open item in browser after upload |
| `--multipart` | Use multipart upload (recommended for files >5 GB) |
| `--retries <N>` | Retry attempts per file (default: 10) |
| `--retry-sleep <SECS>` | Sleep between retries in seconds (default: 30) |
| `--dry-run` | Validate everything, upload nothing |
| `--dashboard` | Full-screen TUI dashboard |
| `--json` | Output results as JSONL |

#### Batch mode and subcommands

**`ia upload --spreadsheet <FILE>`** — Batch upload from a spreadsheet file (CSV/TSV/XLSX/ODS/JSONL). Each row specifies an identifier, file path, and optional metadata. Rows sharing the same identifier are grouped into a single item upload.

Required columns: `identifier`, `file`. All other columns become metadata.

Supports the same options as the bare command: `-m`, `--header`, `--checksums`, `--no-derive`, `--no-backup`, `--no-auto-make-bucket`, `--no-verify`, `--no-size-hint`, `--no-collection-check`, `--clobber`, `--delete-after-upload`, `--test-item`, `--multipart`, `--retries`, `--retry-sleep`, `--dry-run`, `--json`.

**`ia upload template <DIR>`** — Generate a template spreadsheet from a local directory, pre-filled with file paths. Edit the template to add metadata, then feed it to `ia upload --spreadsheet`.

| Flag | Description |
|------|-------------|
| `<DIR>` | Directory to scan for files |
| `-o, --output <PATH>` | Output file (default: stdout) |
| `--format <FORMAT>` | Output format: `csv` (default), `tsv`, `xlsx` |
| `--identifier-prefix <PREFIX>` | Prefix to prepend to generated identifiers |
| `--identifier-from-filename` | Generate identifiers from filenames |
| `--identifier-from-dirname` | Generate identifiers from parent directory names |
| `--json` | Output template as JSONL |

**`ia upload cleanup <IDENTIFIER> [FILE]`** — List or abort incomplete multipart uploads for an item.

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Item identifier |
| `[FILE]` | Specific file to clean up |
| `--abort-all` | Abort all incomplete uploads without confirmation |
| `--json` | Output as JSON |

#### Examples

```sh
# Upload a file to an existing or new item
ia upload my-item file.pdf -m mediatype:texts -m collection:opensource

# Upload a directory, preserving structure
ia upload my-item ./scans/ --keep-directories

# Upload from stdin with an explicit remote name
cat data.csv | ia upload my-item - --remote-name data.csv

# Multipart upload for large files
ia upload my-item big-video.mp4 --multipart

# Force re-upload even if remote files match
ia upload my-item ./files/ --clobber

# Batch upload from a spreadsheet
ia upload --spreadsheet batch.csv

# Generate a template, edit it, then batch upload
ia upload template ./files/ -o template.csv
# ... edit template.csv to add metadata columns ...
ia upload --spreadsheet template.csv

# Abort stale multipart uploads
ia upload cleanup my-item

# Dry run — validate without uploading
ia upload my-item file.pdf --dry-run

# Upload with dashboard
ia upload my-item ./files/ --dashboard
```

#### Resuming Uploads

When `--joblog` is provided, uploads automatically resume from where they left off. Files that were successfully uploaded in a previous run (recorded in the joblog) are skipped, so you can safely re-run the same command after an interruption.

```sh
# First run — uploads all files, logs results
ia upload --spreadsheet batch.csv --joblog upload.jsonl

# Interrupted! Re-run the same command — completed files are skipped
ia upload --spreadsheet batch.csv --joblog upload.jsonl

# Force re-upload everything (ignore previous successes)
ia upload --spreadsheet batch.csv --joblog upload.jsonl --no-resume

# Single-item resume works the same way
ia upload my-item ./files/ --joblog upload.jsonl
```

The resume mechanism reads the joblog at startup and builds a set of `(identifier, filename)` pairs that completed successfully. Any file matching a pair in the set is skipped with a `Resumed` status. The `--no-resume` flag disables this behavior, forcing all files to be re-uploaded.

Use `ia status --joblog upload.jsonl` to see a summary of completed, failed, and skipped files.

### `ia verify`

Verify that local files exist on archive.org with matching checksums. Exits non-zero if any file can't be verified. Alias: `ia ve`.

```bash
# Verify specific files
ia verify my-item file1.pdf file2.pdf

# Verify a directory
ia verify my-item ./local-files/

# Verify using pre-computed checksums (no local files needed)
ia verify my-item --checksum-file md5sums.txt

# Use SHA-1 instead of MD5
ia verify my-item ./files/ --checksum-type sha1

# Require exact filename match (default: hash-only)
ia verify my-item ./files/ --match-names

# Filter remote files to match against
ia verify my-item ./files/ --glob '*.pdf'
ia verify my-item ./files/ --source original

# Batch verify from upload spreadsheet
ia verify --spreadsheet upload.csv

# Gate a script on verification
ia verify my-item ./files/ -q && ./post-upload.sh
```

#### Verification modes

**Hash-only (default):** For each local file, computes its hash and searches for *any* remote file with a matching hash. Reports the matched remote filename if it differs from the local name. This mode answers "is my content on archive.org?"

**Match-names (`--match-names`):** Finds the remote file by name, then compares hashes. Reports `mismatch` if the name matches but hashes differ, `missing` if no remote file has that name. Use this when filename accuracy matters.

#### Hash algorithms

Supports `md5` (default), `sha1`, and `crc32` via `--checksum-type`. When using `--checksum-file`, the algorithm is auto-detected from hash length (32 chars = MD5, 40 chars = SHA-1). CRC32 in GNU format requires explicit `--checksum-type crc32` due to length ambiguity.

Checksum files support both GNU (`hash  filename`) and BSD (`ALG (filename) = hash`) formats.

#### Spreadsheet mode

`--spreadsheet` accepts the same CSV/TSV/XLSX/ODS/JSONL format as `ia upload --spreadsheet`. Requires `identifier` and `file` columns. Optional hash columns (`md5`, `sha1`, `crc32`) skip local file hashing. Other columns are ignored.

```bash
# Same spreadsheet used for upload works for verification
ia upload --spreadsheet upload.csv
ia verify --spreadsheet upload.csv
```

#### Output modes

Console (default) shows per-file status with icons, JSON (`--json`) outputs JSONL, and quiet (`-q`) suppresses output for scripting. Exit code is always 0 (all verified) or 1 (any failure).

### `ia tasks`

Manage Internet Archive catalog tasks: list, submit, view logs, rerun failed tasks, and check rate limits. Alias: `ia ta`.

```sh
ia tasks [IDENTIFIER] [OPTIONS]
```

#### Listing tasks (bare command)

By default, `ia tasks` shows your queued/running tasks. When given an identifier, it shows both active and completed tasks for that item.

| Flag | Description |
|------|-------------|
| `[IDENTIFIER]` | Item identifier (shows catalog + history for that item) |
| `--cmd <CMD>` | Filter by task command (e.g. `derive.php`) |
| `--submitter <EMAIL>` | Filter by submitter email |
| `--server <SERVER>` | Filter by server name |
| `--priority <N>` | Filter by priority |
| `--args <ARGS>` | Filter by args (supports wildcards `*`/`%`) |
| `--color <COLOR>` | Filter by status color: `green` (queued), `blue` (running), `red` (error), `brown` (paused) |
| `--task-id <ID>` | Filter by specific task ID |
| `--since <DATE>` | Show tasks submitted after this date/time |
| `--before <DATE>` | Show tasks submitted before this date/time |
| `--limit <N>` | Cap number of results returned |
| `--active-only` | Only show active tasks (mutually exclusive with `--completed-only`) |
| `--completed-only` | Only show completed tasks (requires identifier or task_id) |
| `--no-summary` | Hide the summary counts header |
| `-p, --parameter <K=V>` | Raw API parameter (repeatable) |
| `--json` | Output as JSONL |

```sh
# List your pending tasks
ia tasks

# List tasks for an item (active + completed)
ia tasks my-item

# Filter by command
ia tasks --cmd derive.php

# Show only running tasks
ia tasks --color blue

# Show only completed tasks for an item
ia tasks my-item --completed-only

# Show a specific task
ia tasks --task-id 101247325

# Filter by date range
ia tasks --since "2026-03-01" --before "2026-03-10"

# JSON output for piping
ia tasks --json
```

#### `ia tasks submit`

Submit a new task to the Tasks API.

```sh
ia tasks submit [IDENTIFIER] --cmd <CMD> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `[IDENTIFIER]` | Item identifier (omit for batch mode) |
| `--cmd <CMD>` | Task command (e.g. `derive`, auto-appends `.php` if needed). Required unless `--spreadsheet` is used. |
| `--args <K=V>` | Task arguments (repeatable) |
| `--comment <TEXT>` | Explanation for why the task is being submitted |
| `--priority <N>` | Task priority (-10 to 10, default: 0) |
| `--reduced-priority` | Submit at reduced priority to avoid rate-limiting |
| `--wait` | Poll until task completes |
| `--wait-interval <SECS>` | Initial poll interval in seconds (default: 2, exponential backoff) |
| `--max-retries <N>` | Max retries on 429 rate-limit responses (default: 10) |
| `--itemlist <PATH>` | Batch mode: file with one identifier per line |
| `--search <QUERY>` | Batch mode: submit task to all matching items |
| `--search-parameter <K=V>` | Extra search parameters for `--search` (`key:value` or `key=value`, repeatable; e.g. `--search-parameter sorts='addeddate desc'`). Note: `-p` is a raw *task* parameter, not a search parameter. |
| `--spreadsheet <PATH>` | Batch mode: submit tasks from a spreadsheet. Required columns: `identifier`, `cmd`. Optional: `comment`, `priority`. Task arguments use `args.` prefix (e.g. `args.remove_derived`). |
| `-p, --parameter <K=V>` | Raw API parameter (repeatable) |
| `--dry-run` | Print what would be submitted without sending |
| `--json` | Output as JSON |

```sh
# Submit a derive task
ia tasks submit my-item --cmd derive

# Submit with a comment
ia tasks submit my-item --cmd make_dark --comment "curation request"

# Submit to multiple items from a file
ia tasks submit --cmd derive --itemlist items.txt --comment "re-derive"

# Submit to items from a search query
ia tasks submit --cmd derive --search "collection:nasa" --comment "re-derive all"

# Submit with custom args
ia tasks submit my-item --cmd derive --args remove_derived="*.jpg"

# Submit and wait for completion
ia tasks submit my-item --cmd derive --wait

# Batch submit from a spreadsheet
ia tasks submit --spreadsheet jobs.csv
```

#### `ia tasks log`

Fetch and display the execution log for a task.

```sh
ia tasks log <TASK_ID> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `<TASK_ID>` | Task ID (integer) |
| `--json` | Output as JSON |

```sh
# View a task log
ia tasks log 1234567

# Save a task log to a file
ia tasks log 1234567 > task.log

# Output as JSON
ia tasks log 1234567 --json
```

#### `ia tasks rerun`

Rerun failed tasks. Accepts task IDs as arguments, from stdin, or via query filters.

```sh
ia tasks rerun <TASK_ID>... [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `<TASK_ID>...` | Task IDs to rerun (use `-` for stdin) |
| `--cmd <CMD>` | Rerun all failed tasks matching this command |
| `--color <COLOR>` | Filter by status color (default: `red` when using query filters) |
| `--submitter <EMAIL>` | Filter by submitter |
| `--identifier <ID>` | Filter by identifier |
| `--max-retries <N>` | Max retries per rerun request on failure |
| `--json` | Output as JSON |

```sh
# Rerun a single failed task
ia tasks rerun 1234567

# Rerun multiple tasks
ia tasks rerun 1234567 1234568 1234569

# Rerun all failed derive tasks
ia tasks rerun --cmd derive.php

# Rerun from a pipeline
ia tasks --cmd derive.php --color red --json | ia tasks rerun
```

#### `ia tasks rate-limit`

Check task submission rate limits.

```sh
ia tasks rate-limit [CMD] [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `[CMD]` | Task command to check (default: `derive.php`, auto-appends `.php`) |
| `--json` | Output as JSON |

```sh
# Check derive rate limits (default)
ia tasks rate-limit

# Check a specific command
ia tasks rate-limit make_dark

# JSON output
ia tasks rate-limit --json
```

### `ia collection`

Manage Internet Archive collections.

#### `ia collection create`

Create a new collection item on Internet Archive via S3. Only the identifier and parent collection are required. Title, description, and subject are recommended but optional.

```sh
ia collection create <IDENTIFIER> --collection <COLL> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Collection identifier (3-100 chars, alphanumeric + `._-@`) |
| `-C, --collection <COLL>` | Parent collection identifier (required) |
| `-t, --title <TITLE>` | Collection title |
| `-D, --description <DESC>` | Collection description |
| `-s, --subject <SUBJ>` | Subject/topic |
| `-I, --image <PATH>` | Path to collection cover image |
| `-m, --metadata <K:V>` | Additional metadata (repeatable) |
| `--dry-run` | Validate and show full details without creating the collection |
| `--json` | Output as JSON |

```sh
# Minimal collection (identifier + parent only)
ia collection create my-collection --collection opensource

# Recommended: include title, description, subject
ia collection create my-collection \
    --title "My Collection" \
    -D "A collection of things" \
    --subject "things" \
    --collection opensource

# With an image and extra metadata
ia collection create my-collection \
    --title "My Collection" \
    -D "A collection of things" \
    --subject "things" \
    --collection opensource \
    --image logo.png -m hidden:true

# Dry run (shows full metadata details)
ia collection create my-collection \
    --collection opensource --dry-run
```

### `ia config`

Configure Internet Archive credentials and settings.

```sh
ia config <SUBCOMMAND>
```

#### Subcommands

| Subcommand | Description |
|------------|-------------|
| `login` | Log in to archive.org and save credentials |
| `show` | Print current configuration as JSON |
| `check` | Validate stored S3 credentials |
| `whoami` | Show account information (screenname, email) |
| `print-cookies` | Print cookies in Netscape format |
| `print-auth` | Print the Authorization header for S3 API requests |

#### Flags

| Flag | Subcommand | Description |
|------|------------|-------------|
| `-u, --username <EMAIL>` | `login` | Email address (prompts if omitted) |
| `-p, --password <PASS>` | `login` | Password (prompts if omitted) |
| `--netrc` | `login` | Read credentials from `~/.netrc` |
| `--show-secrets` | `show` | Show secret values instead of redacting them |
| `--json` | all | Output as JSON |

#### Examples

```sh
# Interactive login (prompts for email and password)
ia config login

# Non-interactive login
ia config login -u user@example.com -p mypassword

# Login using .netrc credentials
ia config login --netrc

# Show current config (secrets redacted)
ia config show

# Show config with all secret values visible
ia config show --show-secrets

# Check if stored credentials are valid
ia config check

# Show account info
ia config whoami

# Save cookies for use with curl
ia config print-cookies > cookies.txt
curl -b cookies.txt https://archive.org/...

# Use auth header with curl
curl -H "$(ia config print-auth)" https://s3.us.archive.org/...
```

### `ia update`

Check for updates, list available versions, or install a specific version. This command is only available in standalone release builds (feature-gated behind `self-update`). If you installed via `cargo install`, use cargo to update instead.

```sh
ia update [OPTIONS]
ia update list [OPTIONS]
ia update install <VERSION> [OPTIONS]
```

#### Subcommands

| Subcommand | Description |
|------------|-------------|
| *(bare)* | Check for updates and install the latest version |
| `list` | List available versions from GitHub Releases |
| `install <VERSION>` | Install a specific version |

#### Flags

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--check` | *(bare)* | Only check for updates, don't install |
| `--all` | `list` | Show all versions (not just the 5 most recent) |
| `--json` | all | Output results as JSON |

#### Examples

```sh
# Update to the latest version
ia update

# Check for updates without installing
ia update --check

# Machine-readable check
ia update --check --json

# List available versions
ia update list

# List all versions
ia update list --all

# Install a specific version
ia update install 0.5.1
```

### `ia ai`

AI tooling for Internet Archive metadata. **Experimental** — only available in builds with the `alpha` feature.

```sh
ia ai qa <IDENTIFIER>... [OPTIONS]
ia ai config <show|create|edit> <COLLECTION> [OPTIONS]
```

| Global flag | Description |
|------|-------------|
| `--ai-config <PATH>` | Path to a local AI Config JSON file (overrides collection lookup) |

#### `ia ai qa`

Verify AI-extracted metadata using vision-based LLM QA. Fetches AI-extracted metadata for items, downloads page images from the item's JP2 zip, and sends both to a second LLM model for verification. Produces per-field verdicts with confidence scores. With `--promote`, writes confirmed metadata back to the item.

Input sources:

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>...` | Item identifier(s) to QA |
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | QA items matching a search query |
| `--search-parameter <K=V>` | Extra search parameters (repeatable) |
| `--from-results <PATH>` | Re-process cached QA results from a JSONL file (no LLM calls) |

LLM configuration:

| Flag | Description |
|------|-------------|
| `--model <NAME>` | QA LLM model |
| `--base-url <URL>` | LLM API base URL |
| `--api-key <KEY>` | LLM API key |
| `--provider <NAME>` | `openai` or `anthropic` (auto-detected from base URL if omitted) |
| `--temperature <FLOAT>` | Sampling temperature (default: 0.2) |
| `--image-quality <LEVEL>` | Page image resolution: `high`, `medium` (default), `low`, `min` — lower is cheaper |
| `--image-urls` | Send image URLs to the LLM instead of downloading and base64-encoding |

Output and promotion:

| Flag | Description |
|------|-------------|
| `-o, --output <PATH>` | Write results to file(s); format from extension (`.xlsx`, `.jsonl`, `.csv`, `.tsv`); repeatable |
| `--json` | Output results as JSONL to stdout |
| `--promote` | Write confirmed metadata to items after QA |
| `--confidence <FLOAT>` | Min overall confidence for promotion (default: 0.8) |
| `--min-field-confidence <FLOAT>` | Min per-field confidence (default: 0.6) |
| `--dry-run` | Show what would be done without making changes |
| `--estimate` | Estimate cost without processing items |
| `--print-prompt` | Print the prompt that would be sent to the LLM and exit |
| `--dashboard` | Interactive TUI for reviewing QA results |

```sh
# QA a single item
ia ai qa my-item

# QA items from a search, output JSONL
ia ai qa --json --search "collection:theses"

# Save results to XLSX and JSONL in one run
ia ai qa --search "collection:theses" -o results.xlsx -o results.jsonl

# Re-process cached results into XLSX (no LLM calls)
ia ai qa --from-results results.jsonl -o results.xlsx

# QA and promote confirmed metadata
ia ai qa --promote --confidence 0.9 item1 item2

# Dry run — show what would be promoted
ia ai qa --promote --dry-run my-item

# Use the Anthropic API directly
ia ai qa --base-url https://api.anthropic.com --model claude-sonnet-4-6 item1

# Local model (no API key needed)
ia ai qa --base-url http://localhost:11434/v1 --model llava:34b item1
```

#### `ia ai config`

Read, create, and edit AI Config JSON files stored in collection items. These configs define the LLM model, prompt, page selection, and response schema used by the AI Metadata Extractor derive module.

| Subcommand | Description |
|------------|-------------|
| `show <COLLECTION>` | Display the collection's AI config (`--json`) |
| `create <COLLECTION>` | Create a new AI config (`--from-file`, `--model`, `--prompt`, `--prompt-file`, `--pages`, `--schema-file`, `--dry-run`, `--json`) |
| `edit <COLLECTION>` | Edit an existing config (`--editor` opens `$EDITOR`; or `--set-model`, `--set-prompt`, `--set-prompt-file`, `--set-pages`) |

```sh
# Show a collection's AI config
ia ai config show theses-and-dissertations

# Create with defaults
ia ai config create my-collection

# Create from a file
ia ai config create my-collection --from-file config.json

# Edit the config in $EDITOR
ia ai config edit my-collection --editor
```

#### `ia ai analyze` / `ia ai undo`

Builds compiled with the `ai-analyze` feature (not part of the standard `alpha` build) also include:

- **`ia ai analyze <IDENTIFIER>...`** — LLM-suggested metadata improvements (typos, dates, missing fields, schema conformance) with interactive review, `--headless` batch mode, and focus flags like `--dates-only` and `--only-fields`.
- **`ia ai undo <JOBLOG>`** — Reverse metadata changes recorded in a previous session's joblog. Supports `--dry-run` and `--json`.

## Global options

These options can be used with any subcommand:

| Flag | Description |
|------|-------------|
| `-c, --config-file <PATH>` | Path to configuration file |
| `-j, --jobs <N>` | Concurrent operations (omit for adaptive concurrency; commands without it use 8) |
| `-i, --insecure` | Allow insecure (HTTP) connections |
| `-H, --host <HOST>` | Override the archive.org host |
| `--user-agent-suffix <STRING>` | Append to the default User-Agent |
| `--joblog <PATH>` | Write operation results to a JSONL log file (enables auto-resume) |
| `--no-resume` | Don't resume from joblog — process all items fresh |
| `-q, --quiet` | Suppress output (repeat for more quiet: `-q` summary only, `-qq` silent) |
| `-l, --log` | Enable logging |
| `-v, --verbose` | Increase output verbosity (`-v` info, `-vv` debug, `-vvv` trace) |

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

Track operations with `--joblog`. The log is a JSONL file (one JSON object per line) recording the outcome of each file operation. When `--joblog` is provided, auto-resume is enabled — re-running the same command automatically skips already-completed items.

```sh
# Download with job logging
ia download nasa --joblog downloads.jsonl

# View job log summary
ia status --joblog downloads.jsonl

# Re-run to retry failures (auto-resume skips completed items)
ia download nasa --joblog downloads.jsonl
```

### Disk pool

Distribute downloads across multiple disks by passing `--destdir` multiple times:

```sh
ia download --search "collection:nasa" --destdir /mnt/disk1 --destdir /mnt/disk2
```

Items are assigned to the disk with the most free space. If a disk fills up, downloads automatically fail over to the next available disk.

### Dashboard mode

A full-screen terminal dashboard for monitoring batch operations, built with ratatui (ships by default):

```sh
ia download --search "collection:nasa" --dashboard
ia upload my-item ./files/ --dashboard
```

The dashboard shows panels for items, workers, errors, and throughput. Press `q` to quit.

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
- **stderr** emits structured error JSON, `{"error": {"code": "...", "message": "..."}}`, on some commands; others still print plain error text. Rely on the exit code until this is unified.
- Progress bars, color, and decorative output are suppressed
- Exit codes are binary: `0` for success, `1` for failure (details in stderr JSON)

Supported on all commands except `ia completions` (which outputs shell scripts, not data).

## Architecture

The project is a Cargo workspace with two crates:

- **ia-core** -- Library crate with the client, API types, download engine, search backends, and utilities. Designed as a standalone library for external consumers.
- **ia-cli** -- Binary crate with the CLI interface, progress display, and TUI dashboard

A desktop GUI is developed separately (not yet public) and consumes `ia-core` as a library dependency.

See [the design doc](plans/2026-02-20-ia-rust-port-design.md) for full architectural details.
