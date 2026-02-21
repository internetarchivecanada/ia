# ia GUI Design — Slint Desktop Application

**Date:** 2026-02-21
**Status:** Approved
**Relates to:** `ia-core` library, `ia-cli` terminal interface

## Overview

Native desktop GUI for the Internet Archive CLI tool, targeting non-technical librarians and archivists who need a visual interface for searching, browsing, and downloading from archive.org.

## Toolkit Decision

**Slint** (Rust-native GUI framework) was chosen over GTK4, Qt, egui, iced, and Tauri after evaluating:

- **Single binary, no runtime dependencies** — users download and run, no GTK runtime or WebView needed
- **Platform-adaptive styling** — Fluent (Windows), Cupertino (macOS), Cosmic (Linux)
- **Designed for Rust** — no C++ bridge (Qt) or JavaScript layer (Tauri)
- **GPLv3 license** — compatible with this project's AGPLv3
- **Efficient** — designed for embedded hardware, effortless on desktop

### What Slint trades away

- vs GTK4: Smaller widget library, less battle-tested (GTK is 25+ years old)
- vs Qt: Less powerful widget set (Qt is the gold standard for native desktop)
- Slint's widget set covers lists, tables, text inputs, progress bars, dialogs, and menus — sufficient for this application

## Architecture

### Workspace Structure

```
ia/
├── ia-core/          # Shared library (unchanged)
├── ia-cli/           # Terminal CLI (unchanged)
├── ia-gui/           # Slint desktop app
│   ├── Cargo.toml
│   ├── build.rs      # slint-build compilation
│   ├── src/
│   │   ├── main.rs           # App entry, window setup, tokio runtime
│   │   ├── backend/          # Rust logic bridging ia-core to Slint
│   │   │   ├── mod.rs
│   │   │   ├── download.rs   # Download queue management
│   │   │   ├── search.rs     # Search operations
│   │   │   ├── metadata.rs   # Item metadata retrieval
│   │   │   ├── lists.rs      # List persistence and management
│   │   │   └── settings.rs   # Config persistence
│   │   └── models/           # Slint-compatible data models
│   └── ui/                   # .slint markup files
│       ├── app.slint          # Main window, sidebar, routing
│       ├── components/        # Reusable UI components
│       │   ├── search-result.slint
│       │   ├── progress-bar.slint
│       │   ├── item-card.slint
│       │   └── file-row.slint
│       ├── pages/             # Per-section views
│       │   ├── search.slint
│       │   ├── downloads.slint
│       │   ├── lists.slint
│       │   ├── metadata.slint
│       │   ├── item-detail.slint
│       │   └── settings.slint
│       └── theme.slint        # Colors, fonts, spacing
└── docs/plans/
```

### Key Principle

`ia-gui` depends on `ia-core` the same way `ia-cli` does. All business logic lives in `ia-core`. The GUI is purely a frontend. Features added to `ia-core` benefit both CLI and GUI.

### How Slint Works

- **`.slint` files**: Declarative markup for UI layout and styling, compiled at build time
- **Rust backend**: Sets properties (data flows down) and handles callbacks (events flow up)
- **Models**: Slint's list data type, populated from Rust, rendered in ListView/GridView
- **Async**: Works with tokio — background tasks push updates to UI via properties
- **Type-safe**: Slint compiler catches mismatched properties at build time

## Core Design Concepts

### Lists as First-Class Objects

Lists are named collections of item identifiers that flow between operations. They are the connective tissue of the application.

- Created from: search results, manual entry, file import
- Used for: batch download, batch metadata retrieval, browsing
- Formats: JSONL, CSV, TSV, plain text (one ID per line)
- Persisted to: `lists/` directory in app data folder, one file per list
- Auto-maintained "Downloaded" list tracks every downloaded item

### Item Detail as Universal View

Clicking any item anywhere in the app opens the same Item Detail page. Consistent experience whether you arrive from search, lists, downloads, or metadata.

### Job Log as Persistent State

