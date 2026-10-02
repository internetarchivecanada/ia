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
| `-C, --checksum` | Verify md5 checksums (slower, reads every local file); a mismatch keeps the bytes as `<name>.md5-mismatch` |
| `-R, --retries <N>` | Max retries per file, and the number of stalls allowed (default: 5). Waits are random, up to a cap that doubles from 1 s to 60 s; a `Retry-After` header sets the wait instead, as given |
| `--min-speed <RATE>` | Abandon and resume a stream averaging below RATE over the last 60 s, after a 30 s grace (default: `10K`; `0` disables) |
| `--no-timestamps` | Don't set file modification times |
| `--dry-run` | Show what would be downloaded without downloading |
| `--count-views` | Increment archive.org's public view counter (off by default) |
| `--zip-list <ZIPFILE>` | List files inside a ZIP archive (e.g., `"item_jp2.zip"`) |
| `--zip-member <ZIPFILE/MEMBER>` | Download a single file from inside a ZIP archive |
| `--zip-convert <EXT>` | Convert format when downloading a zip member (e.g., `jpg` for JP2 → JPEG; requires `--zip-member`) |
| `--dashboard` | Full-screen dashboard mode |
| `--json` | Output results as JSONL (one object per line) |

#### After an interruption

Killing `ia download`, or losing the connection, leaves each unfinished file as `<name>.part`. The rerun continues from the bytes already on disk with a `Range` request; nothing is fetched twice. The finished file is renamed into place only when its size matches the item's metadata, and with `--checksum` its md5 too. `--joblog` adds a second layer on top: an item whose files a previous run all finished is skipped without a request (an item with any failed file is entered again in full, and its finished files are then skipped by the local size and mtime check). A `.part` that is a symlink is removed and that file starts over; nothing is ever written through a link. The sections below give the rules in detail.

#### Retries

A file whose attempt fails with a retryable error (a dropped connection, a `429`, a `5xx`, a size mismatch that left a resumable `.part`, a checksum mismatch) is tried again up to `--retries` times (default 5). The wait before each retry is random, up to a cap that doubles from 1 s to 60 s (full jitter, so many clients retrying at once do not land together). When the failed response carried a `Retry-After` header, that wait is used instead, as given: the seconds form or the HTTP-date form, even above 60 s, and `Retry-After: 0` means try again at once. In `--json` output an `http_error` that carried the header shows it as `retry_after`.

```bash
# Ride out a flaky link: 20 retries per file
ia download nasa --retries 20
```

#### Partial files and the size check

Each file streams to `<name>.part` and is renamed into place only when the number of bytes received equals the `size` in the item's metadata. Otherwise the file is reported as failed with a message of the form `download size mismatch for <name>: expected N bytes, received M bytes` (in `--json` output the error code is `download_failed` and this text is the message). When the body came up short, the `.part` file is kept and the next attempt, whether the built-in retry or a rerun of the command, sends a `Range` request and resumes from the bytes already on disk. When the body ran long, the `.part` file is longer than the file and cannot be resumed, so it is deleted and the file fails permanently with `server reports N bytes for <name> but item metadata says M bytes`. A body more than 10% (at least 1 KB) over the metadata size is abandoned mid-stream instead, with `download too large` and its `.part` deleted.

If the server answers a `Range` request with a `Content-Range` total that differs from the metadata size, nothing from that response is written and the file fails with `server reports N bytes for <name> but item metadata says M bytes`. Retrying cannot fix that, so the command moves on to the next file.

If the server answers a `Range` request with `416 Range Not Satisfiable`, the `.part` file is already as long as the server's copy of the file or longer, and resuming it can never succeed. The 416's `Content-Range: bytes */N` gives the server's length. When that differs from the metadata size, the file fails with the same `server reports N bytes ... but item metadata says M bytes` message and the `.part` file is left alone. When the two agree and the `.part` file is exactly that long, it already holds the whole file: nothing more is downloaded, the md5 is compared when `--checksum` is on, and the `.part` file is renamed into place. When the two agree but the `.part` file is longer, it is deleted and the file is reported as `download size mismatch`, which is retried from the beginning.

Files with no `size` in metadata are not checked, nor is `<identifier>_files.xml`, which records its own size before it is final. The one exception is a 416 on a resume: with no metadata size to compare against, the server's length is taken as the file's, so the `.part` file is removed and the download restarts, even when the `.part` file is already that long.

#### Slow and stalled downloads

