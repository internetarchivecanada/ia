# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2026-02-21

### Added

- **Search command** (`ia search`) with three backends: scrape (cursor-based), advanced (page-based), and full-text search (scroll-based)
- **List command** (`ia list` / `ia ls`) with table output, column selection, glob filtering, and download URLs
- **Metadata command** (`ia metadata`) with pretty JSON output, `--exists` check, and `--formats` listing
- **Status command** (`ia status`) for viewing job log summaries and failure details
- **Job logging** (`--joblog`) with JSONL append-only format and `--retry-failed` support
- **Batch downloads** via `--search` query and `--items` concurrency control
- **Disk pool** for multi-disk downloads with `--destdir` (repeatable) and automatic failover
- **TUI mode** with interactive ratatui terminal UI (`--dashboard`, feature-gated behind `tui`)
- Global CLI options: `--insecure`, `--host`, `--user-agent-suffix`, `--config-file`

### Fixed

- Linux binary now statically linked (musl) to avoid GLIBC version errors

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

[0.2.0]: https://github.com/jjjake/ia/releases/tag/v0.2.0
[0.1.0]: https://github.com/jjjake/ia/releases/tag/v0.1.0
