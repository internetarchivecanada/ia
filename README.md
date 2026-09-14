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
  One static binary, no runtime. Structured output on every command.
</p>

<p align="center">
  <a href="docs/usage.md">Usage</a> · <a href="docs/showcase.md">Showcase</a> · <a href="docs/why-rust.md">Why Rust?</a> · <a href="docs/design-philosophy.md">Design philosophy</a> · <a href="docs/ai-development.md">How it's built</a>
</p>

---

**Alpha.** Interfaces may change between releases. Take care with writes; `upload`, `metadata modify` and `tasks submit` all accept `--dry-run`.

Nearly all of the implementation is written by AI agents under human direction. [How it's built](docs/ai-development.md) covers the safeguards, what has gone wrong, and how it was caught.

## Quick start

```sh
ia download nasa                            # fetch an item
ia search "collection:nasa" --json          # JSONL, one object per line
ia metadata nasa --json | jq .metadata      # pipe into anything
ia metadata my-item -m "title:New" --dry-run
```

`ia --help` lists every command. `ia <command> --help` documents that command in full, with examples. That is the reference, and it ships with the binary.

[Showcase](docs/showcase.md) is a short tour with real output. [Usage](docs/usage.md) covers configuration and longer workflows.

## Commands

| Command | What it does |
|---|---|
| `download` | Concurrent downloads with resume, checksum verification, glob/format filters, multi-disk pool, ZIP-member extraction, TUI dashboard |
| `upload` | Single file or batch from a spreadsheet; multipart for large files, automatic resume |
| `metadata` | Read and write, compound `+` operations in one request, bulk import/export, schema lookup and audit |
| `search` | Three backends: scrape (cursor), advanced (paged), full-text (scroll) |
| `list` | File listings with column selection, filters, and download URLs |
| `verify` | Confirm local files exist remotely with matching checksums (MD5/SHA-1/CRC32) |
| `tasks` | List, submit, rerun and monitor catalog tasks; logs and rate limits |
| `collection` | Create collection items with metadata and cover images |
| `config` | Login, credential checks, cookie and auth-header export |
| `status` | Summarize a joblog: what succeeded, what failed |
| `update` | Check for, list, and install releases |

## For scripts and agents

`--json` works on every command. Output is JSONL, one object per line, with errors as structured JSON on stderr. Exit codes are meaningful.

`--dry-run` previews any write. `--joblog <file>` records each operation, and re-running the same command resumes instead of starting over. Concurrency is adaptive by default; `-j N` pins it.

Flags and their behavior are documented in `--help` at every level, so there is no need to read these docs to drive the tool.

## Install

Download a prebuilt binary from [Releases](https://github.com/internetarchivecanada/ia/releases):

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

The binary is `ia-cli`, not `ia`, because the Python [`internetarchive`](https://github.com/jjjake/internetarchive) client already provides `ia`. Examples in these docs use `ia`; alias it with `alias ia=ia-cli`, and `ia-cli completions zsh --rename ia` generates matching completions. `ia-cli` may shorten to `ia` later, but that isn't promised.

The Python client remains maintained and is the stable option; [Why Rust?](docs/why-rust.md) explains what this does differently and what it trades away.

<details>
<summary>Build from source</summary>

Requires [Rust](https://www.rust-lang.org/tools/install) 1.88+.

```sh
git clone https://github.com/internetarchivecanada/ia.git
cd ia
cargo install --path ia-cli
```

</details>

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). If you're thinking of sending code, open an issue first.

## License

[AGPL-3.0](LICENSE)
