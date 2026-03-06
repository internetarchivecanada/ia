# ia — Internet Archive CLI & Library in Rust

## Project Overview

Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python CLI and library.
Design doc: `docs/plans/2026-02-20-ia-rust-port-design.md`

## ABSOLUTE SAFETY RULES

These rules are non-negotiable and must NEVER be violated:

### 1. NEVER Send Write Requests to Live archive.org
- NEVER send POST, PUT, DELETE, or PATCH requests that modify data on live archive.org
- NEVER run metadata modify, upload, delete, or any data-altering operation against live archive.org
- ALL write operation tests MUST use wiremock mocks — ZERO live write requests
- Read-only operations (GET requests, search queries, auth login/check) are OK
- POST to read-only endpoints (search scrape API, FTS, xauthn login) is OK
- The developer will perform manual live testing of write operations

### 2. Authentication: Allowed for Read, Never for Live Writes
- Reading credentials from config files and environment variables is OK
- Sending `Authorization: LOW` headers for read operations is OK
- Loading and using S3 keys and cookies in code and tests is OK
- NEVER use real credentials in automated tests against live write endpoints
- All authenticated write tests MUST use wiremock mocks

### 3. GitHub: jjjake Account Only
- This repo is `jjjake/ia` (PRIVATE) — direnv handles auth (see global CLAUDE.md)

### 4. NEVER Publish Secrets
- NEVER commit credentials, API keys, tokens, or .env files
- NEVER include real IA credentials in test fixtures or examples

## Architecture

