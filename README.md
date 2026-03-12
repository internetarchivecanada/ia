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
  <a href="https://github.com/jjjake/ia/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/jjjake/ia/ci.yml?branch=main&label=CI" alt="CI status"></a>
  <a href="https://github.com/jjjake/ia/releases/latest"><img src="https://img.shields.io/github/v/release/jjjake/ia" alt="Release"></a>
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
  <a href="docs/why-rust.md">Why Rust?</a> · <a href="docs/design-philosophy.md">Design philosophy</a> · <a href="docs/usage.md">Usage guide</a>
</p>

---

## Install

Download a prebuilt binary from [GitHub Releases](https://github.com/jjjake/ia/releases):

| Platform | Binary |
|----------|--------|
| Linux (x86_64) | `ia-x86_64-unknown-linux-musl` |
| macOS (Apple Silicon) | `ia-aarch64-apple-darwin` |
| Windows (x86_64) | `ia-x86_64-pc-windows-msvc.exe` |

```sh
# Example: Linux
curl -L -o ia https://github.com/jjjake/ia/releases/latest/download/ia-x86_64-unknown-linux-musl
chmod +x ia
sudo mv ia /usr/local/bin/
```

<details>
<summary>Build from source</summary>

Requires [Rust](https://www.rust-lang.org/tools/install) 1.85+.

```sh
git clone https://github.com/jjjake/ia.git
cd ia
cargo install --path ia-cli
```

</details>

## Features

- **Download** — concurrent file downloads with resume, checksum verification, glob/format filtering, multi-disk pool, and a full-screen TUI dashboard
- **Upload** — single file, batch (from spreadsheet), multipart for large files, automatic resume, streaming progress dashboard
- **Search** — three backends: scrape (cursor), advanced (paged), full-text search (scroll)
- **Metadata** — read, write (modify/append/insert/remove), compound operations (`+` chaining), bulk import/export, schema lookup
- **Tasks** — list, submit, rerun, and monitor catalog tasks; view task logs; check rate limits
- **Config & Auth** — login, credential validation, whoami, cookie/auth header export
- **Self-update** — check, list versions, install specific releases from GitHub
- **`--json` on all commands** — structured JSON/JSONL output for scripts, AI agents, and MCP tool servers
- **Job logging** — JSONL audit trail with `--retry-failed` support

## Documentation

See the [usage guide](docs/usage.md) for quick start examples, configuration, and advanced features.

Run `ia --help` or `ia <command> --help` for built-in documentation.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for development setup, workflow, and code conventions.

## License

[AGPL-3.0](LICENSE)