- Every download writes to a JSONL job log (always-on, unlike CLI's `--joblog` flag)
- History tab reads from the job log
- Retry-failed reads failed entries from the job log
- Active download queue is in-memory (lost on app close unless explicitly persisted)

## Navigation & Layout

Sidebar navigation with main content area:

```
┌──────────────────────────────────────────────────────┐
│  Internet Archive                            ─ □ ✕   │
├──────────┬───────────────────────────────────────────┤
│          │                                           │
│ Search   │  (Active section content area)            │
│          │                                           │
│ Downloads│  Clicking any item opens Item Detail      │
│          │                                           │
│ Lists    │                                           │
│          │                                           │
│ Metadata │                                           │
│          │                                           │
│ ── later ──                                          │
│ Upload   │                                           │
│ Tasks    │                                           │
│ ── ── ── ──                                          │
│          │                                           │
│ Settings │                                           │
│          │                                           │
├──────────┴───────────────────────────────────────────┤
│  Status bar: connection, active downloads, list count │
└──────────────────────────────────────────────────────┘
```

| Section | Purpose | Phase |
|---|---|---|
| Search | Find items, preview with thumbnails, save to lists | MVP |
| Downloads | Queue, progress, history, browse downloaded files | MVP |
| Lists | Named identifier collections, import/export, feed operations | MVP |
| Metadata | Browse single items, batch retrieve | MVP (browse), Later (batch, write) |
| Upload | Upload files to archive.org | Later (needs auth) |
| Tasks | IA catalog task viewer/submitter | Later (needs auth) |
| Settings | Connection, download defaults, disk pool | MVP |

"Later" sections are greyed out in sidebar until implemented.

## Section Designs

### Search

Home screen. Users find items on archive.org, preview results, and take action.

```
┌─────────────────────────────────────────────────────────┐
│ Search                                                   │
│                                                          │
│ ┌─────────────────────────────────────┐ ┌─────────────┐ │
│ │ collection:nasa AND mediatype:image │ │   Search     │ │
│ └─────────────────────────────────────┘ └─────────────┘ │
│                                                          │
│ Backend: ● Scrape  ○ Advanced  ○ Full-Text    Fields: ▼  │
│ Sort: ▼ relevance     Results: 12,847 found              │
│                                                          │
│ ┌──────────┬─────────────────────────────────────────┐   │
│ │ [thumb]  │ NASA Images Archive                      │   │
│ │          │ nasa-images · collection · 42,819 files  │   │
│ ├──────────┼─────────────────────────────────────────┤   │
│ │ [thumb]  │ Hubble Space Telescope Image Gallery     │   │
│ │          │ hubble-gallery · image · 1,204 files     │   │
│ └──────────┴─────────────────────────────────────────┘   │
│                                                          │
│ ☐ Select all    [Add to list ▼]  [Download ▼]  [Export]  │
│ Page 1 of 129  [< Prev] [Next >]                         │
└─────────────────────────────────────────────────────────┘
```

**Features:**

- Query field with full IA search syntax
- Backend selector: Scrape (default, cursor), Advanced (page), Full-Text (scroll)
- Fields dropdown: choose returned metadata fields
- Sort dropdown: relevance, date, title, downloads
- Result count display
- Thumbnails via `archive.org/services/img/<identifier>`
- Checkboxes for multi-select
- Actions on selection: add to list, queue for download, export to file
- Export formats: JSONL, CSV, TSV
- Pagination: page-based (Advanced) or "load more" (Scrape/FTS)
- Clicking a result opens Item Detail

**CLI mapping:**

| GUI | CLI |
|---|---|
| Query field | `ia search <query>` |
| Backend selector | `--fts` flag |
| Fields dropdown | `--field` option |
| Sort dropdown | `--sort` option |
| Result count | `--num-found` flag |
| Export | `--itemlist` output |

### Downloads

Central download manager with three sub-views.

**Active Downloads:**

```
┌─────────────────────────────────────────────────────────┐
│ Downloads                                                │
│                                                          │
│ [Active ◉]  [History]  [Downloaded Files]                │
│                                                          │
│ Overall: ━━━━━━━━━╸──────── 45% · 2.3GB/5.1GB · 42MB/s  │
│                                                          │
│ ┌────────────────────────────────────────────────────┐   │
│ │ nasa-images                                    ⏸ ✕ │   │
│ │   14/42 files · 890MB/2.1GB · 31MB/s               │   │
│ │   ━━━━━━━━╸──────────── photo-001.tif  8.1MB/s     │   │
│ │   ━━━━━━━━━━━━━━━━━━━━ photo-002.tif  done         │   │
│ ├────────────────────────────────────────────────────┤   │
│ │ apollo11-photos                                ⏸ ✕ │   │
│ │   2/89 files · 145MB/3.0GB · 11MB/s                │   │
│ │   ━━━╸─────────────── AS11-40-5875.tif  11MB/s     │   │
│ └────────────────────────────────────────────────────┘   │
│                                                          │
│ Disks: /data 450GB free · /backup 1.2TB free             │
│ Queue: 3 items · Jobs: 5/5 active                        │
└─────────────────────────────────────────────────────────┘
```

**History:**

```
┌─────────────────────────────────────────────────────────┐
│ [Active]  [History ◉]  [Downloaded Files]                │
│                                                          │
│ ┌──────────────┬────────┬─────────┬────────┬──────────┐  │
│ │ Item         │ Status │ Files   │ Size   │ Date     │  │
│ ├──────────────┼────────┼─────────┼────────┼──────────┤  │
│ │ nasa-images  │ ✓ Done │ 42/42   │ 2.1GB  │ 10:32am  │  │
│ │ hubble-gal   │ ⚠ Err  │ 1198/12 │ 890MB  │ 9:15am   │  │
│ │ apollo11     │ ✓ Done │ 89/89   │ 3.0GB  │ Yesterday│  │
│ └──────────────┴────────┴─────────┴────────┴──────────┘  │
│                                                          │
│ [Retry Failed]  [Export Log]  [Clear History]            │
└─────────────────────────────────────────────────────────┘
```

**Downloaded Files:**

```
┌─────────────────────────────────────────────────────────┐
│ [Active]  [History]  [Downloaded Files ◉]                │
│                                                          │
│ Browse: /data/downloads                         [Change] │
│                                                          │
│ ┌──────────┬──────────────────────────────────────────┐  │
│ │ [thumb]  │ nasa-images/                             │  │
│ │          │ 42 files · 2.1GB · Downloaded 10:32am    │  │
│ ├──────────┼──────────────────────────────────────────┤  │
│ │ [thumb]  │ apollo11-photos/                         │  │
│ │          │ 89 files · 3.0GB · Downloaded yesterday  │  │
│ └──────────┴──────────────────────────────────────────┘  │
│                                                          │
│ Clicking an item opens Item Detail                       │
└─────────────────────────────────────────────────────────┘
```

**Features:**

- Three sub-tabs: Active (live progress), History (from job log), Downloaded Files (local browser)
- Per-item and per-file progress bars with speed
- Pause/cancel per item
- Disk space display (disk pool integration)
- Concurrency respects global jobs setting
- History powered by job log (JSONL)
- Retry failed, export log, clear history
- Downloaded files browser with thumbnails; clicking opens Item Detail
- Downloaded items auto-added to "Downloaded" list

**Download entry points (multiple paths into Downloads):**

1. Search results → select → "Download"
2. Item Detail → "Download" / "Download selected files"
3. Lists → select list → "Download all"
4. Downloads page → manual identifier entry / "Import list"

**State management:**

- Job log (JSONL): persistent history, survives app restarts
- In-memory: active download queue, current progress
- Config: default output dir, concurrency, disk pool
- Auto-maintained "Downloaded" list: every successful download tracked

**CLI mapping:**

| GUI | CLI |
|---|---|
| Active progress | `ia download` / `--dashboard` |
| History | `ia status` |
| Retry failed | `--retry-failed` |
| Disk space | `--destdir` (disk pool) |
| Concurrency | `-j/--jobs` |
| File filtering | `--glob`, `--format`, `--source` |

### Lists

Named collections of item identifiers. The connective tissue between all operations.

```
┌─────────────────────────────────────────────────────────┐
│ Lists                                                    │
│                                                          │
│ [+ New List]  [Import from File]                         │
│                                                          │
│ ┌──────────────────┬───────┬──────────┬───────────────┐  │
│ │ Name             │ Items │ Created  │ Actions       │  │
│ ├──────────────────┼───────┼──────────┼───────────────┤  │
│ │ NASA Collections │ 847   │ Feb 20   │ [⬇] [📄] [✕] │  │
│ │ Apollo Missions  │ 12    │ Feb 19   │ [⬇] [📄] [✕] │  │
│ │ Downloaded       │ 3     │ auto     │ [⬇] [📄]     │  │
│ └──────────────────┴───────┴──────────┴───────────────┘  │
│                                                          │
│ ─── Selected: NASA Collections (847 items) ───           │
│                                                          │
│ ┌──────────┬──────────────────────────────────────────┐  │
│ │ [thumb]  │ nasa-images                              │  │
│ │          │ collection · 42,819 files                 │  │
│ ├──────────┼──────────────────────────────────────────┤  │
│ │ [thumb]  │ nasa-technical-reports                    │  │
│ │          │ collection · 8,201 files                  │  │
│ └──────────┴──────────────────────────────────────────┘  │
│                                                          │
│ [Download All]  [Fetch Metadata]  [Export]  [Open in Search] │
└─────────────────────────────────────────────────────────┘
```

**Features:**

- Create lists from: search results, manual entry, file import
- Import/export formats: JSONL, CSV, TSV, plain text (one ID per line)
- Per-list actions: download all, fetch metadata, delete
- Batch operations: download, metadata retrieval, export, open in search
- Item preview: selecting a list shows items with thumbnails
- Clicking an item opens Item Detail
- Auto-maintained "Downloaded" list (not deletable)
- Persisted to `lists/` directory in app data folder

**Cross-section integration:**

| From | Action | Result |
|---|---|---|
| Search → Lists | "Add to list" on selected results | Items added to chosen list |
| Lists → Downloads | "Download All" | Items queued in Downloads |
| Lists → Metadata | "Fetch Metadata" | Batch metadata job started |
| Downloads → Lists | Auto-track + export history | "Downloaded" list + new list from log |
| File import → Lists | "Import from File" | New list from file on disk |

### Metadata

Inspect and batch-retrieve item metadata.

**Browse mode:**

```
┌─────────────────────────────────────────────────────────┐
│ Metadata                                                 │
│                                                          │
│ [Browse ◉]  [Batch Retrieve]  [Write (coming soon)]     │
│                                                          │
│ ┌───────────────────────────────────┐                    │
│ │ Enter identifier: nasa-images     │  [Fetch]           │
│ └───────────────────────────────────┘                    │
│                                                          │
│ ┌──────────┬─────────────────────────────────────────┐   │
│ │ [thumb]  │ NASA Images Archive                      │   │
│ │          │ Identifier: nasa-images                  │   │
│ │          │ Mediatype: collection                    │   │
│ │          │ Created: 2009-03-12                      │   │
│ └──────────┴─────────────────────────────────────────┘   │
│                                                          │
│ Metadata (JSON):                                         │
│ ┌────────────────────────────────────────────────────┐   │
│ │ {                                                  │   │
│ │   "metadata": { ... },                             │   │
│ │   "files_count": 42819                             │   │
│ │ }                                                  │   │
│ └────────────────────────────────────────────────────┘   │
│                                                          │
│ [Copy JSON]  [Save to File]  [View Files →]  [Download →]│
└─────────────────────────────────────────────────────────┘
```

**Batch Retrieve mode:**

```
┌─────────────────────────────────────────────────────────┐
│ [Browse]  [Batch Retrieve ◉]  [Write (coming soon)]     │
│                                                          │
│ Source:  ○ Enter identifiers  ● From list  ○ From file   │
│ List:    [NASA Collections ▼]  (847 items)               │
│ Format:  ● JSONL  ○ JSON Array  ○ CSV                    │
│ Output:  [/data/nasa-metadata.jsonl        ]  [Browse]   │
│                                                          │
│ [Start Batch Retrieve]                                   │
│                                                          │
│ ┌────────────────────────────────────────────────────┐   │
│ │ Progress: ━━━━━━━━━╸──────── 312/847 · 4.2 req/s  │   │
│ │ ✓ 310 retrieved · 2 errors · 535 remaining         │   │
│ └────────────────────────────────────────────────────┘   │
│                                                          │
│ [Pause]  [Cancel]  [View Output File]                    │
└─────────────────────────────────────────────────────────┘
```

**Features:**

- Browse mode: single identifier lookup, human-readable summary + raw JSON
- Batch Retrieve: feed from list, file, or manual entry; export to JSONL/JSON/CSV
- Progress tracking with speed and error count
- Pause/cancel for batch operations
- Write mode: placeholder for future metadata editing (needs auth)

**CLI mapping:**

| GUI | CLI |
|---|---|
| Browse single item | `ia metadata <id>` |
| Exists check | `ia metadata --exists <id>` |
| Formats summary | `ia metadata --formats <id>` |
| Batch retrieve | `ia metadata` with batch mode (TBD in ia-core) |

### Item Detail

Universal detail view opened by clicking any item anywhere in the app. Equivalent to `archive.org/details/<identifier>`.

```
┌─────────────────────────────────────────────────────────┐
│ ← Back                          nasa-images              │
│                                                          │
│ ┌────────────┬───────────────────────────────────────┐   │
│ │  [large    │ NASA Images Archive                    │   │
│ │  thumbnail]│ Identifier: nasa-images                │   │
│ │            │ Mediatype:  collection                 │   │
│ │            │ Files:      42,819 (18.2 GB)           │   │
│ └────────────┴───────────────────────────────────────┘   │
│                                                          │
│ [Details ◉]  [Files]  [JSON]                             │
│                                                          │
│ Description:                                             │
│ A comprehensive collection of NASA imagery...            │
│                                                          │
│ Subject:     NASA, space, photography                    │
│ Creator:     National Aeronautics and Space Admin.       │
│ License:     Public Domain                               │
│ Collections: nasa, image                                 │
│                                                          │
│ [Download Item]  [Add to List ▼]  [View on archive.org]  │
└─────────────────────────────────────────────────────────┘
```

**Sub-tabs:**

- **Details**: Human-readable metadata — description, subject, creator, dates, collections
- **Files**: Full file list with filtering (glob, format, source), selectable for download. Maps to `ia list`
- **JSON**: Raw metadata JSON, syntax highlighted

**Features:**

- Large thumbnail from `archive.org/services/img/<identifier>`
- Back navigation returns to previous context (search, lists, downloads)
- Actions: download item, download selected files, add to list, open on archive.org
- Downloaded items easily addable to any list for reuse in other sections

### Settings

Visual form for global configuration options.

```
┌─────────────────────────────────────────────────────────┐
│ Settings                                                 │
│                                                          │
│ Connection                                               │
│   Host:             [archive.org              ]          │
│   User-Agent Suffix:[                         ]          │
│   Insecure (HTTP):  [ ]                                  │
│                                                          │
│ Downloads                                                │
│   Default directory: [/data/downloads       ] [Browse]   │
│   Concurrent jobs:   [5            ] (1-50)              │
│   Auto-retry failed: [ ]                                 │
│   Always save log:   [✓]                                 │
│   Log location:      [~/.local/share/ia/joblog.jsonl]    │
│                                                          │
│ Disk Pool                                                │
│   [+ Add Disk]                                           │
│   /data       450GB free  [✕]                            │
│   /backup     1.2TB free  [✕]                            │
│                                                          │
│ Authentication (coming soon)                             │
│   Status: Not configured                                 │
│                                                          │
│ [Save]  [Reset to Defaults]                              │
└─────────────────────────────────────────────────────────┘
```

Persists settings to config file (compatible with or extending `ia.ini` format).

## MVP Scope (Phase 1)

### Included
1. Search — query, thumbnails, results, export to file/list
2. Downloads — active progress, history (job log), downloaded files browser
3. Lists — named collections, import/export, cross-section integration
4. Metadata — single-item browse with JSON view
5. Item Detail — universal detail page from any context
6. Settings — connection, download defaults, disk pool

### Deferred
- Metadata batch retrieve (needs `ia-core` batch metadata support)
- Metadata write (needs authentication infrastructure)
- Upload (needs authentication)
- Tasks viewer/submitter (needs authentication)
- Download queue persistence across app restarts
- Enhanced offline browsing with full item pages for local content

## Dependencies (ia-gui/Cargo.toml)

- `slint` — GUI framework
- `ia-core` — shared library (workspace dependency)
- `tokio` — async runtime
- `serde` + `serde_json` — serialization
- `anyhow` — error handling
- `dirs` — platform-appropriate data/config directories
- `tracing` — logging (shared with ia-core/ia-cli)
