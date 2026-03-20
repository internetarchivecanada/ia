# Upload Dashboard Redesign — Design Spec

**Date:** 2026-03-19
**Status:** Draft

## Context

The upload dashboard's main Upload tab currently splits the screen into side-by-side Items and Transfers panels. This layout doesn't surface enough useful information to justify the split — items and their files are disconnected, and key stats (completed count, throughput, ETA) are buried in a thin header line. This redesign unifies the Upload tab into a single expandable tree view with better information density.

## Layout

```
╺━━ ia upload ━━╸                             ← header (1 line)
❶ Upload  ❷ Tasks  ❸ Log  ❹ Errors            ← tab bar (1 line)
                                               ← spacer (1 line)
┌─ S3 Tasks ──────────┬─ Progress ────────────┐
│ ⧖ 3   ↻ 2   ✗ 0    │ ◫ 3/10 items  ≡ 28/45 │
│ ⊕ 156 global [2s]   │ ↑ 1.2 GiB    ⧗ ~28m   │
└──────────────────────┴───────────────────────┘
┌─ Items ─────────────────────────────────────┐
│ ▾ item-003  2/8 files · 340 MiB   (expand) │
│   photo1.jpg ████░░░ 45% 8.2 MiB/s 12/26M  │
│   doc.pdf    ██░░░░░ 28% 4.1 MiB/s 3/11M   │
│   ✓ meta.xml · 12 KiB                      │
│   ⏫ old.pdf · skipped (exists)             │
│   · large.zip · 1.2 GiB                    │
│ ▸ item-004  1/4 · 80 MiB          (active) │
│   intro.mp4  ██░░░░░ 15% 6.0 MiB/s 9/60M   │
│ ▸ item-001  ✓ 5/5 · 220 MiB · 1m12s (done) │
│ ▸ item-005  · 0/6 files           (pending) │
└─────────────────────────────────────────────┘
▁▂▃▅▇█▇▅▃▂▁▂▃▅  12.4 MiB/s  ▅▃▂▁▂▃▅▇█▇▅▃▂▁▂
←/→ expand   j/k scroll   /search   ? help   q
```

### Vertical layout breakdown

| Row | Constraint | Content |
|-----|-----------|---------|
| Header | `Length(1)` | `╺━━ ia upload ━━╸` (centered, unchanged from current) |
| Tab bar | `Length(1)` | Tab indicators, active tab highlighted |
| Spacer | `Length(1)` | Empty |
| Top panes | `Length(4)` | S3 Tasks + Progress, horizontal 50/50 (2 content lines + top/bottom border) |
| Items tree | `Min(8)` | Unified expandable item/file tree (fills remaining space) |
| Sparkline | `Length(2)` | Split sparkline with centered speed |
| Footer | `Length(1)` | Context-sensitive key hints + elapsed |

### Top panes (side-by-side)

**S3 Tasks pane (left 50%):**
- Line 1: `⧖ N` (queued, gold) `↻ N` (running, green) `✗ N` (errors, red) — icon+number only, no labels
- Line 2: `⊕ N global  [Xs ago]` — dimmed, smaller text

**Progress pane (right 50%):**
- 2×2 grid layout:
  - Top row: `◫ X/Y items` (green) `≡ X/Y files` (blue)
  - Bottom row: `↑ X.X GiB` (white) `⧗ ~Xm` (muted)
- No speed here (speed is in the sparkline bar)
- No overall progress bar (total batch size is unknown)

### Items tree

Single scrollable list replacing the old Items + Transfers panels.

**Item row format (collapsed):**
```
▸ identifier  X/Y files · Z MiB [· elapsed]
```

**Item row format (expanded):**
```
▾ identifier  X/Y files · Z MiB
  filename.ext  ████░░░ 45% · 8.2 MiB/s · 12/26 MiB
  ✓ completed.txt · 340 KiB
  ⏫ skipped.pdf · skipped (exists)
  · pending.zip · 1.2 GiB
```

**Collapsed active items** — even when collapsed, show the current file's progress on the next line:
```
▸ item-004  1/4 files · 80 MiB
  intro.mp4  ██░░░░░ 15% · 6.0 MiB/s · 9/60 MiB
```

