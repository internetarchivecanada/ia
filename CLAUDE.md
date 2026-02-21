# ia — Internet Archive CLI & Library in Rust

## Project Overview

Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python CLI and library.
Design doc: `docs/plans/2026-02-20-ia-rust-port-design.md`

## ABSOLUTE SAFETY RULES

These rules are non-negotiable and must NEVER be violated:

### 1. NEVER Write to archive.org
- NEVER send POST, PUT, DELETE, or PATCH requests to any archive.org service
- NEVER implement upload, delete, metadata modify, or any write operation against live archive.org
- Read-only operations ONLY: GET requests to archive.org endpoints
- The only exception is POST to search endpoints (scrape API, FTS) which are read-only queries

### 2. NEVER Use Authentication with archive.org
- NEVER read, load, or use IA S3 credentials (access key, secret key)
- NEVER read, load, or use IA cookies (logged-in-user, logged-in-sig)
- NEVER read `~/.config/internetarchive/ia.ini` or `~/.ia` for credentials
- NEVER read `IA_ACCESS_KEY_ID` or `IA_SECRET_ACCESS_KEY` environment variables
- NEVER send `Authorization: LOW` headers to archive.org
- Config file reading for non-auth settings (host, user_agent_suffix) is OK

### 3. GitHub: ALWAYS Use jjjake Account
- Before ANY `gh` command or `git push`: run `gh auth switch --user jjjake`
- This repo is `jjjake/ia` (PRIVATE)
- NEVER push to any other repo
- NEVER use any other GitHub account for this project

### 4. NEVER Publish Secrets
- NEVER commit credentials, API keys, tokens, or .env files
- NEVER include real IA credentials in test fixtures or examples

## Architecture

- **Cargo workspace**: `ia-core` (library) + `ia-cli` (binary with TUI)
- **Approach B**: `IaClient` wraps `reqwest::Client`, module-level operation functions
- **MVP**: Download command with concurrent async downloads
- See design doc for full details

## Crate Stack

- CLI: `clap` 4 (derive)
- Async: `tokio` 1 + `futures` 0.3
- HTTP: `reqwest` 0.12 (rustls-tls) + `reqwest-middleware` + `reqwest-retry`
- Serialization: `serde` 1 + `serde_json` 1 + `quick-xml` 0.37
- Config: `configparser` 3
- Errors: `thiserror` 2 (library) + `anyhow` 1 (CLI)
- Logging: `tracing` 0.1 + `tracing-subscriber` 0.3
- Console: `indicatif` 0.17 + `console` 0.15 + `comfy-table` 7
- TUI: `ratatui` (feature-gated)
- Retry: `backon` 1
- Testing: `wiremock` 0.6 + `assert_cmd` 2 + `tempfile` 3

## User-Agent

Every HTTP request to archive.org MUST include a User-Agent string:
```
ia/{version} ({OS} {arch}; N; en) Rust/{rust_version}
```

## Development Workflow

- Use GitHub issues on `jjjake/ia` to organize work
- Ralph Loop for iterating through issues
- Keep docs/plans/ updated with design decisions
- Keep MEMORY.md updated with conventions and lessons learned
- Run `cargo check`, `cargo test`, `cargo clippy` before committing

## Key IA API Quirks (from research)

- IA S3 does NOT support chunked transfer encoding — always set Content-Length
- Search scrape API uses cursor-based pagination (POST to /services/search/v1/scrape)
- Advanced search uses page-based pagination (GET /advancedsearch.php)
- FTS uses scroll-based pagination on a different host (be-api.us.archive.org)
- File metadata fields (size, mtime) come as strings from JSON API — deserialize carefully
- Auth headers must be preserved on redirects to *.archive.org domains
- S3 metadata headers: underscores become double-dashes, non-ASCII wrapped in uri()