A connection that drops is resumed: the bytes already in the `.part` file stay, and the file is re-requested with a `Range` header from that offset. A connection that keeps sending bytes too slowly gets the same treatment. Once a stream is 30 s old, `ia` compares its average rate over the last 60 s (or over the stream's whole life while it is younger than that) with the `--min-speed` floor, once a second, whether or not any bytes are arriving. Below the floor, the stream is abandoned, the `.part` file is flushed, and the file is re-requested with `Range` from the bytes on disk. The new stream gets its own 30 s grace. No byte is lost or fetched twice, and the md5 comparison made with `--checksum` still covers the whole file.

The default floor is `10K`, 10 KiB/s. `RATE` is bytes per second: a plain number, or a number followed by `K`, `M`, or `G` for powers of 1024 (`10K` is 10240, `1M` is 1048576). `--min-speed 0` turns the check off; then only the transport's 60 s read timeout, which resets on every chunk, can end a silent stream, and a stream that trickles never ends.

Each stall spends one of the file's `--retries` (default 5). When they are gone, the file fails with `download of <name> stalled N times: X B/s over the last 60 s is below the --min-speed floor of Y B/s`, where N counts every stall and so is one more than `--retries`; the `.part` file is kept for a later run, and the file is not attempted again in this one (in `--json` output the error code is `download_failed` and this text is the message, as for every per-file failure). Dropped connections have their own budget of three re-requests per attempt, after short fixed waits (0.5 s, 1.5 s, 4.5 s), and do not count against the stalls. `--retries 0` means the first stall fails the file, with `stalled 1 time`.

#### Checksum mismatches

With `--checksum`, a local file whose md5 matches the item metadata is skipped, and a downloaded file's md5 is computed from the stream and compared when the stream ends. On a mismatch the bytes are kept beside the file as `<name>.md5-mismatch` and the error names that path: `checksum mismatch for <name>: expected X, got Y; kept the download at <path>`. Nothing is left as `.part`, so the file is downloaded again from byte 0. Keeping the copy is what makes the next step possible: comparing it against a second download, or against the source, tells a corrupt transfer from a bad source file or wrong metadata.

If the second download has the same wrong md5, the transfer is not corrupting anything and the source file or its metadata is wrong. The file then fails for good with `checksum mismatch for <name> twice in a row (expected X, got Y): the source file or its metadata is likely wrong; kept the download at <path>` (in `--json` output the error code is `download_failed` and this text is the message). A different wrong md5 means the transfer is corrupting data, and retrying continues up to `--retries`. Only one `.md5-mismatch` copy is kept per file: a new mismatch replaces it, and it is deleted once a later attempt verifies. A download that ends with a mismatch therefore needs room for two copies of the file while the retry runs.

`.md5-mismatch` files are never resumed from and never count as a downloaded file; delete them when you are done with them. If the copy cannot be kept because something the rename cannot replace sits at that path (a directory, say), the download is removed instead and the message says `the download could not be kept and was removed`.

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

# Give up on a stream averaging under 1 MiB/s and resume it with a Range request
ia download nasa --min-speed 1M

# Never abandon a slow stream (only the 60 s read timeout applies)
ia download nasa --min-speed 0

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

Every backend handles throttling the same way: a `429` on any request is retried up to three times, waiting what the server's `Retry-After` header says (seconds or an HTTP date, as given) or else a random wait up to a cap that doubles from 1 s; past that the command fails with `rate limited (retry after Ns)` (`--json` error code `rate_limited`). A `5xx` is retried by the HTTP layer on the same rule.

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
| `--checksum-file <PATH>` | Path to pre-computed MD5 checksums file (`--checksums` is accepted as an alias) |
| `--delete-after-upload` | Delete local file after verified upload (with `--multipart`, only once IA lists the assembled file with the expected size and md5; a file reported "not yet verified" is kept) |
| `--no-verify` | Skip Content-MD5 verification |
| `--no-derive` | Skip derivative generation |
| `--no-backup` | Don't keep old file versions |
| `--no-auto-make-bucket` | Error if item doesn't already exist |
| `--no-collection-check` | Skip collection existence check |
| `--no-size-hint` | Don't send x-archive-size-hint header |
| `--test-item` | Upload to test_collection (auto-removed after 30 days) |
| `--open-after-upload` | Open item in browser after upload |
| `--multipart` | Use multipart upload (recommended for files >5 GB): 100 MiB parts, each retried on its own; the same skip check as a single PUT; a rerun resumes from the parts IA holds once they are checked against the local file; a part that fails for good leaves the upload on IA for that rerun; after completion the assembled file is confirmed by size and md5 (see below) |
| `--retries <N>` | Retry attempts per IA-S3 request — per file, or per part with `--multipart` (default: 10). Waits are random, up to a cap that doubles from 1 s to 60 s. A `Retry-After` header sets the wait instead, as given, even above 60 s; `Retry-After: 0` means re-send at once |
| `--dry-run` | Validate everything, upload nothing |
| `--dashboard` | Full-screen TUI dashboard |
| `--json` | Output results as JSONL |

#### Retries

Every IA-S3 request in an upload (the single PUT, or each multipart request: initiate, part, complete, abort, listings) gets `--retries` attempts after the first (default 10). Transient failures retry: connect errors, timeouts, resets, 5xx responses, `429`, and IA's `503 SlowDown`; refusals such as `AccessDenied` do not. The wait before each retry is random, up to a cap that doubles from 1 s to 60 s (full jitter, so retries from many clients do not land together). When the failed response carries a `Retry-After` header, that wait is used instead, as given: the seconds form or the HTTP-date form, even above 60 s, and `Retry-After: 0` re-sends at once. After a `503` on the single PUT, the upload also polls IA's `check_limit` endpoint on the same schedule until the rate limit clears, up to `--retries` polls.

```bash
# Ride out a flaky link: 20 attempts per part
ia upload my-item big.iso --multipart --retries 20
```

#### Resuming a multipart upload

Before sending any part, `--multipart` asks IA for unfinished multipart uploads of the same file name in the item and checks each part IA already holds against the local file: the part number must fall within the file's parts at the current part size (100 MiB), the part's size on IA must equal the local range's size, and its ETag must equal the md5 of the local range. The md5s come from one read of the local file. Every part passes → those parts are skipped and the rest are sent under the same upload ID. Any part fails (the local file changed, the part size changed, a different file has the same name) → a fresh upload starts and the stale one is left on IA, named in a warning: `not resuming multipart upload <id>: part 1 has ETag <etag> on IA but the local range's md5 differs; it is left on IA, discard it with: ia upload cleanup <item> <file> --abort`. With several unfinished uploads for the name, the newest one that passes is resumed. A listing without a part size is checked by md5 alone.

#### Verifying a multipart upload

The skip check applies to `--multipart` as to a single PUT: the one read that gives the per-part md5s also gives the whole-file md5, and a file the item already lists with that md5 is skipped (`--clobber` uploads it anyway; `--clobber --no-verify` skips the read altogether). IA assembles a multipart object after completion, and for about a minute the URL may 404 or serve a placeholder, so a 200 on completion proves nothing. After completion `ia` therefore asks the item's metadata until the file appears with the expected size and md5, for up to 5 minutes, waiting on the retry schedule between polls (1 s to 60 s, as for retries; a `429` is honored through its `Retry-After`, and one that reaches past the 5 minutes ends the check). Three outcomes: listed with the right size and md5 → `uploaded`, and `--delete-after-upload` deletes the local file now; listed with the right size but another md5 → the file fails with `assembled file md5 X on IA does not match local md5 Y`, because the object on IA is wrong; not listed in time → `uploaded, not yet verified` (`--json`: `"status":"uploaded_unverified"`), exit 0 with a warning, the local file kept, and the joblog records the file as ok. A rerun's skip check then settles it: skipped once IA lists the md5, uploaded again if it never does. With `--joblog`, that rerun needs `--no-resume` (the joblog's ok would otherwise skip the file before the check runs). `--no-verify` checks the size only. The result's `md5` is the local md5, as for a single PUT.

#### Multipart part failures

With `--multipart`, a part that fails for good does not abort the upload, because an abort tells IA to delete every part already uploaded. The upload stays on IA, and the file fails with a message that names it. When IA refused the part (`AccessDenied`, `InvalidAccessKeyId`, `BadDigest`, any non-retryable S3 code): `part 3 of 7 refused by IA (AccessDenied: Access Denied): multipart upload <id> is kept with 2 parts on IA; fix the cause and rerun the same command to resume, or discard it with: ia upload cleanup <item> <file> --abort`. When the part's `--retries` ran out: `part 3 of 7 failed after 11 attempts (SlowDown: ...): multipart upload <id> is kept with 2 parts on IA; rerun the same command to resume, or discard it with: ia upload cleanup <item> <file> --abort`. When part 1 fails on a fresh upload the message says the upload `is kept on IA with no parts yet`. A remote name with spaces or quote characters is single-quoted in the suggested command. IA's spam rejection on a part is not a part failure: it stops the whole item, as it does for a single PUT. In `--json` output and the joblog this text is the failure message. Rerunning the same command finds the upload, skips the parts IA already holds, and sends the rest. Killing the process mid-upload has always behaved this way; the two paths now match.

```bash
# After a part failure: rerun to resume from the parts already on IA ...
ia upload my-item big.iso --multipart
# ... or discard the kept upload
ia upload cleanup my-item big.iso --abort
```

#### Batch mode and subcommands

**`ia upload --spreadsheet <FILE>`** — Batch upload from a spreadsheet file (CSV/TSV/XLSX/ODS/JSONL). Each row specifies an identifier, file path, and optional metadata. Rows sharing the same identifier are grouped into a single item upload.

Required columns: `identifier`, `file`. All other columns become metadata.

Supports the same options as the bare command: `-m`, `--header`, `--checksum-file`, `--no-derive`, `--no-backup`, `--no-auto-make-bucket`, `--no-verify`, `--no-size-hint`, `--no-collection-check`, `--clobber`, `--delete-after-upload`, `--test-item`, `--multipart`, `--retries`, `--dry-run`, `--json`.

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

**`ia upload cleanup <IDENTIFIER> [FILE]`** — List or abort incomplete multipart uploads for an item. Lists by default, with or without FILE: each unfinished upload with its upload ID, when it started, and the parts IA holds (count and bytes). Nothing is aborted unless asked, and there is no interactive prompt. An abort tells IA to delete every part already uploaded; a rerun of the upload resumes from those parts instead, so abort only what you mean to discard.

| Flag | Description |
|------|-------------|
| `<IDENTIFIER>` | Item identifier |
| `[FILE]` | Only this file's incomplete uploads (lists them; add `--abort` to abort) |
| `--abort` | Abort FILE's incomplete upload(s); requires FILE |
| `--abort-all` | Abort every incomplete upload of the item |
| `--dry-run` | Show what `--abort` or `--abort-all` would abort; abort nothing (with neither, lists) |
| `--json` | Output as JSONL: one object per upload for a listing (`key`, `upload_id`, `initiated`, `parts`, `bytes`; no lines when there are none), one per abort with `"action": "aborted"` or `"would_abort"`; an error is `{"error": {"code", "message"}}` on stderr |

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

# List stale multipart uploads, then abort one of them
ia upload cleanup my-item
ia upload cleanup my-item file.zip --abort

# Dry run — validate without uploading
ia upload my-item file.pdf --dry-run

# Upload with dashboard
ia upload my-item ./files/ --dashboard
```

#### Resuming uploads

Two things can resume, and they are different. A plain upload sends each file in one PUT: if it is interrupted, the rerun sends that file again from byte 0 (the skip check spares files the item already lists with the same md5). With `--multipart`, the rerun resumes a file from the parts IA already holds, after checking them against the local file (see "Resuming a multipart upload" above). On top of either, `--joblog` skips whole files a previous run finished.

When `--joblog` is provided, files the joblog lists as uploaded are skipped, so you can safely re-run the same command after an interruption.

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

Requests to GitHub's release API are retried up to three times on a `5xx`, a `429` or a connection failure. The wait before each retry is random, up to a cap that doubles from 1 s to 30 s; a `Retry-After` header on the failed response sets the wait instead, as given.

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

LLM requests that fail with a `429`, a `5xx` or a connection failure are retried up to five times. The wait before each retry is random, up to a cap that doubles from 1 s to 60 s; a `Retry-After` header on the failed response (seconds or an HTTP date) sets the wait instead, as given.

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
| `--joblog <PATH>` | Write operation results to a JSONL log file; a rerun with the same `--joblog` skips what it records as done: finished files for upload, fully finished items for download (a partial file resumes from its `.part` regardless) |
| `--no-resume` | Ignore the joblog's record of finished work and process every file again (download, upload, ai) |
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

Track operations with `--joblog`. The log is a JSONL file (one JSON object per line) recording the outcome of each file operation. When `--joblog` is provided, re-running the same command skips what the log records as done: finished files for upload, fully finished items for download. That is one of two resume mechanisms: download resumes a partial file from its `.part` with a `Range` request whether or not a joblog is in use, and `--multipart` uploads resume from the parts IA holds; the joblog works at the level of whole files on top of both (see "After an interruption" under `ia download` and "Resuming uploads" under `ia upload`).

```sh
# Download with job logging
ia download nasa --joblog downloads.jsonl

# View job log summary
ia status --joblog downloads.jsonl

# Re-run to retry failures (what the joblog records as done is skipped)
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
