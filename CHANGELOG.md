# Changelog

All notable changes to this project will be documented in this file.

## [0.8.0] - 2026-03-10

### Added

- **Upload command** (`ia upload`) — single file, multi-file, stdin, and directory uploads to Internet Archive S3
- **Batch upload** (`ia upload import`) — bulk uploads from CSV/TSV/XLSX/ODS/JSONL spreadsheets with per-row metadata
- **Upload template** (`ia upload template`) — generate a pre-filled CSV from a local directory for batch upload workflows
- **Multipart upload** (`--multipart`) — chunked uploads for large files (>5 GB), with automatic resume via server-side state
- **Upload cleanup** (`ia upload cleanup`) — abort stale multipart uploads for an item
- **Upload dashboard** (`--dashboard`) — full-screen TUI for monitoring upload progress (single and batch modes)
- **ProgressBody** — async body stream wrapper for byte-level upload progress tracking
- **Tasks API** (`tasks.rs`) — minimal read-only client for polling archive.org task status (used by upload dashboard)
- **Metadata schema** (`ia metadata schema`) — browse and filter the archive.org metadata schema; single-field detail cards
- **Shared TUI framework** (`tui/framework.rs`) — `Dashboard` trait and `TerminalGuard` lifecycle, shared by download and upload dashboards
- **Shared TUI widgets** (`tui/widgets.rs`) — `ThroughputTracker`, format helpers, and reusable panel components

### Changed

- Download dashboard migrated to the shared TUI framework and widgets
- `format_bytes` consolidated to a single KiB-based implementation across all TUI code
- Upload streams file bodies instead of reading entire files into memory
- MD5 checksums run on the blocking thread pool to avoid starving the async runtime

### Fixed

- Unsafe `transmute` in upload progress callback replaced with `Arc<dyn Fn>`
- `--delete-after-upload` now rejected when combined with `--no-verify`
- Panic on odd-length hex strings in checksum parsing
- HTTP error classification for upload retry (transient vs. permanent)
- Cross-platform `--open-after-upload` support

### Security

- Upload validates identifiers, metadata, and file existence before any network request
- Collection existence check prevents uploads to non-existent collections

## [0.7.0] - 2026-03-05

### Added

- **Compound metadata operations** — chain multiple metadata writes with `+` separator into a single POST request (`ia metadata modify -m "title:X" + -m "subject:Y"`)
- `compute_compound_patch()` for building chained RFC 6902 JSON Patch arrays
- `modify_compound()` for single-POST compound metadata writes
- `WriteContext` struct consolidates shared write parameters across metadata subcommands

### Changed

- `ChangeGroup` converted from tuple alias to a named struct for clarity

### Fixed

- False-positive compound detection on non-metadata commands
- Insert continuations now produce one `ChangeGroup` per `-m` argument

## [0.6.0] - 2026-03-04

### Added

- **`ia update list`** — show available versions from GitHub Releases (5 most recent by default, `--all` for full list)
- **`ia update install`** — install a specific version by tag, with minimum-version floor enforcement
- **Metadata export** (`ia metadata export`) — export item metadata to CSV/TSV/XLSX/JSONL with `write_spreadsheet()`
- **FTS `num_found`** — `fts_num_found()` for counting full-text search results

### Changed

- **CLI restructured** — `ia metadata` split into subcommands (export, modify, append, append-list, insert, remove, import, schema); `ia search` split into backend subcommands (scrape, advanced, fts); `ia ai undo` extracted as subcommand
- `ia update` restructured with `list` and `install` subcommands
- Advanced search default rows set to 50; `rows` field added to `SearchOpts`

### Fixed

- Multi-value metadata fields now expand into indexed columns on export
- Array values preserved in metadata export instead of lossy `join`
- Indexed columns handled correctly on spreadsheet import
- Search CLI: `num_found` helper, `--rows` flag, field filtering fixes
- FTS `!L` prefix parsing

## [0.5.0] - 2026-03-03

### Added

- **`ia config` command** with subcommands: `login`, `show`, `check`, `whoami`, `print-cookies`, `print-auth`
- **Auth module** (`auth.rs`) — `login()` via xauthn API, `check_keys()`, `whoami()`, netrc parsing
- **Config file writing** — `write_config_file()` with merge and secure file permissions
- **Config JSON serialization** — `to_json()` with selective secret redaction (`--show-secrets` to reveal)

## [0.4.4] - 2026-03-03

### Added

- **`ia update`** — self-update command (check for updates, download and replace binary from GitHub Releases); feature-gated behind `self-update`
- **`ia ai`** — AI-assisted metadata cleanup with LLM-powered suggestions; interactive TUI review, headless batch mode, dry-run, undo via joblog
- `--json` flag added to all remaining CLI commands (list, metadata, status, completions)

### Fixed

- Don't retry downloads on 403 and other permanent HTTP errors
- Restore cursor visibility after dashboard exit

### Security

