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
ia download <IDENTIFIER>... [FILES]... [OPTIONS]
```

#### Flags

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>...` | Item identifier(s) to download |
| `[FILES]...` | Specific files to download from the item |
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

#### Subcommands

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

`--spreadsheet` accepts the same CSV/TSV/XLSX/ODS/JSONL format as `ia upload import`. Requires `identifier` and `file` columns. Optional hash columns (`md5`, `sha1`, `crc32`) skip local file hashing. Other columns are ignored.

```bash
# Same spreadsheet used for upload works for verification
ia upload import upload.csv
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

AI-assisted metadata cleanup using an LLM. Analyzes item metadata, suggests improvements (typo fixes, date normalization, missing fields, schema conformance), and optionally applies changes. **Experimental.**

```sh
ia ai <IDENTIFIER>... [OPTIONS]
ia ai undo <JOBLOG>
```

#### Modes

| Flag | Description |
|------|-------------|
| *(default)* | Interactive TUI review — approve/reject each suggestion |
| `--headless` | Auto-accept all suggestions, output JSONL (no TUI) |
| `--record-only` | TUI review, save to local JSON instead of writing to IA |
| `--dry-run` | Show suggestions without applying any changes |

#### Input sources

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>...` | Item identifier(s) to analyze |
| `--itemlist <PATH>` | Read identifiers from file (one per line) |
| `--search <QUERY>` | Use search results as input |

#### Focus flags

| Flag | Description |
|------|-------------|
| `--dates-only` | Only suggest date-related changes |
| `--titles-only` | Only suggest title changes |
| `--descriptions-only` | Only suggest description changes |
| `--missing-fields` | Only fill empty/missing fields |
| `--schema-fix` | Only fix schema conformance issues |
| `--typos` | Only fix typos |
| `--only-fields <FIELDS>` | Only suggest changes to these fields (comma-separated) |
| `--exclude-fields <FIELDS>` | Never suggest changes to these fields (comma-separated) |

#### LLM configuration

| Flag | Description |
|------|-------------|
| `--base-url <URL>` | LLM API base URL (default: OpenAI) |
| `--api-key <KEY>` | LLM API key (or set `IA_AI_API_KEY` env var, or `[ai] api_key` in ia.ini) |
| `--model <NAME>` | Model name (default: gpt-4o-mini) |
| `--temperature <FLOAT>` | Sampling temperature (default: 0.2) |
| `--max-tokens <N>` | Max tokens in response (default: 4096) |

#### Other flags

| Flag | Description |
|------|-------------|
| `--ai-jobs <N>` | Concurrent LLM requests (default: 1) |
| `--prefetch <N>` | Items to prefetch ahead (default: 5) |
| `--max-tokens-budget <N>` | Stop after this many total tokens |
| `-o, --output <PATH>` | Write accepted changes to JSON file (record-only mode) |
| `--json` | Output results as JSON/JSONL |

#### Subcommands

**`ia ai undo <JOBLOG>`** — Reverse metadata changes from a previous AI session. Reads the joblog, finds successful changes, and applies reverse operations. Supports `--dry-run` and `--json`.

#### Examples

```sh
# Interactive review of one item
ia ai nasa_photo_apollo11

# Headless batch processing
ia ai --headless --search "collection:nasa"

# Dry run — show suggestions without applying
ia ai --dry-run nasa

# Focus on date fixes only
ia ai --dates-only --itemlist items.txt

# Undo changes from a previous session
ia ai undo session.jsonl

# Preview what would be undone
ia ai undo session.jsonl --dry-run
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
| `--joblog <PATH>` | Write operation results to a JSONL log file (enables auto-resume for uploads) |
| `--retry-failed` | Retry failed operations from a job log |
| `--no-resume` | Don't resume from joblog — upload all files fresh |
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
- **stderr** emits structured error JSON: `{"error": {"code": "...", "message": "..."}}`
- Progress bars, color, and decorative output are suppressed
- Exit codes are binary: `0` for success, `1` for failure (details in stderr JSON)

Supported on all commands.

## Architecture

The project is a Cargo workspace with two crates:

- **ia-core** -- Library crate with the client, API types, download engine, search backends, and utilities. Designed as a standalone library for external consumers.
- **ia-cli** -- Binary crate with the CLI interface, progress display, and TUI dashboard

A desktop GUI ([ia-gui](https://github.com/jjjake/ia-gui)) is developed separately and consumes `ia-core` as a library dependency.

See [the design doc](plans/2026-02-20-ia-rust-port-design.md) for full architectural details.
