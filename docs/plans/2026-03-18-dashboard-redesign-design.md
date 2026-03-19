# Dashboard Redesign — Design Spec

**Date**: 2026-03-18
**Status**: Approved
**Scope**: Upload dashboard TUI (`--dashboard` flag)

## Problem

The current upload dashboard is too sparse — there's no compelling reason to use it over regular console output. It lacks interactivity, S3 task visibility, and useful data views. The visual style doesn't spark joy.

## Solution

Redesign the upload dashboard as an interactive, multi-tab mission control with dense data display, vi-like navigation, and archive.org-inspired visual identity.

## Visual Style

### Color Palette
- **Background**: `#1c1c1c` (soft dark, lower contrast than pure black)
- **Text**: `#d4d4d4` primary, `#999` secondary, `#666` muted, `#444` very muted
- **Borders**: `#333` (1px with border-radius — in ratatui: `Block::bordered()` with muted gray style)
- **Panel labels**: Box-drawing text inside bordered blocks (`┌ Label ──┐` / `└──────┘`)

### Accent Colors (Archive.org-Inspired)
- **Maroon** `#8b1a1a` / `#cd5c5c` — section labels, header decorations, active tab background
- **Gold** `#ffd700` — queued items, active/cursor row highlight, rate-limited status
- **Blue** `#5b8dd9` — running status
- **Green** `#3fb950` — completed/success
- **Red** `#f85149` — errors

### Layout Elements
- Centered decorative header: `━━━ ia upload ━━━ 3/10 items ━━━ 4.2 GB ━━━ 12.5 MB/s ━━━ ETA 28m ━━━`
- Tab bar below header: active tab has maroon background pill `❶ Upload`, inactive tabs in muted gray
- Context-sensitive key hints in footer: maroon `[key]` labels with gray descriptions
- Sparkline throughput graph in muted `#444` (background texture, not focal point)
- Active item highlight: gold left-border + subtle gold background tint

## Tab Structure

### ❶ Upload Tab (Main Dashboard)

The primary view showing upload progress.

**Panels:**
1. **S3 Tasks** — queued/running/errors from tasks API, global task count in top-right corner of panel border. When rate-limited: `⏸ rate-limited` replaces `✗ 0 errors` (show both if errors > 0).
2. **Items** — per-item status list with completion icons (✓ done, ▸ active, · pending), bytes, file counts. Scrollable.
3. **Transfers** — active file uploads with progress bars and speed. Rate-limited files show `⏸ rate-limited` instead of progress bar. Below: recently completed files.
4. **Completed** — last N completed files.
5. **Throughput** — muted sparkline with peak/avg stats in panel border.

**Keys:** `j/k` scroll focused panel, `Tab` cycle panel focus, `Enter` open item history in browser.

### ❷ Tasks Tab

Browsable, searchable view of S3 catalog tasks.

**Panels:**
1. **S3 Summary** — same queued/running/errors counts as Upload tab, plus user/global toggle indicator and global count.
2. **Task List** — scrollable table with columns: SUBMITTED, IDENTIFIER, CMD, STATUS. Highlighted cursor row with gold left-border.

**Keys:** `j/k` scroll, `/` search by identifier, `u` toggle user tasks vs global, `Enter` open `https://archive.org/history/<id>` in browser.

**Polling:** Every 15 seconds (vs 60s on other tabs).

### ❸ Log Tab

Human-readable view of the joblog.

**Panels:**
1. **Job Log** — scrollable table with columns: TIME, ITEM, FILE, STATUS. Status color-coded: green `✓ uploaded`, gray `– skipped`, red `✗ failed` (with error code). Entry count in panel border. Position indicator in bottom border (`showing 115-127`).

**Keys:** `j/k` scroll, `G` jump to end, `gg` jump to top, `/` search, `f` filter by status (uploaded/skipped/failed).

**Live updates:** New joblog entries appear in real-time as uploads complete.

### ❹ Errors Tab

Upload errors and (rarely) S3 task failures.

**Panels:**
1. **Upload Errors** — scrollable table with columns: TIME, ITEM, FILE, ERROR. Error count in panel border.
2. **S3 Task Errors** — hidden by default, only shown if S3 task errors exist (extremely rare). Same table format.

**Keys:** `j/k` scroll, `Enter` expand full error detail.

## Architecture

### Tab Framework
- `TabView` trait with methods: `draw(&self, frame, area)`, `handle_key(&mut self, code, modifiers) -> bool`, `tick(&mut self)`
- `UploadDashboard` implements the existing `Dashboard` trait (from `framework.rs`), wrapping the tabs internally
- `Dashboard::draw` renders the shared header/footer, then delegates the content area to the active `TabView::draw`
- `Dashboard::handle_key` handles global keys (`1-4` tab switch, `q` quit, `?` help), then routes remaining keys to the active tab's `handle_key`
- `is_done` and `quit_requested` remain on the `Dashboard` impl (not on individual tabs)
- The existing `run_dashboard_sync` event loop drives the outer dashboard unchanged — it calls `Dashboard::draw` which internally calls `TabView::tick` + `TabView::draw`
- Tab switching via `1-4` keys handled at dashboard level before routing
- Shared header and footer rendered by dashboard, not individual tabs
- Framework designed for reuse by download dashboard (and potentially standalone tasks dashboard). Download dashboard migration is a **future milestone**, not in scope here.

### Upload Tab Panel Layout
The Upload tab uses a vertical stack with conditional visibility:

| Panel | Constraint | Visibility |
|-------|-----------|------------|
| S3 Tasks | `Length(5)` | Always |
| Items + Transfers (side by side) | `Min(8)` | Always — splits horizontal space 50/50 |
| Throughput | `Length(4)` | Always (after first throughput sample) |