- **Path traversal prevention** in downloads (CVE-2025-58438 equivalent caught before any public release)
- **Symlink detection** — reject symlinks in download paths and `.part` files
- **Resume TOCTOU race** fixed — reset `resume_from` after symlink `.part` removal
- **Download size validation** — reject responses larger than expected
- **Redirect domain restriction** — only follow redirects to `*.archive.org`

## [0.4.3] - 2026-02-23

### Added

- Design doc for `ia ai` command

### Fixed

- Don't retry downloads on 403 and other permanent HTTP errors
- Restore cursor visibility after dashboard exit
- Exclude `ia-gui` from CI and release workflows

## [0.4.2] - 2026-02-23

### Fixed

- Parse JSONL itemlists so identifiers are extracted correctly

## [0.4.1] - 2026-02-23

### Added

- `--json` flag on `ia download` for structured JSONL output
- Shared JSON error serialization infrastructure for `--json` mode
- `design-philosophy.md` — core design principles document
- Agent/machine integration section in `why-rust.md`
- Agent-friendly structured output design doc

### Fixed

- Download skip logic and dashboard bytes tracking

## [0.4.0] - 2026-02-23

### Added

- **Metadata write** — `ia metadata modify`, `--append`, `--append-list`, `--insert`, `--remove` for field-level modifications via RFC 6902 JSON Patch
- **Bulk metadata update** — `--spreadsheet` for batch writes from CSV/TSV/XLSX/ODS/JSONL files, with rate limiting and `RateLimiter` integration
- `--json` flag on `ia search` for structured JSONL output
- Colored, layered CLI help (`-h` terse, `--help` long with examples) using `color-print`
- Long help with examples for all commands (download, search, list, metadata, status, completions)
- `why-rust.md` motivation document
- `usage.md` comprehensive usage guide (extracted from README)
- README redesigned as a polished landing page

### Changed

- `modify()` refactored to use `ModifyRequest` struct
- Replace `atty` with `std::io::IsTerminal`
- Extract shared success/error output helper

### Fixed

- Spreadsheet dry-run change count in summary
- Client-side collection last-removal enforcement

## [0.3.0] - 2026-02-22

### Added

- **GUI desktop app** — Slint-based desktop application for browsing the Internet Archive (now a [separate repo](https://github.com/jjjake/ia-gui))

### Fixed

- Clippy warnings for Rust 1.93 (io_other_error, cloned_ref_to_slice_refs)

## [0.2.0] - 2026-02-21

### Added

- **Search command** (`ia search`) with three backends: scrape (cursor-based), advanced (page-based), and full-text search (scroll-based)
- **List command** (`ia list` / `ia ls`) with table output, column selection, glob filtering, and download URLs
- **Metadata command** (`ia metadata`) with pretty JSON output, `--exists` check, and `--formats` listing
- **Status command** (`ia status`) for viewing job log summaries and failure details
- **Job logging** (`--joblog`) with JSONL append-only format and `--retry-failed` support
- **Batch downloads** via `--search` query and `--items` concurrency control
- **Disk pool** for multi-disk downloads with `--destdir` (repeatable) and automatic failover
- **Dashboard mode** (`--dashboard`) with full-screen ratatui UI for batch downloads — items table, disk space, error log, throughput sparkline, ETA
- **Rich inline output** by default — per-file progress bars with speeds, separator lines, disk space reporting
- **Batch download display** with per-item progress, speeds, and summary stats
- **Shell completions** (`ia completions bash/zsh/fish/powershell`)
- **Compact JSON** output for metadata command (one item per line)
- Global CLI options: `--insecure`, `--host`, `--user-agent-suffix`, `--config-file`

### Changed

- Renamed `--tui` to `--dashboard` — more user-friendly
- TUI (`ratatui`) is now a default feature — no more rebuilding from source
- Dashboard supports batch downloads (was single-item only)

### Fixed

- Linux binary now statically linked (musl) to avoid GLIBC version errors
- Clippy warnings resolved across workspace

## [0.1.0] - 2026-02-21

### Added

- Initial release with download command
- Concurrent async file downloads with semaphore-based parallelism
- File filtering by glob pattern, format, and source type
- Resume support for partially downloaded files
- Checksum verification (`--checksum`)
- Progress bars with indicatif
- Configuration file support (ia.ini compatible)
- Retry with exponential backoff via reqwest-middleware

[0.8.0]: https://github.com/jjjake/ia/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jjjake/ia/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/jjjake/ia/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jjjake/ia/compare/v0.4.4...v0.5.0
[0.4.4]: https://github.com/jjjake/ia/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/jjjake/ia/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/jjjake/ia/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/jjjake/ia/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/jjjake/ia/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jjjake/ia/releases/tag/v0.3.0
[0.2.0]: https://github.com/jjjake/ia/releases/tag/v0.2.0
[0.1.0]: https://github.com/jjjake/ia/releases/tag/v0.1.0
