# ia

A command-line tool for interacting with the [Internet Archive](https://archive.org), written in Rust.

This is a from-scratch Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python library and CLI. It currently supports read-only operations: downloading, searching, listing files, and viewing metadata.

## Installation

### Prebuilt binaries

Download the latest binary from [GitHub Releases](https://github.com/jjjake/ia/releases):

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

### Build from source

Requires [Rust](https://www.rust-lang.org/tools/install) 1.75+.

```sh
git clone https://github.com/jjjake/ia.git
cd ia
cargo install --path ia-cli
```

To include the optional TUI mode:

```sh
cargo install --path ia-cli --features tui
```

## Quick start

### Download an item

```sh
# Download all files from an item
ia download nasa

# Download only specific file types
ia download nasa --glob "*.jpg"

# Download multiple items
ia download item1 item2 item3

# Download items matching a search query
ia download --search "collection:nasa AND mediatype:image"
```

### Search

```sh
# Search for items
ia search "collection:nasa AND mediatype:texts"

# Get identifiers only (useful for piping)
ia search "subject:mars" --itemlist

# Get the number of matching items
ia search "collection:opensource" --num-found

# Full-text search
ia search "apollo 11 transcript" --fts
```

### List files

```sh
# List files in an item
ia list nasa

# Show specific columns
ia list nasa --columns name,size,format

# Show download URLs
ia list nasa --location

# Filter by glob
ia list nasa --glob "*.pdf"
```

### View metadata

```sh
# Display item metadata as JSON
ia metadata nasa

# Check if an item exists
ia metadata nasa --exists

# List file formats in an item
ia metadata nasa --formats
```

## Global options

These options can be used with any subcommand:

| Flag | Description |
|------|-------------|
| `-c, --config-file <PATH>` | Path to configuration file |
| `-i, --insecure` | Allow insecure (HTTP) connections |
| `-H, --host <HOST>` | Override the archive.org host |
| `--user-agent-suffix <STRING>` | Append to the default User-Agent |
| `--joblog <PATH>` | Write operation results to a JSONL log file |
| `--retry-failed` | Retry failed operations from a job log |
| `-q, --quiet` | Suppress output (repeat for more quiet) |
| `-l, --log` | Enable logging |
| `-d, --debug` | Enable debug output |

## Configuration

`ia` reads settings from an INI configuration file, compatible with the Python `ia` tool's format. The default location is `~/.config/internetarchive/ia.ini`.

```ini
[general]
host = archive.org
secure = true
```

Override the config file path with `--config-file`:

```sh
ia --config-file ~/my-ia.ini download nasa
```

## Advanced features

### Job logging

Track download operations with `--joblog`:

```sh
ia download nasa --joblog downloads.jsonl

# View job log summary
ia status --joblog downloads.jsonl

# Retry failed downloads
ia download nasa --joblog downloads.jsonl --retry-failed
```

### Disk pool

Distribute downloads across multiple disks:

```sh
ia download --search "collection:nasa" --destdir /mnt/disk1 --destdir /mnt/disk2
```

Items are assigned to the disk with the most free space. If a disk fills up, downloads automatically fail over to the next available disk.

### Batch downloads

Download many items concurrently:

```sh
# From a search query
ia download --search "collection:nasa" --items 4

# From a file of identifiers
ia download --itemlist items.txt --items 4
```

### TUI mode

An interactive terminal UI for monitoring downloads (requires building with `--features tui`):

```sh
ia download nasa --dashboard
```

## Architecture

The project is a Cargo workspace with two crates:

- **ia-core** -- Library crate with the client, API types, download engine, search backends, and utilities
- **ia-cli** -- Binary crate with the CLI interface, progress display, and optional TUI

## License

[AGPL-3.0](LICENSE)
