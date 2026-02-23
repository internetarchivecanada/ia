# Why Rust?

The Python [`internetarchive`](https://github.com/jjjake/internetarchive) library and its `ia` CLI have served the Internet Archive community well for over a decade. This rewrite addresses several long-standing technical limitations that are difficult or impossible to fix within the existing Python codebase.

## Concurrent downloads

The Python CLI downloads one file at a time. There is no `--jobs` flag and no way to parallelize downloads within a single invocation. This has been an open request for years, and the architecture makes it hard to add: the library is synchronous Python built on `requests`, with no async support.

The Rust version downloads files concurrently by default (`--jobs 2`). It uses a shared `tokio::sync::Semaphore` to control parallelism across both files within an item and items within a batch. You can raise the limit (`--jobs 8`) or lower it (`--jobs 1`) depending on your network and the server's tolerance.

## HTTP performance

The Python library sets `Connection: close` on every HTTP request. This means every file download requires a fresh TCP connection and TLS handshake — a measurable overhead when downloading thousands of small files from the same server.

The Rust version uses HTTP keep-alive and connection pooling (via `reqwest`). Multiple downloads to the same host reuse existing connections. For uploads (future), it also supports `Expect: 100-continue`, which avoids sending a large request body only to get a 4xx rejection.

## Resume

Resuming interrupted downloads of large files is unreliable in the Python CLI. The Rust version implements byte-range resume: if a `.part` file exists from a previous interrupted download, it sends a `Range` header to continue where it left off rather than restarting from the beginning. After completion, the file's checksum is verified against the server's metadata.

## Distribution

The Python CLI requires a working Python installation and `pip` (or a PEX bundle that still needs a Python interpreter). Version conflicts, broken pip installs, and virtualenv confusion are common pain points, especially for non-developer users like archivists and librarians.

The Rust version compiles to a single static binary with no runtime dependencies. Prebuilt binaries are available for:

- Linux (x86_64)
- macOS (Apple Silicon)
- Windows (x86_64)

Download, make executable, and run. No package manager required.

## Modern UX

The Rust CLI provides:

- **Progress bars** for individual files and overall batch progress
- **Colored, structured output** inspired by [uv](https://github.com/astral-sh/uv)
- **Full-screen TUI dashboard** (`--dashboard`) for monitoring large batch downloads
- **JSONL job logging** (`--joblog`) that records every operation for auditing and retry
- **Multi-disk pool** support for spreading large downloads across multiple drives

## Agent and machine integration

The Python CLI was designed for humans at a terminal. The Rust CLI is designed for three audiences:

1. **Humans** — colored output, progress bars, tables, TUI dashboard
2. **AI coding agents** (Claude Code, Cursor, Copilot) that shell out to CLI tools, read `--help`, construct commands, and parse stdout
3. **Machine consumers** — MCP tool servers that wrap each command as a typed tool, and internal software like book scanning pipelines and batch archiving systems

In the agentic coding paradigm, a well-designed CLI with structured output is a better integration surface than a language-specific library. The CLI encapsulates all IA API complexity — S3 quirks, rate limiting, retry logic, three search backends, metadata patch format — behind a stable, language-agnostic interface. `--help` is the documentation, flags are the parameters, stdout is the return value.

Every command supports a `--json` flag:

- **stdout**: Data as JSON (single object) or JSONL (one object per line for streaming/batch)
- **stderr**: Errors as typed JSON objects with stable error codes for programmatic matching
- **Exit codes**: Binary `0`/`1` — error details live in the structured stderr output
- **No progress bars, no color, no decorative output** — clean machine-parseable data

Batch operations (download, search, metadata modify) stream JSONL — one object per line, parseable with `jq` while the command is still running. This is the same format used by `--joblog`, so tooling that reads one can read both.

The Python CLI has no equivalent. Adding structured output to a synchronous, print-statement-driven codebase would require touching every command — effectively a rewrite.

See the [agent-friendly output design](plans/2026-02-23-agent-friendly-output-design.md) for the full convention, error schema, and per-command output shapes.

## Further reading

- [Design document](plans/2026-02-20-ia-rust-port-design.md) — full architecture and rationale
- [Implementation plan](plans/2026-02-20-ia-implementation-plan.md) — milestone breakdown and task list
