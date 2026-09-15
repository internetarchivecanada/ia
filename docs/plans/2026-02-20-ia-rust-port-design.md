# ia — Internet Archive CLI & Library in Rust

**Date**: 2026-02-20
**Author**: Jake (jjjake)
**Status**: Approved
**Repo**: `internetarchivecanada/ia`

## Overview

A full Rust port of the [internetarchive](https://github.com/jjjake/internetarchive) Python CLI and library. The goal is a rock-solid, blazing-fast, beautifully-designed replacement for the `ia` command-line tool, backed by a clean Rust library suitable for building GUIs and other tools.

### Goals

- **Performance**: Concurrent/async downloads and operations (Python is sequential)
- **Reliability**: Robust retry logic, resume support, joblog for batch operations
- **Distribution**: Single static binary (`cargo install ia`) — no Python/pip dependency hell
- **Design**: Beautiful, uv-inspired console output with optional interactive TUI mode
- **HTTP excellence**: Connection pooling, keep-alive, 100-continue for uploads, `Retry-After` respect
- **Disk management**: Multi-disk pool support for large batch downloads
- **Library-first**: Clean Rust API for building GUIs and custom tools
- **Agent-friendly**: Every command supports `--json` structured output for AI agents, MCP tool servers, and programmatic consumers ([design doc](2026-02-23-agent-friendly-output-design.md))

### Non-Goals (for now)

- Write operations to archive.org (upload, delete, metadata modify)
- Authentication with archive.org
- GUI desktop application (future, after CLI/TUI prove out)

### MVP Focus

**Download command** — the most visually impressive and immediately useful feature. Proves out the core HTTP infrastructure, concurrency model, and console output design.

## Architecture

### Project Structure

```
ia/                          # GitHub: internetarchivecanada/ia
├── Cargo.toml               # Workspace root
├── CLAUDE.md                # Claude Code instructions + safety rules
├── docs/
│   └── plans/               # Design docs, implementation plans
├── ia-core/                  # Library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            # Public API re-exports
│       ├── client.rs         # IaClient (wraps reqwest::Client + config)
│       ├── config.rs         # INI config loading (ia.ini compat)
│       ├── error.rs          # Error types (thiserror)
│       ├── types.rs          # Shared types (Item, File, Metadata, etc.)
│       ├── download.rs       # Download operations
│       ├── search.rs         # Search operations (scrape, advanced, FTS)
│       ├── metadata.rs       # Metadata read (write later)
│       ├── files.rs          # File listing
│       ├── catalog.rs        # Tasks API (read-only)
│       └── user_agent.rs     # UA string construction
├── ia-cli/                   # Binary crate (CLI + TUI)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # Entry point, clap arg parsing
│       ├── commands/
│       │   ├── mod.rs
│       │   ├── download.rs   # Download subcommand
│       │   ├── search.rs     # Search subcommand
│       │   ├── metadata.rs   # Metadata subcommand
│       │   ├── list.rs       # List subcommand
│       │   ├── status.rs     # Status subcommand (joblog viewer)
│       │   └── ...
│       ├── output.rs         # Console formatting (progress bars, tables, colors)
│       └── tui/              # TUI mode (ratatui, behind feature flag)
│           ├── mod.rs
│           └── ...
```

Desktop GUI is developed separately at jjjake/ia-gui (separate repo, not yet public), consuming `ia-core` as a git dependency.

### Approach: Client + Typed Operations (B → C)

A thin `IaClient` wraps `reqwest::Client` with IA-specific configuration. Operation modules (`download`, `search`, etc.) are functions that take `&IaClient`. Convenience methods on `IaClient` delegate to these modules.

```rust
let client = IaClient::new()?;

// Module functions (explicit, testable)
let item = ia_core::metadata::get(&client, "nasa").await?;

// Convenience methods (discoverable via autocomplete)
let item = client.get_item("nasa").await?;
```

**Future-proofing**: Define an `IaHttpClient` trait that `IaClient` implements. Operations use concrete `&IaClient` for now; switching to `impl IaHttpClient` later is a one-line change per function. This avoids the Python trap of coupling to a specific HTTP implementation.

## IaClient & HTTP Layer

### Client

```rust
pub struct IaClient {
    http: reqwest::Client,       // Connection pooling, keep-alive (not Connection: close like Python!)
    config: IaConfig,            // Parsed from ia.ini + env vars
    user_agent: String,          // ia/{version} ({OS} {arch}; N; en) Rust/{version}
}
```

Key improvements over Python:
- **Keep-alive by default** (Python sets `Connection: close`)
- **Connection pooling** via reqwest's built-in pool
- **HTTP-level retry** via `reqwest-middleware` + `reqwest-retry` for 5xx on idempotent requests. Metadata writes and task submission retry connect failures only: a 5xx can arrive after the server applied the change, so replaying it would apply the change twice
- **`Retry-After` header respected** globally on all requests
- **100-continue** for uploads (future) via hyper
- **Application-level retry** via `backon` for S3 overload logic (future)

### URL Construction

Each operation module owns its endpoint knowledge. The client provides protocol/host config:

```rust
impl IaClient {
    pub fn host(&self) -> &str       // "archive.org" (configurable)
    pub fn protocol(&self) -> &str   // "https" (configurable)
    pub fn url(&self, path: &str) -> String
}
```

**IA endpoint topology:**
| Host | Purpose |
|------|---------|
| `archive.org` | Metadata, downloads, search (scrape + advanced), tasks, reviews, simplelists, changes, views |
| `s3.us.archive.org` | S3-compatible upload API (different retry/auth model) |
| `be-api.us.archive.org` | Full-text search (FTS) API |
| `web.archive.org` | Wayback Machine CDX API |

**Key service paths on `archive.org`:**
| Path | Method | Purpose |
|------|--------|---------|
| `/metadata/{id}` | GET | Metadata read |
| `/metadata/{id}` | POST | Metadata write (different auth: credentials in body) |
| `/download/{id}/{file}` | GET | File download |
| `/services/search/v1/scrape` | POST | Cursor-based search |
| `/advancedsearch.php` | GET | Page-based search |
| `/services/tasks.php` | GET/POST | Catalog tasks |
| `/services/xauthn/` | POST | Authentication |
| `/services/changes/` | GET | Item change tracking |
| `/services/views/` | GET | View/download statistics |
| `/services/reviews/` | GET/POST/DELETE | Item reviews |
| `/services/simplelists/` | GET/POST | Item relationships/lists |

### Configuration

```rust
pub struct IaConfig {
    pub s3_access: Option<String>,
    pub s3_secret: Option<String>,
    pub cookies: HashMap<String, String>,
    pub general: GeneralConfig,    // host, secure, user_agent_suffix
    pub logging: LoggingConfig,
}
```

**Config discovery order** (matches Python):
1. `IA_CONFIG_FILE` env var
2. `$XDG_CONFIG_HOME/internetarchive/ia.ini`
3. `~/.config/ia.ini` (legacy)
4. `~/.ia` (legacy)

Environment variable overrides: `IA_ACCESS_KEY_ID`, `IA_SECRET_ACCESS_KEY`.

### User-Agent

Format mirrors the Python library:
```
ia/{version} ({OS} {arch}; N; en) Rust/{rust_version}
```

With optional suffix from config: `user_agent_suffix`.

## Download Architecture (MVP)

### Three Modes

1. **Single item**: `ia download nasa --glob="*.mp4"`
2. **Batch**: `ia download --itemlist items.txt` or `ia download --search "collection:nasa"`
3. **Interactive TUI**: `ia download nasa --tui`

### Pipeline

```
┌──────────────┐     ┌───────────────┐     ┌──────────────┐     ┌──────────────┐
│  Item Queue  │────▶│  File Resolver │────▶│  Download    │────▶│  Verify &    │
│              │     │  (metadata +   │     │  Workers     │     │  Finalize    │
│              │     │  filter/glob)  │     │  (N parallel │     │  (checksum,  │
│              │     │               │     │  per item)   │     │  set mtime)  │
└──────────────┘     └───────────────┘     └──────────────┘     └──────────────┘
       │                                          │                      │
       │              ┌───────────────┐           │                      │
       └─────────────▶│  Disk Pool    │◀──────────┘                      │
                      │  Manager      │◀─────────────────────────────────┘
                      └───────────────┘
```

### Concurrency Model

```rust
pub struct DownloadOpts {
    // Concurrency
    pub jobs: usize,              // Concurrent file downloads per item (default: 4)
    pub items: usize,             // Concurrent items in batch mode (default: 2)

    // Destination
    pub destdirs: Vec<PathBuf>,   // Disk pool (default: ["."])
    pub no_directories: bool,     // Flat output (no item subdirectory)

    // Filtering
    pub glob: Option<String>,
    pub exclude: Option<String>,
    pub formats: Vec<String>,
    pub source: Option<Source>,   // original, derivative, metadata

    // Resume/retry
    pub checksum: bool,           // Verify with MD5 (opt-in, not default)
    pub retries: usize,           // Per-file retry count (default: 5)
    // Resume of partial files (.part) is automatic, no flag needed

    // Output
    pub quiet: u8,                // 0=normal, 1=summary only, 2=silent
    pub dry_run: bool,
    pub stdout: bool,             // Write single file to stdout
    pub tui: bool,                // Launch interactive TUI mode
    pub no_timestamps: bool,      // Don't set mtime from Last-Modified
}
```

**Item affinity**: Once a worker starts downloading files from an item, it finishes that item before moving to the next. No fragmented partial items on disk.

**Default skip behavior**: A file is skipped (without downloading) if a local file exists with matching name + size + mtime. Checksum verification is opt-in via `--checksum`.

### Disk Pool

```rust
struct DiskPool {
    disks: Vec<DiskInfo>,   // path, capacity, used space
}

impl DiskPool {
    fn assign_item(&mut self, item_id: &str, estimated_size: u64) -> Result<&Path>;
    fn handle_disk_full(&mut self, item_id: &str) -> Result<&Path>;
    // On disk full: clean up partial item, re-assign to next disk with space, restart
}
```

Users specify multiple destination directories: `ia download --destdir /disk1 --destdir /disk2`. Items are assigned to the disk with the most free space. If a disk fills during download, partial files are cleaned up and the item restarts on the next available disk.

### File Download Flow

1. **Skip check**: File exists + name + size + mtime match → skip (default). File exists + MD5 matches → skip (if `--checksum`).
2. **Resume check**: `.part` file exists → send `Range` header for partial download.
3. **Stream download**: GET `/download/{id}/{file}`, stream to `.part` file, update progress bar.
4. **Finalize**: Rename `.part` → final name, set mtime from `Last-Modified`, log to joblog.
5. **On error**: Retry with backoff (respecting `Retry-After`), up to `--retries`. After max retries, log failure and continue.

## Global Operations Infrastructure

### Global CLI Options

```rust
pub struct GlobalOpts {
    pub config_file: Option<PathBuf>,  // -c, --config
    pub log: bool,                      // -l, --log (enable file logging)
    pub debug: bool,                    // -d, --debug
    pub joblog: Option<PathBuf>,        // --joblog=path
    pub retry_failed: bool,             // --retry-failed (re-run failures from joblog)
    pub quiet: u8,                      // -q (repeatable)
}
```

### Joblog

Append-only JSONL file recording every operation:

```jsonl
{"ts":"2026-02-20T15:30:00Z","op":"download","item":"nasa","file":"photo.jpg","status":"ok","bytes":4200000,"elapsed_ms":2100}
{"ts":"2026-02-20T15:30:02Z","op":"download","item":"nasa","file":"video.mp4","status":"error","error":"timeout after 12s","retries":5}
{"ts":"2026-02-20T15:30:02Z","op":"download","item":"nasa","file":"doc.pdf","status":"skipped","reason":"mtime+size match"}
```

`--retry-failed` reads the joblog and re-runs only entries with `"status":"error"`.

### Status Subcommand

`ia status --joblog downloads.log` shows a summary of a joblog file: total operations, success/fail/skip counts, list of failures with error messages, and the command to retry.

## Console Output Design

### Default CLI (uv-inspired, non-interactive)

**Single item:**
```
nasa  Resolving... 47 files, 12 matching *.jpg (23.4 MB)

  [1/12] earth_from_space.jpg     ████████████████████  4.2 MB   done
  [2/12] moon_landing_01.jpg      ████████████░░░░░░░░  1.8/3.1 MB
  [3/12] shuttle_launch.jpg       ██████░░░░░░░░░░░░░░  0.9/2.8 MB
  [4/12] mars_surface.jpg         ███░░░░░░░░░░░░░░░░░  0.4/2.1 MB
  8 files queued

nasa  Downloaded 12 files (23.4 MB) in 8.2s
```

**Batch mode:**
```
nasa  12 files (23.4 MB)
  [1/12] earth_from_space.jpg     ████████████████████  4.2 MB   done
  [2/12] moon_landing_01.jpg      ███████░░░░░░░░░░░░░  1.2/3.1 MB

apollo11  8 files (15.7 MB)
  [1/8]  lunar_module.jpg         ████████████████████  2.1 MB   done
  [2/8]  first_step.jpg           █████░░░░░░░░░░░░░░░  0.8/3.4 MB

2 items  20 files  Downloaded 6/20 (12.1/39.1 MB)  2.8 MB/s
```

Item identifiers appear as bold section headers. Files are grouped under their item.

### Color Palette

| Element | Color |
|---------|-------|
| Item identifiers | Bold white |
| Success markers ("done", counts) | Green |
| File sizes, ETAs, secondary info | Dim/gray |
| Errors, failed counts | Red |
| Warnings, skipped counts | Yellow |
| Progress bars | Blue/cyan |

Colors auto-detected and disabled when output is piped (via `console` crate).

### TUI Mode (`--tui`)

Full interactive terminal UI (ratatui) with:
- Overall progress bar with aggregate throughput
- Per-worker file progress with individual speeds
- Queue status and completed/failed/skipped counts
- Disk usage per pool disk
- Keyboard controls: pause, retry failed, adjust concurrency, scroll

## Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum IaError {
    #[error("item not found: {0}")]
    NotFound(String),

    #[error("HTTP error {status}: {message}")]
    Http { status: u16, message: String },

    #[error("rate limited (retry after {retry_after}s)")]
    RateLimited { retry_after: u64 },

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("disk full: {path}")]
    DiskFull { path: PathBuf },

    #[error("no disk in pool has {needed} bytes free")]
    NoDiskSpace { needed: u64 },

    #[error("download resume failed, restarting: {reason}")]
    ResumeFailed { reason: String },

    #[error(transparent)]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

