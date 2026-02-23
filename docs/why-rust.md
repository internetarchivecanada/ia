# Why Rust?

The Python [`internetarchive`](https://github.com/jjjake/internetarchive) library and its `ia` CLI have served the Internet Archive community well for over a decade. This rewrite addresses long-standing technical limitations that are difficult to fix within the existing Python codebase.

The Python CLI was designed for humans at a terminal. The Rust CLI is designed for three audiences: humans, AI coding agents, and machine consumers. Everything below follows from that.

## Speed

The Python CLI downloads one file at a time with no way to parallelize. Every request opens a fresh TCP connection. Resuming interrupted downloads is unreliable.

The Rust version downloads files concurrently by default, reuses connections across requests, and resumes interrupted downloads with byte-range requests and checksum verification.

## Single binary

The Python CLI requires a working Python installation and `pip`. Version conflicts, broken pip installs, and virtualenv confusion are common pain points — especially for non-developer users like archivists and librarians.

The Rust version compiles to a single static binary with no runtime dependencies:

- Linux (x86_64)
- macOS (Apple Silicon)
- Windows (x86_64)

Download, make executable, and run. No package manager required.

## Modern UX

- **Progress bars** for individual files and overall batch progress
- **Colored, structured output** inspired by [uv](https://github.com/astral-sh/uv)
- **Full-screen TUI dashboard** (`--dashboard`) for monitoring large batch operations
- **JSONL job logging** (`--joblog`) for auditing and retry
- **Multi-disk pool** support for spreading large downloads across multiple drives

## Agent and machine native

Every command supports `--json` for structured output. AI agents read `--help` and parse stdout. MCP tool servers wrap each command as a typed tool. Pipelines and batch systems consume JSONL streams.

The CLI is a language-agnostic integration surface — any tool that can spawn a process and read stdout can use it. See [Design philosophy](design-philosophy.md) for how this works and where we're heading.

## Further reading

- [Design philosophy](design-philosophy.md) — core design principles and architecture
- [Usage guide](usage.md) — quick start, configuration, advanced features
- [Full design document](plans/2026-02-20-ia-rust-port-design.md) — architecture and rationale
