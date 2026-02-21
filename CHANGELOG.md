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

[0.2.0]: https://github.com/jjjake/ia/releases/tag/v0.2.0
[0.1.0]: https://github.com/jjjake/ia/releases/tag/v0.1.0