CLI uses `anyhow` for context-rich error messages at the application boundary.

## Rust Crate Stack

| Purpose | Crate | Version |
|---------|-------|---------|
| CLI framework | `clap` (derive) | 4 |
| Shell completions | `clap_complete` | 4 |
| Async runtime | `tokio` | 1 |
| Stream combinators | `futures` | 0.3 |
| HTTP client | `reqwest` (rustls-tls, stream, json) | 0.12 |
| HTTP middleware | `reqwest-middleware` | 0.4 |
| HTTP retry | `reqwest-retry` | 0.7 |
| App-level retry | `backon` | 1 |
| Serialization | `serde` (derive) + `serde_json` | 1 |
| XML | `quick-xml` (serialize) | 0.37 |
| Config (INI) | `configparser` | 3 |
| Error (library) | `thiserror` | 2 |
| Error (CLI) | `anyhow` | 1 |
| Logging | `tracing` + `tracing-subscriber` | 0.1 / 0.3 |
| Progress bars | `indicatif` | 0.17 |
| Terminal utils | `console` | 0.15 |
| Tables | `comfy-table` | 7 |
| TUI framework | `ratatui` | (feature-gated) |
| MD5 checksums | `md5` | 0.7 |

## Full CLI Command Map

Commands to implement (read-only MVP first, write ops later):

