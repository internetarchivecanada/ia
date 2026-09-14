# ia — Agent Instructions

Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python CLI and library.
Design doc: `docs/plans/2026-02-20-ia-rust-port-design.md`

## Safety

**Tests must be hermetic — zero outbound network requests.** Every HTTP call in a test, read or write, must go through a `wiremock` mock.

Outside tests: never send write requests (POST/PUT/DELETE/PATCH) to live archive.org from any automated code path.

## Architecture

- **Cargo workspace**: `ia-core` (library) + `ia-cli` (binary with TUI)
- `IaClient` wraps `reqwest::Client` with middleware retry stack
- `ia-core` is a standalone library also consumed by [ia-gui](https://github.com/internetarchivecanada/ia-gui). Public API changes must consider external consumers.

## Crate Stack

- CLI: `clap` (derive)
- Async: `tokio` + `futures`
- HTTP: `reqwest` (rustls-tls) + `reqwest-middleware` + `reqwest-retry`
- Serialization: `serde` + `serde_json`
- Config: `configparser` — INI format for backwards compat with Python `ia.ini`
- Errors: `thiserror` (library) + `anyhow` (CLI)
- Logging: `tracing` + `tracing-subscriber`
- Console: `indicatif` + `console` + `comfy-table`
- TUI: `ratatui` (default feature, `--dashboard` flag)
- Metadata write: `json-patch` (RFC 6902); `urlencoding`
- Spreadsheet: `calamine`; `rust_xlsxwriter`; `csv`
- Upload: `md-5`
- Auth: `rpassword`
- Testing: `wiremock` + `assert_cmd` + `tempfile`

Versions live in `Cargo.toml` (load-bearing pins are explained in Build). Don't add crates without asking. Prefer `std`, existing deps, or small focused code. When a new crate is approved, update this list.

## Build

- Rust 1.85 — pin `comfy-table` to 7.1.x and `wiremock` to 0.6.2 (later versions require let chains)
- `Cargo.lock` is tracked (binary crate)
- `reqwest-middleware` doesn't expose `.json()` — use `.header("content-type", ...).body(serde_json::to_vec(...))`
- Every HTTP request must include User-Agent: `ia/{version} ({OS} {arch}; N; en) Rust/{rust_version}`

## Workflow

Work happens on feature branches. main accepts only pull requests, enforced by GitHub branch protection since the repository became public.

- For non-trivial work, write a design doc or implementation plan in `docs/plans/` and commit it before implementation code.
- Create worktrees with `scripts/ia-worktree <type> <slug>` (types: `fix`, `feat`, `refactor`, `docs`, `chore`) — it names the branch `<type>/<slug>`, checks for branch collisions, and symlinks untracked `.claude` settings. Clean up after merge with `scripts/ia-cleanup <slug>`.
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
- All `/download/` GETs send `cnt=0` to suppress the public view counter; `ia download --count-views` opts out. Details in the doc comments on `download::ensure_cnt_zero` and `download::fetch_response`.
