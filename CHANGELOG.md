# Changelog

All notable changes to this project will be documented in this file.

## [0.3.0] - 2026-02-22

### Added

- **GUI desktop app** (`ia-gui`) — Slint-based desktop application for browsing the Internet Archive
  - **Search page** with scrape/advanced/FTS backends, result table, thumbnail support, and export (JSONL/CSV/identifiers)
  - **Item detail page** with metadata summary, files tab with size/format info, and raw JSON viewer
  - **Downloads page** with active download progress, file counts, and download history from job log
  - **Lists page** with create/delete/select lists, add/remove items, download all, export
  - **Metadata browse page** with identifier lookup, file listing, JSON viewer, and save-to-disk
  - **Settings page** with connection config (host, UA suffix, insecure) and download preferences (directory, concurrency, job log)
  - **Status bar** showing connection host, active download count, and list count (updates in real-time)
  - **Sidebar navigation** with page routing between all sections
  - Cross-section integration: "Add to List" from search/metadata, auto-track completed downloads
  - Async backend infrastructure with tokio runtime and IaClient integration
  - Persistent settings (JSON) and list storage

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

[0.3.0]: https://github.com/jjjake/ia/releases/tag/v0.3.0
[0.2.0]: https://github.com/jjjake/ia/releases/tag/v0.2.0
[0.1.0]: https://github.com/jjjake/ia/releases/tag/v0.1.0