### Phase 1: MVP (Read-Only)

| Command | Description |
|---------|-------------|
| `ia download <id> [files...]` | Download files from an item |
| `ia download --search <query>` | Download from search results |
| `ia download --itemlist <file>` | Download from item list |
| `ia search <query>` | Search items |
| `ia metadata <id>` | View item metadata |
| `ia list <id>` | List files in an item |
| `ia status [--joblog <file>]` | View joblog status |

### Phase 2: Full Read-Only

| Command | Description |
|---------|-------------|
| `ia tasks <id>` | View catalog tasks (read-only) |
| `ia configure --whoami` | Show current user |
| `ia configure --show` | Show current config |
| `ia configure --check` | Validate credentials |

### Phase 3: Write Operations (Future, Requires Auth)

| Command | Description |
|---------|-------------|
| `ia upload <id> <files...>` | Upload files (with 100-continue!) |
| `ia metadata --modify` | Modify item metadata |
| `ia delete <id> <files...>` | Delete files |
| `ia copy/move` | Server-side copy/move |
| `ia configure` | Interactive auth setup |
| `ia tasks --submit` | Submit catalog tasks |
| `ia reviews` | Manage reviews |
| `ia flag` | Manage item flags |

## Safety Constraints

These are enforced via CLAUDE.md and git hooks:

