# ia — Agent Instructions

Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python CLI and library.
Design doc: `docs/plans/2026-02-20-ia-rust-port-design.md`

## Safety

**Tests must be hermetic — zero outbound network requests.** Every HTTP call in a test, read or write, must go through a `wiremock` mock.

Outside tests: never send write requests (POST/PUT/DELETE/PATCH) to live archive.org from any automated code path.

## Architecture

- **Cargo workspace**: `ia-core` (library) + `ia-cli` (binary with TUI)
- `IaClient` wraps `reqwest::Client` with middleware retry stack
- `ia-core` is a standalone library also consumed by [ia-gui](https://github.com/jjjake/ia-gui). Public API changes must consider external consumers.

## Crate Stack

- CLI: `clap` 4 (derive)
- Async: `tokio` 1 + `futures` 0.3
- HTTP: `reqwest` 0.12 (rustls-tls) + `reqwest-middleware` + `reqwest-retry`
- Serialization: `serde` 1 + `serde_json` 1
- Config: `configparser` 3 — INI format for backwards compat with Python `ia.ini`
- Errors: `thiserror` 2 (library) + `anyhow` 1 (CLI)
- Logging: `tracing` 0.1 + `tracing-subscriber` 0.3
- Console: `indicatif` 0.17 + `console` 0.15 + `comfy-table` 7
- TUI: `ratatui` (default feature, `--dashboard` flag)
- Metadata write: `json-patch` 3 (RFC 6902); `urlencoding` 2
- Spreadsheet: `calamine` 0.26; `rust_xlsxwriter` 0.79; `csv` 1
- Upload: `md-5` 0.10
- Auth: `rpassword` 5
- Testing: `wiremock` 0.6 + `assert_cmd` 2 + `tempfile` 3

Don't add crates without asking. Prefer `std`, existing deps, or small focused code. When a new crate is approved, update this list.

## Build

- Rust 1.85 — pin `comfy-table` to 7.1.x and `wiremock` to 0.6.2 (later versions require let chains)
- `Cargo.lock` is tracked (binary crate)
- `reqwest-middleware` doesn't expose `.json()` — use `.header("content-type", ...).body(serde_json::to_vec(...))`
- Every HTTP request must include User-Agent: `ia/{version} ({OS} {arch}; N; en) Rust/{rust_version}`

## Workflow

Work happens on feature branches — main is protected by GitHub branch protection.

- For non-trivial work, write a design doc or implementation plan in `docs/plans/` and commit it before implementation code.
- Helper scripts are available for git worktrees: `scripts/ia-worktree <type> <slug>` (types: `fix`, `feat`, `refactor`, `docs`, `chore`), `scripts/ia-cleanup <slug>` after merge.
- Run `cargo test -p ia-core -p ia-cli` and `cargo clippy -p ia-core -p ia-cli -- -D warnings` before committing.
- Update CLI help text (`about`, `long_about`, `after_long_help`) when modifying flags or subcommands.
- When changing user-visible behavior (new flags, commands, or defaults), update: docs/usage.md and README.md.

## `--json` Output

Every command supports `--json` as a subcommand flag. Design doc: `docs/plans/2026-02-23-agent-friendly-output-design.md`

- `--json` → stdout becomes JSON/JSONL, stderr becomes `{"error": {"code": "...", "message": "..."}}`
- Exit codes: 0/1 only. Details in stderr JSON.
- `--json` suppresses progress bars, color, and decorative output
- `--json --dashboard` is mutually exclusive

## Reserved CLI Short Flags

Global flags — do not reuse in subcommands:
`-c` (config-file), `-l` (log), `-v` (verbose), `-i` (insecure), `-H` (host), `-j` (jobs), `-q` (quiet)

## IA API Quirks

- IA S3 does NOT support chunked transfer encoding — always set Content-Length
- Search scrape API: cursor-based pagination, POST to /services/search/v1/scrape
- Advanced search: page-based pagination, GET /advancedsearch.php
- FTS: scroll-based pagination on be-api.us.archive.org
- File metadata fields (size, mtime) come as strings from JSON — deserialize carefully
- Auth headers must be preserved on redirects to *.archive.org domains
- S3 metadata headers: underscores become double-dashes, non-ASCII wrapped in `uri()`
- Metadata write POST: form-encoded (`-target`, `-patch`, `priority`, `access`, `secret`), not JSON
- Metadata write uses RFC 6902 JSON Patch (test/add/replace/remove ops)
