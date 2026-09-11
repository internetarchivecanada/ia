<pre align="center">
  ██████╗  █████╗
  ╚══██║ ██╔══██╗
    ██║ ███████║
    ██║ ██╔══██║
  ██████║ ██║  ██║
  ╚═════╝ ╚═╝  ╚═╝
  Internet Archive CLI
</pre>

<p align="center">
  <a href="https://github.com/internetarchivecanada/ia/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/internetarchivecanada/ia/ci.yml?branch=main&label=CI" alt="CI status"></a>
  <a href="https://github.com/internetarchivecanada/ia/releases/latest"><img src="https://img.shields.io/github/v/release/internetarchivecanada/ia" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue" alt="License"></a>
  <img src="https://img.shields.io/badge/status-alpha-orange" alt="Alpha status">
</p>

<p align="center">
  A fast, concurrent command-line tool for the <a href="https://archive.org">Internet Archive</a>, written in Rust.<br>
  Single binary, no dependencies. Built for humans, AI agents, and machine consumers.
</p>

<p align="center">
  <b>This project is in alpha. Bugs may exist and the CLI interface may change between releases. Use caution with write operations (upload, metadata modify).</b>
</p>

<p align="center">
  <a href="docs/why-rust.md">Why Rust?</a> · <a href="docs/ai-development.md">How it's built</a> · <a href="docs/design-philosophy.md">Design philosophy</a> · <a href="docs/usage.md">Usage guide</a> · <a href="docs/showcase.md">Showcase</a>
</p>

---

## How it's built

Nearly all implementation is written by AI agents (primarily [Claude Code](https://claude.com/claude-code)) under human direction — an experiment in agent-built software, run in the open. [How this project is built](docs/ai-development.md) explains the approach, the safeguards that stand in for line-by-line review, and — importantly — what has gone wrong so far and how it was caught. See [CONTRIBUTING.md](./CONTRIBUTING.md) for the practical workflow.

## Install

Download a prebuilt binary from [GitHub Releases](https://github.com/internetarchivecanada/ia/releases):

| Platform | Asset |
|----------|-------|
| Linux (x86_64) | `ia-x86_64-unknown-linux-musl` |
| macOS (Apple Silicon) | `ia-aarch64-apple-darwin` |
| Windows (x86_64) | `ia-x86_64-pc-windows-msvc.exe` |

```sh
# Example: Linux
curl -L -o ia-cli https://github.com/internetarchivecanada/ia/releases/latest/download/ia-x86_64-unknown-linux-musl
chmod +x ia-cli
sudo mv ia-cli /usr/local/bin/
```

The binary is `ia-cli`, not `ia` — the Python [`internetarchive`](https://github.com/jjjake/internetarchive) client already provides `ia`. Examples in these docs use `ia`; alias it with `alias ia=ia-cli`, and `ia-cli completions zsh --rename ia` generates matching completions. `ia-cli` may shorten to `ia` later, but that isn't promised.

<details>
<summary>Build from source</summary>

Requires [Rust](https://www.rust-lang.org/tools/install) 1.85+.

```sh
git clone https://github.com/internetarchivecanada/ia.git
cd ia
cargo install --path ia-cli
```

</details>

## Features

- **Download** — concurrent file downloads with resume, checksum verification, glob/format filtering, multi-disk pool, ZIP-member extraction, HTTP retry diagnostics, and a full-screen TUI dashboard; suppresses archive.org's public view counter by default (`--count-views` to opt back in)
- **Upload** — single file, batch (from spreadsheet), multipart for large files, automatic resume, streaming progress dashboard
- **Verify** — confirm local files exist on archive.org with matching checksums (MD5/SHA-1/CRC32)
- **Search** — three backends: scrape (cursor), advanced (paged), full-text search (scroll)
- **List** — file listings with column selection, glob/source filtering, and download URLs
- **Metadata** — read, write (modify/append/insert/remove), compound operations (`+` chaining), bulk import/export, schema lookup, schema audit
- **Tasks** — list, submit, rerun, and monitor catalog tasks; view task logs; check rate limits
- **Collections** — create collection items with metadata and cover images
- **Config & Auth** — login, credential validation, whoami, cookie/auth header export
- **Self-update** — check, list versions, install specific releases from GitHub
- **Job logging** — JSONL audit trail with automatic resume; `ia status` summarizes results
- **`--json` everywhere** — structured JSON/JSONL output on every command (except `completions`) for scripts, AI agents, and MCP tool servers
- **AI QA (alpha)** — vision-based LLM verification of AI-extracted metadata, with cost estimation and promotion of confirmed fields

## Documentation

See the [usage guide](docs/usage.md) for quick start examples, configuration, and advanced features.

Run `ia-cli --help` or `ia-cli <command> --help` for built-in documentation.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for development setup, workflow, and code conventions.

## License

[AGPL-3.0](LICENSE)