Items and Transfers are rendered side-by-side within a single horizontal split. Completed files are shown at the bottom of the Transfers panel (below a divider line), not as a separate panel — fills available space, up to 10 entries.

**Focusable panels:** Items and Transfers only. S3 Tasks and Throughput are display-only (not in the `Tab` focus cycle).

### Shared State
- `UploadTuiState` — existing upload progress state (items, files, bytes, throughput)
- `S3TaskState` — task counts (queued/running/errors), global count, full task list (for Tasks tab), user email for filtering, `last_polled: Instant` for "polled Ns ago" display
- `JoblogState` — parsed log entries as `Vec<LogEntry>`, tail-reads new entries periodically (every 1-2s, not every tick — uses file seek from last known position to avoid re-reading)
- `ErrorState` — upload errors (existing `failed_files`) + S3 task errors

### Data Flow

**Upload progress:** Unchanged — async upload tasks send `UploadProgress` events, TUI state processes on each tick (100ms).

**S3 task polling:**
- Poll every 15s
- User tasks: `get_tasks(&TasksQuery { args: "*s3-put*", submitter: <email> })` — returns catalog entries including task_id, identifier, cmd, status, submittime
- Global count: separate query without submitter filter, with `limit: 0` + `summary: true` to get just the total count without fetching entries
- Tasks tab stores full user task list for display; when toggling to global via `u`, re-queries without submitter filter (with `limit: 100` to cap result size)
- Other tabs use summary counts only
- The `args: "*s3-put*"` filter is intentional — this dashboard is for upload monitoring, so we only show S3-put tasks

**Joblog:**
- On startup: read existing joblog, parse into `LogEntry` structs (time, item, file, status, error)
- Tail behavior: track file position with `seek`, check for new lines every 1-2 seconds (not every 100ms tick), parse and append
- No JSON in display — human-readable table format

**Rate limit signaling:**
- S3 Tasks panel: `⏸ rate-limited` indicator when any item is rate-limited
- Transfers panel: affected file shows `⏸ rate-limited` instead of progress bar
- When rate limiting ends, indicators clear automatically

**Browser integration:**
- Use `open` crate (`open::that()`) for cross-platform browser opening — add `open` as a dependency to `ia-cli/Cargo.toml`
- Triggered by `Enter` key on highlighted item in Items panel or Tasks tab
- Opens `https://archive.org/history/{id}`

### Key Sequence Handling
- Single-key bindings (`j`, `k`, `G`, `/`, `f`, `u`, `q`, `?`, `Enter`, `Tab`, `1-4`) are handled directly from `KeyCode`
- `gg` (go to top): implemented via a `pending_g: bool` state flag. First `g` sets the flag, second `g` within 500ms triggers the action. Any other key (or timeout) clears the flag. This is lightweight — no full key sequence state machine needed.

### Search and Filter
- `/` triggers search mode: a text input line appears at the bottom of the panel (vim-style command line). Typed characters filter the visible list in real-time. `Enter` confirms and returns to normal mode with the filter active. `Escape` cancels and clears the filter.
- `f` on Log tab: cycles through filter states: All → Uploaded → Skipped → Failed → All. Current filter shown in the panel border (e.g., `┌ Job Log [filter: failed] ──────┐`).
- During search mode, `j/k` and other navigation keys are consumed by the text input. Only `Enter` and `Escape` exit search mode.

### Help Overlay
- Popup overlay (centered `Block` rendered over existing content via `Clear` widget) triggered by `?`, dismissed on any key
- Context-sensitive: shows only keys relevant to current tab
- Global keys always shown at top

### Error Detail Expansion
- `Enter` on an error row in the Errors tab expands it inline — the row grows to show the full error message, HTTP status, response body snippet (if available). Press `Enter` again or `Escape` to collapse.

### Color Fallback
- Colors use `Color::Rgb()` for true-color terminals
- For 256-color terminals, degrade gracefully: background uses `Color::Indexed(234)` (`#1c1c1c`), borders use `Color::Indexed(236)` (`#333`), etc.
- Detection: check `$COLORTERM` env var for `truecolor`/`24bit`; fall back to indexed colors otherwise

## S3 Tasks Panel — Detail

The S3 Tasks panel appears on the Upload tab and as a summary on the Tasks tab.

**Normal state:**
```
┌ S3 Tasks ──────────────────────── Global: 847 ┐
  ⧖ Queued: 23   ↻ Running: 4   ✗ Errors: 0
└──────────────────────── polled 12s ago ────────┘
```

**Rate-limited state:**
```
┌ S3 Tasks ──────────────────────── Global: 847 ┐
  ⧖ Queued: 23   ↻ Running: 4   ⏸ rate-limited
└──────────────────────── polled 12s ago ────────┘
```

**Rate-limited with errors:**
```
┌ S3 Tasks ──────────────────────── Global: 847 ┐
  ⧖ Queued: 23   ↻ Running: 4   ✗ Errors: 1   ⏸ rate-limited
└──────────────────────── polled 12s ago ────────┘
```

## Testing Strategy

- Unit tests for each tab's state management (key handling, scrolling, filtering)
- Unit tests for S3 task polling state (user/global toggle, count aggregation)
- Unit tests for joblog parsing and tail behavior
- Integration tests for tab switching and shared state updates
- Existing upload TUI tests adapted for new tab structure

## Migration

- The existing `UploadDashboard` in `tui/upload_app.rs` becomes the Upload tab
- Existing `upload_ui.rs` panel rendering functions are refactored into the Upload tab's draw method
- S3 task polling (already in `upload_app.rs`) is extracted into shared `S3TaskState`
- New files: `tui/tabs.rs` (TabView trait), `tui/tasks_tab.rs`, `tui/log_tab.rs`, `tui/errors_tab.rs`