1. **No writes to archive.org** — No upload, delete, metadata modify, or any POST/PUT to IA services
2. **No authentication** — No reading or using IA credentials (S3 keys, cookies)
3. **GitHub user `jjjake`** — Always verify before `gh` or `git push` commands
4. **Private repo `internetarchivecanada/ia`** — All pushes go here only
5. **User-Agent required** — Every request to archive.org includes proper identification

## Key Design Decisions

1. **Approach B (Client + Typed Operations)** over Session-centric: Avoids Rust ownership issues, naturally extensible, each module independently testable. Trait can be added later for abstraction.

2. **Keep-alive by default** (unlike Python's `Connection: close`): Better performance for batch operations.

3. **mtime+size skip as default** (not checksum): Checksum requires reading every local file from disk. mtime+size is instant and sufficient for most use cases.

4. **JSONL joblog** (not CSV/SQLite): Human-readable, append-only, easy to parse, streamable.

5. **Item affinity in batch downloads**: Prevents fragmented partial items on disk.

6. **CLI + TUI in one binary**: TUI is just a different rendering mode for the same operations.

7. **Disk pool**: Unique differentiator for bulk archive work. Automatic failover when disks fill.

## References

- Python `internetarchive` library: https://github.com/jjjake/internetarchive
- TypeScript `internetarchive-ts`: https://github.com/karpour/internetarchive-ts
- IA Developer Docs: https://archive.org/developers
- IA Items: https://archive.org/developers/items.html
- IA Metadata Schema: https://archive.org/developers/metadata-schema
- TypeScript port issues.md (IA API quirks): https://github.com/karpour/internetarchive-ts/blob/main/issues.md
