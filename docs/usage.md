# Usage

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
| `-j, --jobs <N>` | Concurrent operations (default: 2) |
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

An interactive terminal UI for monitoring downloads (built-in, ships by default):

```sh
ia download nasa --dashboard
```

## Architecture

The project is a Cargo workspace with three crates:

- **ia-core** -- Library crate with the client, API types, download engine, search backends, and utilities
- **ia-cli** -- Binary crate with the CLI interface, progress display, and TUI dashboard
- **ia-gui** -- Desktop GUI application built with Slint

See [the design doc](plans/2026-02-20-ia-rust-port-design.md) for full architectural details.