- **Cargo workspace**: `ia-core` (library) + `ia-cli` (binary with TUI)
- `IaClient` wraps `reqwest::Client`, module-level operation functions
- Design doc: `docs/plans/2026-02-20-ia-rust-port-design.md`
- **External consumers**: `ia-core` is a standalone library. The desktop GUI ([jjjake/ia-gui](https://github.com/jjjake/ia-gui)) consumes it as a git dependency. Public API design must consider third-party usage.

## Crate Stack

- CLI: `clap` 4 (derive)
- Async: `tokio` 1 + `futures` 0.3
- HTTP: `reqwest` 0.12 (rustls-tls) + `reqwest-middleware` + `reqwest-retry` — middleware stack gives declarative retry without cluttering business logic
- Serialization: `serde` 1 + `serde_json` 1
- Config: `configparser` 3 — INI format for backwards compat with Python `internetarchive`'s `ia.ini`
- Errors: `thiserror` 2 (library) + `anyhow` 1 (CLI)
- Logging: `tracing` 0.1 + `tracing-subscriber` 0.3
- Console: `indicatif` 0.17 + `console` 0.15 + `comfy-table` 7 — lightweight ASCII table rendering for `ia list`
- TUI: `ratatui` (default feature, `--dashboard` flag)
- Metadata write: `json-patch` 3 — archive.org API requires RFC 6902 JSON Patch format; `urlencoding` 2
- Spreadsheet: `calamine` 0.26 — single API for XLSX/ODS/XLS read, pure Rust; `rust_xlsxwriter` 0.79 — XLSX write; `csv` 1 (CSV/TSV)
- Upload: `md-5` 0.10 — MD5 checksums for Content-MD5 verification and file integrity
- Auth: `rpassword` 5 — hidden password input for interactive login
- Testing: `wiremock` 0.6 + `assert_cmd` 2 + `tempfile` 3

**Adding dependencies**: Don't add crates without asking first. Prefer `std`, existing deps, or small focused code over new dependencies. Every crate must have a clear justification — "it's easy to add" is not one. When a new crate is approved, update the crate stack above with the crate and a one-liner rationale.

## Build Notes

- Rust 1.85 — pin `comfy-table` to 7.1.x and `wiremock` to 0.6.2 (later versions require let chains)
- `Cargo.lock` is tracked (binary crate)
- `reqwest-middleware` doesn't expose `.json()` — use `.header("content-type", ...).body(serde_json::to_vec(...))`

## User-Agent

Every HTTP request to archive.org MUST include a User-Agent string:
```
ia/{version} ({OS} {arch}; N; en) Rust/{rust_version}
```

## Development Workflow

### Feature Development Order

1. **Design first** — Brainstorm, explore approaches, write design doc (`docs/plans/`)
2. **Create GitHub issue(s)** — After the design is finalized, create issue(s) on `jjjake/ia` based on what the design/plan reveals. A single feature may need multiple issues.
3. **Create worktree** — `git worktree add` from main with `feat/`, `fix/`, or `refactor/` prefix
4. **Implement** — TDD, frequent commits, link issues in commit messages
5. **PR** — Push branch, create PR with `Closes #N` for every issue it resolves

### Rules

- **ALWAYS use git worktrees** for feature/fix branches (see global CLAUDE.md) — NEVER work directly on main or on unrelated feature branches
- **ALWAYS create GitHub issues before implementation** — Issues come after design but before any code. Every piece of work must be tracked.
- **ALWAYS make high-quality issues** — Every issue must have: appropriate labels, detailed description with context, and references to relevant design docs/plans (file paths like `docs/plans/YYYY-MM-DD-<topic>-design.md`). Issues should be self-contained enough for someone to understand the work without digging through other issues.
- **ALWAYS link issues in PRs**: Use `Closes #N` in the PR body for every issue the PR resolves, so they auto-close on merge. List each issue on its own line.
- **ALWAYS add tests**: Every change must include tests that verify the new behavior.
- **ALWAYS push as a PR**: Push the feature branch and create a GitHub PR for review.
- **ALWAYS update help text**: When adding or modifying CLI flags, subcommands, or behaviors, update the corresponding `about`, `long_about`, `after_long_help`, and option-level help strings. Help text is user-facing documentation — it must stay accurate.
- Run `cargo check`, `cargo test`, `cargo clippy` before committing

### Pre-Implementation Checklist (MUST complete before writing any code)

Stop and verify each item. Do not skip ahead.

- [ ] On a clean worktree created from main (NOT main, NOT an unrelated branch)
- [ ] `git status` is clean — no stale changes from other work
- [ ] Design doc exists in `docs/plans/` AND is committed to the feature branch
- [ ] Implementation plan exists in `docs/plans/` AND is committed to the feature branch
- [ ] GitHub issue(s) created and number(s) noted for PR linking

### Pre-PR Checklist (MUST complete before creating a PR)

Stop and verify each item. Do not skip ahead.

- [ ] `git status` — no uncommitted files that belong in the PR
- [ ] `cargo test -p ia-core -p ia-cli` — all tests pass
- [ ] `cargo clippy -p ia-core -p ia-cli -- -D warnings` — zero warnings
- [ ] Design/plan docs are committed (not just written to disk)
- [ ] MEMORY.md updated with new modules, commands, or status changes
- [ ] PR body includes `Closes #N` for every linked issue

### Post-Merge Checklist (MUST complete after PR is merged)

Order matters — worktree must be removed before branch deletion.

- [ ] Remove the worktree first (`git worktree remove <path>`) — git won't delete a branch with an active worktree
- [ ] Merge via `gh pr merge --merge --delete-branch` (or merge + manual branch cleanup)
- [ ] Pull main to get the merge commit (`git pull` on main)
- [ ] Update MEMORY.md status (e.g., "IN PROGRESS" → "COMPLETE")

## Agent-Friendly Output (`--json`)

Every command must support `--json` as a **subcommand flag** (not global). Design doc: `docs/plans/2026-02-23-agent-friendly-output-design.md`

- `--json` → stdout becomes JSON/JSONL, stderr becomes structured error JSON
- `--json` is per-subcommand (output shape differs per command), shared error infra in ia-core
- Errors: `{"error": {"code": "...", "message": "...", ...extra_fields}}`
- Exit codes: binary 0/1 only. Error details in stderr JSON, not exit codes.
- Batch operations: JSONL streaming (one object per line as items complete)
- `--json` suppresses progress bars, color, and decorative output
- `--json --dashboard` is mutually exclusive (error)
- ALWAYS add `--json` support when creating new commands

## Global CLI Short Flags (Reserved)

These short flags are used by global options and MUST NOT be reused in subcommands:
- `-c` — `--config-file`
- `-l` — `--log`
- `-d` — `--debug`
- `-i` — `--insecure`
- `-H` — `--host`
- `-j` — `--jobs`
- `-q` — `--quiet`

## Key IA API Quirks (from research)

- IA S3 does NOT support chunked transfer encoding — always set Content-Length
- Search scrape API uses cursor-based pagination (POST to /services/search/v1/scrape)
- Advanced search uses page-based pagination (GET /advancedsearch.php)
- FTS uses scroll-based pagination on a different host (be-api.us.archive.org)
- File metadata fields (size, mtime) come as strings from JSON API — deserialize carefully
- Auth headers must be preserved on redirects to *.archive.org domains
- S3 metadata headers: underscores become double-dashes, non-ASCII wrapped in uri()
- Metadata write POST body: `-target={}&-patch={}&priority={}&access={}&secret={}` (form-encoded, NOT JSON)
- Metadata write uses RFC 6902 JSON Patch (test/add/replace/remove ops)
