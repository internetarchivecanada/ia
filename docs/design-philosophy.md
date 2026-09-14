# Design Philosophy

This document describes the core design principles behind the `ia` CLI and `ia-core` library. It serves as a reference for contributors, AI agents working on the codebase, and anyone evaluating the project's architecture.

For why this project exists, see [Why Rust?](why-rust.md). For how it's developed, see [How this project is built](ai-development.md). For usage examples, see [Usage guide](usage.md).

## Three audiences

The Python `ia` CLI was designed for one audience: humans at a terminal. The Rust CLI is designed for three:

1. **Humans** — archivists, librarians, researchers, and developers who type commands and read output. They get colored text, progress bars, tables, and a full-screen TUI dashboard.

2. **AI coding agents** — tools like Claude Code, Cursor, and Copilot that shell out to CLI commands, read `--help` to discover capabilities, construct flags, and parse stdout. For these consumers, `--help` is the documentation, flags are the parameters, and stdout is the return value.

3. **Machine consumers** — MCP tool servers that wrap each command as a typed tool, book scanning pipelines, batch archiving systems, and any software that calls `ia` programmatically.

Every design decision flows from serving all three audiences well. Human output is the default. Machine output is one flag away (`--json`). Both see the same data, presented differently.

## CLI as platform

The Python [`internetarchive`](https://github.com/jjjake/internetarchive) package serves two roles: it's a CLI tool (`ia`) and a Python library (`import internetarchive`). Many integrations use the library directly — importing it into Python scripts, calling `internetarchive.download()`, handling exceptions in-process.

A long-term goal of this project is for the Rust CLI to serve both of those roles: the direct command-line tool *and* the integration surface that replaces `import internetarchive` for many use cases. This isn't a claim that the Python library is unnecessary today — it has a large install base and works well. But we believe a well-designed CLI with structured output can cover a surprising amount of what people use the Python library for, with some real advantages:

- **Language-agnostic.** Any language that can spawn a process and read stdout can use it. No Python dependency, no version conflicts, no FFI bindings.
- **Always up to date.** One binary ships all IA API knowledge — S3 quirks, rate limiting, retry logic, three search backends, metadata patch format. Consumers don't re-implement any of it.
- **Stable contract.** `--json` output shapes and `--help` text are the interface. While the project is alpha they may change between releases; changes are called out in release notes.
- **Composable.** Pipe JSONL through `jq`, feed it to another command, or parse it in any language's JSON library.

There are trade-offs. Process spawning has higher latency than an in-process function call. You lose in-process callbacks and streaming iterators. For those cases, the Rust library (`ia-core`) exists and powers both the CLI and a desktop GUI (separate repo, not yet public). But for the common patterns — searching, downloading, reading metadata, modifying metadata — the CLI with `--json` is often the simpler path.

## Library-first architecture

The project is a [Cargo workspace](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html) with two crates:

- **`ia-core`** — the library. HTTP client, API types, download engine, search backends, metadata read/write, error types. This is a real library with a public API, not CLI internals exposed through `pub`.
- **`ia-cli`** — the CLI. A thin presentation layer that parses arguments, calls `ia-core` functions, and formats output for humans or machines.

A separate desktop GUI built with [Slint](https://slint.dev/), not yet public, consumes `ia-core` as an external dependency, which is how the library is kept usable independently of the CLI. Any Rust project can do the same as a git dependency (`ia-core` is not yet published to crates.io) — it is designed as a standalone library for third-party consumers, not just the CLI's internals.

This separation matters: the CLI is one consumer of the library, not the library itself. If you need in-process Rust integration — building a custom tool, embedding IA access in a larger system — `ia-core` is the right dependency. The CLI is for everything else.

## Output design

The CLI has two output modes, toggled by a `--json` flag on each subcommand:

**Human mode** (default): Colored text, progress bars, tables, spinners. Designed for terminals. Inspired by [uv](https://github.com/astral-sh/uv)'s clean, informative style.

**Machine mode** (`--json`): Structured data on stdout, structured errors on stderr (not yet on every command; see below). No progress bars, no color, no decorative output.

Both modes emit the same underlying data. The difference is presentation, not content.

For batch operations (download, search, metadata modify), machine mode streams JSONL — one JSON object per line, emitted as each item completes. This is parseable with `jq` while the command is still running, and uses the same format as `--joblog`.

`--json` and `--dashboard` are mutually exclusive. `--json` and `--quiet` coexist (`--json` wins).

See the [agent-friendly output design](plans/2026-02-23-agent-friendly-output-design.md) for the full convention, error schema, and per-command output shapes.

## HTTP excellence

The Internet Archive serves petabytes of data across distributed infrastructure. The HTTP layer matters.

The Python `internetarchive` library sets `Connection: close` on every request and relies on `urllib3` for connection management — a combination that produces frequent connection reset errors on large batch jobs. The Rust HTTP stack (hyper → reqwest) handles connection pooling, TLS, and error recovery at a lower level, eliminating entire classes of failures.

- **Connection pooling and keep-alive.** The Python library sets `Connection: close` on every request, forcing a fresh TCP+TLS handshake per file. The Rust client reuses connections, which adds up fast when downloading thousands of small files.
- **Byte-range resume.** Interrupted downloads continue where they left off via `Range` headers. The completed file's checksum is verified against server metadata.
- **Retry with backoff.** Transient failures (5xx, timeouts) are retried automatically with exponential backoff via `reqwest-middleware`. Non-idempotent requests (metadata writes, task submission and rerun) are exempt: a 5xx can arrive after the server applied the change, so replaying it would apply the change twice.
- **Rate limit coordination.** When the server returns `429 Too Many Requests`, a shared `RateLimiter` pauses all concurrent workers — not just the one that got throttled. Downloads resume together when the cooldown expires.
- **`Expect: 100-continue`** for uploads. Avoids sending a large request body only to get a 4xx rejection.

See the [architecture design](plans/2026-02-20-ia-rust-port-design.md) for implementation details.

## Error handling

Errors are typed (`IaError` enum with `thiserror`) and carry stable string codes for programmatic matching:

```json
{"error": {"code": "rate_limited", "message": "Rate limited by server", "retry_after": 30}}
```

Exit codes are binary: `0` (success) or `1` (any failure). Error details live in the structured stderr output, not in exit code values. This keeps programmatic matching simple — check the exit code for pass/fail, parse stderr JSON for details.

In human mode, errors are printed as readable messages with color. In machine mode the intent is the same information as JSON on stderr. As of September 2026 only some commands do this; the others still print the plain error text, so scripts should treat the exit code as the reliable signal until that is unified.

See the [agent-friendly output design](plans/2026-02-23-agent-friendly-output-design.md) for the full error code table.

## Concurrency model

All I/O is async (`tokio`). A shared `Semaphore` controls parallelism: `--jobs N` sets the limit for both files within an item and items within a batch. This single knob replaces what would otherwise be separate file-level and item-level concurrency settings.

Batch operations stream results as JSONL — one line per completed file or item. The `--joblog` flag writes the same format to a file for auditing and automatic resume — re-running a command with the same joblog skips already-completed items.

Multi-disk downloads assign items to the disk with the most free space and fail over automatically when a disk fills.

## Further reading

Design documents in `docs/plans/` cover specific subsystems in detail:

| Document | Topic |
|----------|-------|
| [Rust port design](plans/2026-02-20-ia-rust-port-design.md) | Full architecture, project structure, and API design |
| [Agent-friendly output](plans/2026-02-23-agent-friendly-output-design.md) | `--json` convention, error schema, per-command output shapes |
| [Metadata write](plans/2026-02-22-metadata-write-design.md) | RFC 6902 JSON Patch approach for metadata modification |
| [CLI help text](plans/2026-02-23-cli-help-design.md) | Layered `-h`/`--help`, colored examples, audience-aware help |
| [Download output](plans/2026-02-20-download-output-redesign.md) | Progress display, dashboard mode, batch output |
| GUI | Desktop app, separate repo, paused and not yet public |