**File status indicators:**
- Active upload: progress bar + percentage + speed + bytes
- Completed: `✓ filename · size` (green)
- Skipped: `⏫ filename · skipped (exists)` (gold)
- Pending: `· filename · size` (muted)
- Rate-limited: `⏸ filename · rate-limited` (gold)
- Failed: `✗ filename · error message` (red)

**Item status colors:**
- Active (uploading/verifying): gold text + gold `▸`/`▾`
- Completed: green text + `✓` in summary
- Failed: red text + `✗` in summary
- Pending: muted/dim text

**Completed item summary:**
```
▸ item-001  ✓ 5/5 · 220 MiB · 1m12s
```
When a resume skipped files: `▸ item-009  ✓ 4/6 · 2 skipped · 95 MiB · 30s`

### Item ordering

Items maintain their original input order (from spreadsheet/CLI), with one exception: **actively-uploading items are pinned to the top** of the list. Once an item completes, it returns to its natural position in the original order.

**Cursor follows item:** The cursor tracks the selected item by identifier, not by index. When items shift due to pinning/unpinning, the cursor stays on the same item. The `cursor_identifier` field is used to restore the cursor position after re-ordering.

### Expand/collapse

- `→` (Right arrow): expand the selected item to show all files
- `←` (Left arrow): collapse the selected item
- Active items auto-expand when they start uploading
- Completed and pending items default to collapsed
- Expanding a completed item shows the files that were uploaded

### Sparkline

Short 1-line pane at the bottom. The speed number is **centered**, with sparkline bars flowing on both sides:
```
▁▂▃▅▇█▇▅▃▂▁▂▃▅  12.4 MiB/s  ▅▃▂▁▂▃▅▇█▇▅▃▂▁▂
```

Implementation: ratatui's `Sparkline` widget doesn't support a centered gap, so render the sparkline characters manually using block elements (`▁▂▃▄▅▆▇█`). Split the `ThroughputTracker.history()` data into left and right halves, map values to block chars, and compose a `Line` with three `Span`s: left bars, centered speed label, right bars.

### Search

Add `/` search to the Upload tab, reusing the existing `SearchState` from `tui/search.rs`.

- `/` activates search mode — a search input appears in the footer area
- Typing filters/highlights matching items by identifier
- `Enter` confirms the search query
- `Esc` cancels and clears the query
- `n` jumps to the next matching item
- `N` jumps to the previous matching item
- Matches are highlighted in the items list; non-matching items stay visible but dimmed

**Note:** `n`/`N` match cycling is new — `SearchState` currently only provides `matches()`. The cycling logic (maintaining a match index and advancing/reversing through filtered results) lives in the upload tab, not in `SearchState`.

### Single-item uploads

Same layout as batch, with one always-expanded item. The Progress pane shows `◫ 0/1 items`, file counts, etc. The tree has a single entry. Consistent experience regardless of batch size.

## State changes

### `UploadTab` (upload_tab.rs)

Replace `FocusPanel` enum and dual cursors with:

```rust
pub struct UploadTab {
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,
    cursor: usize,                          // item-level cursor (indexes into item list, not visible rows)
    expanded: HashSet<String>,              // identifiers of expanded items
    search: SearchState,                    // vim-style search
    search_matches: Vec<usize>,             // indices of matching items
    search_match_cursor: usize,             // current match index for n/N
    status_message: Option<(String, Instant)>,
    cursor_identifier: Option<String>,
}
```

**Removed:** `focused_panel`, `items_cursor`, `transfers_cursor`

### `UploadTuiState` (upload_app.rs)

Add a method to get files for a specific item (both active and completed/skipped):

```rust
impl UploadTuiState {
    /// Get all file progress entries for a given identifier.
    /// Returns active files + a synthetic list of completed/skipped/pending files.
    pub fn files_for_item(&self, identifier: &str) -> Vec<FileDisplayEntry> { ... }
}
```

We also need to track completed files per-item (not just a global `completed_files` deque). Add:

```rust
pub struct UploadItemState {
    // ... existing fields ...
    pub completed_file_names: Vec<String>,       // names of completed files for this item
    pub skipped_file_names: Vec<String>,          // names of skipped files
    pub failed_file_names: Vec<(String, String)>, // (name, error)
    pub known_file_names: Vec<String>,            // all file names seen (from Verifying events)
}
```

**Note on file name tracking:** The `Enumerated` event only carries `files_count` and `bytes_total` — it does **not** include individual file names. File names first appear in `Verifying` events (which carry the `key` field). The `known_file_names` list is built incrementally as `Verifying` events arrive. Pending files are derived as: names in `known_file_names` that aren't in `completed_file_names`, `skipped_file_names`, `failed_file_names`, or `active_files`.

The `update()` method already processes events per-item — extend it to record file names on Verifying/Complete/Skipped/Failed events.

### `dashboard.rs`

Update `is_tab_consuming_input()` to include Upload tab:

```rust
fn is_tab_consuming_input(&self) -> bool {
    match self.active_tab {
        TabId::Upload => self.upload_tab.search.is_active(),
        TabId::Tasks => self.tasks_tab.search.is_active(),
        TabId::Log => self.log_tab.search.is_active(),
        _ => false,
    }
}
```

### `widgets.rs`

Add a `draw_split_sparkline()` function that renders the sparkline bars split around a centered speed label.

Add a `draw_progress_panel()` function for the 2×2 progress grid (or inline it in the upload tab draw).

The existing `draw_s3_panel()` may need adjustment to render compact icons without labels.

## Keyboard handling

| Key | Action |
|-----|--------|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `→` / `l` | Expand selected item |
| `←` / `h` | Collapse selected item |
| `Enter` | Open selected item on archive.org |
| `/` | Activate search input |
| `n` | Next search match |
| `N` | Previous search match |
| `Esc` | Cancel search |
| `p` | Pause/resume uploads |
| `r` | Refresh S3 tasks |
| `?` | Toggle help overlay |
| `q` | Quit |

**Removed:** `Tab` key for panel cycling (no longer needed with unified tree)

## Files to modify

| File | Change |
|------|--------|
| `ia-cli/src/tui/upload_tab.rs` | **Major rewrite**: replace dual-panel layout with tree view, add expand/collapse, search, new rendering |
| `ia-cli/src/tui/upload_app.rs` | Add per-item file tracking (completed/skipped/pending names), `files_for_item()` method |
| `ia-cli/src/tui/dashboard.rs` | Add Upload tab to `is_tab_consuming_input()` |
| `ia-cli/src/tui/widgets.rs` | Add `draw_split_sparkline()`, possibly compact S3 panel variant, progress panel |
| `ia-cli/src/tui/help.rs` | Update Upload tab help overlay with new keybindings |

## What stays the same

- Other tabs (Tasks, Log, Errors) — unchanged
- `UploadTuiState` core data model (items, active_files, throughput) — extended, not replaced
- `ThroughputTracker` — reused, rendered differently
- S3 task polling logic — unchanged
- `SearchState` / `FilterCycle` — reused from `tui/search.rs`
- Dashboard framework, `MultiTabDashboard` structure — minimal changes
- `UploadProgress` events from `ia-core` — unchanged

## Testing

- Update existing `upload_tab` tests (tab cycling → removed, cursor clamping → adjusted for tree)
- Add tests for expand/collapse state management
- Add tests for search integration (`/`, `n`/`N`, `Esc`)
- Add tests for item pinning (active items at top, completed return to position)
- Add tests for `files_for_item()` method
- `test_render_does_not_panic` in dashboard.rs stays — verifies new layout renders without panicking
- Manual testing: run `ia upload --dashboard` with a batch upload to verify visual correctness

## Verification

1. `just ci` passes (fmt, clippy, test, doc)
2. `cargo test -p ia-cli` — all TUI tests pass
3. Manual: `ia upload <id> <file> --dashboard` — single item, tree with one expanded item
4. Manual: `ia upload import <spreadsheet> --dashboard` — batch, verify pinning, expand/collapse, search
5. Verify expand/collapse with ←/→ arrows
6. Verify `/` search highlights and `n`/`N` cycles matches
7. Verify sparkline renders correctly with centered speed
