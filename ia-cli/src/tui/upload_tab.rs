//! Upload tab implementation for the multi-tab dashboard.
//!
//! Renders a unified expandable tree view of items and their files, with
//! compact S3 + Progress top panes, a split sparkline, and `/` search.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use ia_core::upload::UploadProgressStatus;

use super::s3_state::S3TaskState;
use super::search::SearchState;
use super::tab::TabView;
use super::theme::Theme;
use super::upload_app::{FileDisplayStatus, UploadItemStatus, UploadTuiState};
use super::widgets;

// ---------------------------------------------------------------------------
// Upload tab
// ---------------------------------------------------------------------------

/// Upload tab state: unified tree view with expand/collapse, search, and
/// item pinning.
#[derive(Debug)]
pub struct UploadTab {
    upload_state: Arc<Mutex<UploadTuiState>>,
    #[allow(dead_code)] // Retained for dashboard construction; S3 panes now rendered by dashboard.
    s3_state: Arc<Mutex<S3TaskState>>,
    /// Item-level cursor index (into the ordered items list).
    pub cursor: usize,
    /// Set of identifiers whose file lists are expanded.
    pub expanded: HashSet<String>,
    /// Vim-style search input.
    pub search: SearchState,
    /// Indices (into ordered items) that match the current search query.
    search_matches: Vec<usize>,
    /// Current position within `search_matches` for n/N cycling.
    search_match_cursor: usize,
    /// A URL opened via Enter, shown in the footer for 5 seconds.
    status_message: Option<(String, Instant)>,
    /// Full identifier of the item under the cursor (for reorder stability).
    cursor_identifier: Option<String>,
    /// Items that were auto-expanded by tick (not manually). Prevents tick from
    /// re-expanding items after the user manually collapses them.
    auto_expanded: HashSet<String>,
    /// Items the user explicitly expanded with →/l. Tick never touches this set,
    /// so manual expansion persists until the user collapses with ←/h.
    manually_expanded: HashSet<String>,
    /// Shared pause flag from the dashboard.
    paused: Arc<AtomicBool>,
    /// First 'g' press for gg detection (jump to top).
    pending_g: bool,
    /// When the first 'g' was pressed (500ms timeout for gg).
    pending_g_at: Instant,
}

// ---------------------------------------------------------------------------
// Free helper functions (avoid borrow conflicts with &self + lock guard)
// ---------------------------------------------------------------------------

/// Return item indices in display order: active items pinned to top,
/// rest in original order.
fn ordered_items(state: &UploadTuiState) -> Vec<usize> {
    let mut active: Vec<usize> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    for (i, item) in state.items.iter().enumerate() {
        if matches!(
            item.status,
            UploadItemStatus::Uploading
                | UploadItemStatus::Verifying
                | UploadItemStatus::RateLimited
        ) {
            active.push(i);
        } else {
            rest.push(i);
        }
    }
    active.extend(rest);
    active
}

/// Compute search match indices from the ordered item list.
fn compute_matches(search: &SearchState, state: &UploadTuiState) -> Vec<usize> {
    let ordered = ordered_items(state);
    ordered
        .iter()
        .enumerate()
        .filter(|&(_, &item_idx)| search.matches(&state.items[item_idx].identifier))
        .map(|(display_idx, _)| display_idx)
        .collect()
}

impl UploadTab {
    /// Create a new Upload tab with the given shared state.
    pub fn new(
        upload_state: Arc<Mutex<UploadTuiState>>,
        s3_state: Arc<Mutex<S3TaskState>>,
        paused: Arc<AtomicBool>,
    ) -> Self {
        Self {
            upload_state,
            s3_state,
            cursor: 0,
            expanded: HashSet::new(),
            search: SearchState::new(),
            search_matches: Vec::new(),
            search_match_cursor: 0,
            status_message: None,
            cursor_identifier: None,
            auto_expanded: HashSet::new(),
            manually_expanded: HashSet::new(),
            paused,
            pending_g: false,
            pending_g_at: Instant::now(),
        }
    }

    /// Recompute search matches from current state.
    fn recompute_search(&mut self) {
        if let Ok(state) = self.upload_state.lock() {
            self.search_matches = compute_matches(&self.search, &state);
        }
    }

    /// Jump cursor to the next search match.
    fn jump_to_next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        // Find first match after current cursor.
        if let Some(pos) = self.search_matches.iter().position(|&m| m > self.cursor) {
            self.search_match_cursor = pos;
        } else {
            // Wrap around.
            self.search_match_cursor = 0;
        }
        self.cursor = self.search_matches[self.search_match_cursor];
    }

    /// Sync `cursor_identifier` to match the current `cursor` position.
    fn sync_cursor_identifier(&mut self) {
        if let Ok(state) = self.upload_state.lock() {
            let ord = ordered_items(&state);
            if let Some(&item_idx) = ord.get(self.cursor) {
                self.cursor_identifier = Some(state.items[item_idx].identifier.clone());
            }
        }
    }

    /// Jump cursor to the previous search match.
    fn jump_to_prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        // Find last match before current cursor.
        if let Some(pos) = self.search_matches.iter().rposition(|&m| m < self.cursor) {
            self.search_match_cursor = pos;
        } else {
            // Wrap around to last match.
            self.search_match_cursor = self.search_matches.len() - 1;
        }
        self.cursor = self.search_matches[self.search_match_cursor];
    }
}

// ---------------------------------------------------------------------------
// TabView impl
// ---------------------------------------------------------------------------

impl TabView for UploadTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let upload = match self.upload_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        // Vertical layout: Items tree (fill) | Sparkline (2)
        // S3+Progress panes are rendered by dashboard.rs above this area.
        let chunks = Layout::vertical([Constraint::Min(8), Constraint::Length(2)]).split(area);

        // ── Items tree ────────────────────────────────────────────────
        draw_items_tree(frame, chunks[0], theme, &upload, self);

        // ── Split sparkline ───────────────────────────────────────────
        let speed = format!(
            "{}/s",
            widgets::format_bytes(upload.throughput.throughput() as u64)
        );

        // Use both lines of the sparkline area
        let spark_rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(chunks[1]);
        widgets::draw_split_sparkline(
            frame,
            spark_rows[0],
            theme,
            upload.throughput.history(),
            &speed,
        );

        // Search input line (or empty)
        if self.search.is_active() {
            let search_line = Line::from(vec![
                Span::styled("/", Style::default().fg(theme.gold)),
                Span::styled(
                    self.search.query().to_string(),
                    Style::default().fg(theme.text),
                ),
                Span::styled("\u{2588}", Style::default().fg(theme.gold)), // cursor block
            ]);
            frame.render_widget(Paragraph::new(search_line), spark_rows[1]);
        } else if !self.search.query().is_empty() {
            let match_info = if self.search_matches.is_empty() {
                "no matches".to_string()
            } else {
                format!(
                    "{}/{} matches",
                    self.search_match_cursor + 1,
                    self.search_matches.len()
                )
            };
            let search_line = Line::from(vec![
                Span::styled("/", Style::default().fg(theme.text_muted)),
                Span::styled(
                    self.search.query().to_string(),
                    Style::default().fg(theme.text_muted),
                ),
                Span::styled(
                    format!("  ({match_info})"),
                    Style::default().fg(theme.text_very_muted),
                ),
            ]);
            frame.render_widget(Paragraph::new(search_line), spark_rows[1]);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // When search input is active, route chars there.
        if self.search.is_active() {
            match code {
                KeyCode::Esc => {
                    self.search.cancel();
                    self.search_matches.clear();
                    self.search_match_cursor = 0;
                }
                KeyCode::Enter => {
                    self.search.confirm();
                    self.recompute_search();
                    if !self.search_matches.is_empty() {
                        self.search_match_cursor = 0;
                        self.cursor = self.search_matches[0];
                    }
                }
                KeyCode::Backspace => {
                    self.search.backspace();
                    self.recompute_search();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.recompute_search();
                }
                _ => {}
            }
            return true;
        }

        // Normal mode key handling.
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.pending_g = false;
                if let Ok(state) = self.upload_state.lock() {
                    let max = state.items.len().saturating_sub(1);
                    self.cursor = self.cursor.saturating_add(1).min(max);
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        self.cursor_identifier = Some(state.items[item_idx].identifier.clone());
                    }
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.pending_g = false;
                self.cursor = self.cursor.saturating_sub(1);
                if let Ok(state) = self.upload_state.lock() {
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        self.cursor_identifier = Some(state.items[item_idx].identifier.clone());
                    }
                }
                true
            }
            KeyCode::Char('G') if modifiers.contains(KeyModifiers::SHIFT) => {
                self.pending_g = false;
                if let Ok(state) = self.upload_state.lock() {
                    let max = state.items.len().saturating_sub(1);
                    self.cursor = max;
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        self.cursor_identifier = Some(state.items[item_idx].identifier.clone());
                    }
                }
                true
            }
            KeyCode::Char('g') => {
                if self.pending_g
                    && self.pending_g_at.elapsed() < std::time::Duration::from_millis(500)
                {
                    // gg: jump to top
                    self.cursor = 0;
                    self.pending_g = false;
                    self.sync_cursor_identifier();
                } else {
                    self.pending_g = true;
                    self.pending_g_at = Instant::now();
                }
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.pending_g = false;
                if let Ok(state) = self.upload_state.lock() {
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        let id = state.items[item_idx].identifier.clone();
                        self.expanded.insert(id.clone());
                        self.manually_expanded.insert(id);
                    }
                }
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.pending_g = false;
                if let Ok(state) = self.upload_state.lock() {
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        let id = &state.items[item_idx].identifier;
                        self.expanded.remove(id);
                        self.manually_expanded.remove(id);
                        // Remove from auto_expanded to prevent re-auto-expanding
                        self.auto_expanded.remove(id);
                    }
                }
                true
            }
            KeyCode::Char('/') => {
                self.pending_g = false;
                self.search.activate();
                true
            }
            KeyCode::Char('n') => {
                self.pending_g = false;
                if !self.search.query().is_empty() {
                    self.recompute_search();
                    self.jump_to_next_match();
                    self.sync_cursor_identifier();
                }
                true
            }
            KeyCode::Char('N') => {
                self.pending_g = false;
                if !self.search.query().is_empty() {
                    self.recompute_search();
                    self.jump_to_prev_match();
                    self.sync_cursor_identifier();
                }
                true
            }
            KeyCode::Esc => {
                self.pending_g = false;
                if !self.search.query().is_empty() {
                    self.search.cancel();
                    self.search_matches.clear();
                    self.search_match_cursor = 0;
                    return true;
                }
                false
            }
            KeyCode::Enter => {
                self.pending_g = false;
                if let Ok(state) = self.upload_state.lock() {
                    let ord = ordered_items(&state);
                    if let Some(&item_idx) = ord.get(self.cursor) {
                        let url = format!(
                            "https://archive.org/details/{}",
                            state.items[item_idx].identifier
                        );
                        let _ = open::that(&url);
                        self.status_message = Some((url, Instant::now()));
                    }
                }
                true
            }
            _ => {
                self.pending_g = false;
                false
            }
        }
    }

    fn tick(&mut self) {
        // Clear status message after 5 seconds.
        if let Some((_, ts)) = &self.status_message {
            if ts.elapsed().as_secs() >= 5 {
                self.status_message = None;
            }
        }

        // Clear pending_g after 500ms timeout.
        if self.pending_g && self.pending_g_at.elapsed() > std::time::Duration::from_millis(500) {
            self.pending_g = false;
        }

        // Collect data from the lock, then mutate self outside the lock.
        let tick_data = self.upload_state.lock().ok().map(|state| {
            // Collect identifiers of active items for auto-expand.
            let active_ids: Vec<String> = state
                .items
                .iter()
                .filter(|item| {
                    matches!(
                        item.status,
                        UploadItemStatus::Uploading
                            | UploadItemStatus::Verifying
                            | UploadItemStatus::RateLimited
                    )
                })
                .map(|item| item.identifier.clone())
                .collect();

            // Collect identifiers of completed items for auto-collapse.
            let completed_ids: Vec<String> = state
                .items
                .iter()
                .filter(|item| {
                    matches!(
                        item.status,
                        UploadItemStatus::Complete | UploadItemStatus::Failed(_)
                    )
                })
                .map(|item| item.identifier.clone())
                .collect();

            // Resolve cursor position.
            let ord = ordered_items(&state);
            let resolved_cursor = if let Some(ref id) = self.cursor_identifier {
                ord.iter()
                    .position(|&idx| state.items[idx].identifier == *id)
            } else {
                None
            };
            let max = ord.len().saturating_sub(1);
            let cursor_id = ord
                .get(resolved_cursor.unwrap_or(self.cursor.min(max)))
                .map(|&idx| state.items[idx].identifier.clone());

            (active_ids, completed_ids, resolved_cursor, max, cursor_id)
        });

        if let Some((active_ids, completed_ids, resolved_cursor, max, cursor_id)) = tick_data {
            // Auto-expand newly active items (only if not already tracked).
            for id in &active_ids {
                if !self.auto_expanded.contains(id) {
                    self.expanded.insert(id.clone());
                    self.auto_expanded.insert(id.clone());
                }
            }
            // Auto-collapse completed items that were auto-expanded (not manually).
            for id in &completed_ids {
                if self.auto_expanded.remove(id) && !self.manually_expanded.contains(id) {
                    self.expanded.remove(id);
                }
            }
            if let Some(pos) = resolved_cursor {
                self.cursor = pos;
            } else {
                self.cursor = self.cursor.min(max);
            }
            self.cursor_identifier = cursor_id;
        }
    }

    fn status_text(&self) -> Option<&str> {
        if let Some((url, _)) = &self.status_message {
            return Some(url.as_str());
        }
        self.cursor_identifier.as_deref()
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        if self.search.is_active() {
            vec![
                ("Enter", "confirm"),
                ("Esc", "cancel"),
                ("?", "help"),
                ("q", "quit"),
            ]
        } else {
            vec![
                ("\u{2190}/\u{2192}", "expand"),
                ("j/k", "scroll"),
                ("/", "search"),
                ("Enter", "open"),
                ("p", "pause"),
                ("r", "refresh"),
                ("?", "help"),
                ("q", "quit"),
            ]
        }
    }
}

// ---------------------------------------------------------------------------
// Items tree rendering
// ---------------------------------------------------------------------------

/// Render the unified items tree view with expandable file lists.
fn draw_items_tree(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    state: &UploadTuiState,
    tab: &UploadTab,
) {
    let block = Block::default()
        .title(Span::styled(
            " Items ",
            Style::default().fg(theme.maroon_bright),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height as usize;
    if visible_height == 0 {
        return;
    }

    let ordered = ordered_items(state);
    let has_search_query = !tab.search.query().is_empty();

    // Build all lines and track which row the cursor item starts at.
    // Each entry: (Line, is_cursor_item_row)
    let mut all_rows: Vec<(Line<'_>, bool)> = Vec::new();

    for (display_idx, &item_idx) in ordered.iter().enumerate() {
        let item = &state.items[item_idx];
        let is_cursor = display_idx == tab.cursor;
        let is_active = matches!(
            item.status,
            UploadItemStatus::Uploading
                | UploadItemStatus::Verifying
                | UploadItemStatus::RateLimited
        );
        let is_expanded = tab.expanded.contains(&item.identifier);
        let is_manually_expanded = tab.manually_expanded.contains(&item.identifier);
        let is_auto_expanded = is_expanded && !is_manually_expanded;
        let is_match = has_search_query && tab.search.matches(&item.identifier);
        let is_dimmed = has_search_query && !is_match;

        // Expand/collapse indicator:
        // - Collapsed: ▸ (default color)
        // - Auto-expanded: ▹ (hollow, gold) — partial view
        // - Manually expanded: ▾ (default color) — full file list
        let (arrow, arrow_color) = if is_manually_expanded {
            ("\u{25be}", None) // ▾, use icon_color
        } else if is_auto_expanded {
            ("\u{25b9}", Some(theme.gold)) // ▹ (hollow), gold
        } else {
            ("\u{25b8}", None) // ▸, use icon_color
        };

        // Check if this active item has drained (no files currently transferring)
        // while globally paused — show ⏸ instead of the expand arrow.
        let is_paused = tab.paused.load(Ordering::Relaxed);
        let item_has_active_files = state
            .active_files
            .values()
            .any(|f| f.identifier == item.identifier);
        let is_drained_paused = is_paused && is_active && !item_has_active_files;

        // Status icon + color
        let (icon, icon_color) = if is_drained_paused {
            ("\u{23f8}", theme.gold) // ⏸ paused
        } else {
            match &item.status {
                UploadItemStatus::Pending => ("\u{00b7}", theme.text_muted),
                UploadItemStatus::Verifying => (arrow, arrow_color.unwrap_or(theme.gold)),
                UploadItemStatus::Uploading => (arrow, arrow_color.unwrap_or(theme.gold)),
                UploadItemStatus::RateLimited => ("\u{23f8}", theme.gold),
                UploadItemStatus::Complete => ("\u{2713}", theme.green),
                UploadItemStatus::Failed(_) => ("\u{2717}", theme.red),
            }
        };

        // File progress summary
        let files_done = item.files_completed + item.files_skipped + item.files_failed;
        let files_info = if item.files_total > 0 {
            format!("{}/{} files", files_done, item.files_total)
        } else {
            String::new()
        };

        // Bytes
        let bytes_info = if item.bytes_uploaded > 0 {
            format!(" \u{00b7} {}", widgets::format_bytes(item.bytes_uploaded))
        } else {
            String::new()
        };

        // Elapsed for completed items
        let elapsed_info =
            if matches!(item.status, UploadItemStatus::Complete) && item.bytes_uploaded > 0 {
                let elapsed = item.started_at.elapsed();
                format!(" \u{00b7} {}", widgets::format_elapsed(elapsed))
            } else {
                String::new()
            };

        // Skipped summary for completed items
        let skipped_info =
            if matches!(item.status, UploadItemStatus::Complete) && item.files_skipped > 0 {
                format!(" \u{00b7} {} skipped", item.files_skipped)
            } else {
                String::new()
            };

        // Item status label
        let status_label = match &item.status {
            UploadItemStatus::Pending => " (pending)",
            UploadItemStatus::Verifying => " (verifying)",
            UploadItemStatus::Uploading => "",
            UploadItemStatus::RateLimited => " (rate-limited)",
            UploadItemStatus::Complete => "",
            UploadItemStatus::Failed(msg) => {
                // We'll render this inline below
                let _ = msg;
                ""
            }
        };

        // Name color based on state and cursor
        let (name_color, name_mod) = if is_cursor {
            (theme.gold, Modifier::BOLD)
        } else if is_dimmed {
            (theme.text_very_muted, Modifier::empty())
        } else if is_active {
            (theme.gold, Modifier::empty())
        } else if matches!(item.status, UploadItemStatus::Complete) {
            (theme.green, Modifier::empty())
        } else if matches!(item.status, UploadItemStatus::Failed(_)) {
            (theme.red, Modifier::empty())
        } else {
            (theme.text, Modifier::empty())
        };

        let icon_style = if is_dimmed {
            Style::default().fg(theme.text_very_muted)
        } else {
            Style::default().fg(icon_color)
        };
        let meta_color = if is_dimmed {
            theme.text_very_muted
        } else {
            theme.text_muted
        };

        // Cursor indicator
        let cursor_span = if is_cursor {
            Span::styled("\u{25b8} ", Style::default().fg(theme.gold))
        } else {
            Span::raw("  ")
        };

        let summary = format!(
            "{}{}{}{}{}",
            files_info, skipped_info, bytes_info, elapsed_info, status_label
        );

        let base_name_style = Style::default().fg(name_color).add_modifier(name_mod);
        let mut spans = vec![cursor_span, Span::styled(format!("{icon} "), icon_style)];

        // Highlight matching substring when searching
        if has_search_query && is_match {
            let match_style = base_name_style.add_modifier(Modifier::UNDERLINED);
            spans.extend(highlight_match(
                &item.identifier,
                tab.search.query(),
                base_name_style,
                match_style,
            ));
        } else {
            spans.push(Span::styled(item.identifier.clone(), base_name_style));
        }

        if !summary.is_empty() {
            spans.push(Span::styled(
                format!("  {summary}"),
                Style::default().fg(meta_color),
            ));
        }

        // Failed items show the error inline
        if let UploadItemStatus::Failed(msg) = &item.status {
            spans.push(Span::styled(
                format!("  \u{2717} {msg}"),
                Style::default().fg(if is_dimmed {
                    theme.text_very_muted
                } else {
                    theme.red
                }),
            ));
        }

        all_rows.push((Line::from(spans), is_cursor));

        // File rows: show when expanded, or when active+collapsed show current file
        let show_files = is_expanded
            || (is_active
                && state
                    .active_files
                    .values()
                    .any(|f| f.identifier == item.identifier));

        if show_files {
            let files = state.files_for_item(&item.identifier);

            if is_auto_expanded {
                // Auto-expanded: show only active files.
                let mut showed_any = false;
                for file in &files {
                    if matches!(file.status, FileDisplayStatus::Active) {
                        all_rows.push((render_file_line(file, theme, is_dimmed), false));
                        showed_any = true;
                    }
                }
                // If no active files but item is still going, show the next
                // pending file as a placeholder so the row doesn't collapse
                // and cause a visual jump.
                if !showed_any && is_active {
                    if let Some(next) = files
                        .iter()
                        .find(|f| matches!(f.status, FileDisplayStatus::Pending))
                    {
                        all_rows.push((render_file_line(next, theme, is_dimmed), false));
                    }
                }
            } else {
                for file in &files {
                    let file_line = render_file_line(file, theme, is_dimmed);
                    all_rows.push((file_line, false));

                    // For collapsed active items, only show the first active file.
                    if !is_expanded && is_active && matches!(file.status, FileDisplayStatus::Active)
                    {
                        break;
                    }
                }
            }
        }
    }

    // Empty state
    if all_rows.is_empty() {
        let msg = if state.done {
            "All uploads complete."
        } else {
            "Waiting for uploads..."
        };
        let line = Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(if state.done {
                theme.green
            } else {
                theme.text_muted
            }),
        ));
        frame.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    // Find which row the cursor item header is at, and scroll to keep it visible.
    let cursor_row = all_rows
        .iter()
        .position(|(_, is_cursor)| *is_cursor)
        .unwrap_or(0);

    let scroll = if cursor_row < visible_height {
        0
    } else {
        cursor_row.saturating_sub(visible_height / 2)
    };

    let visible: Vec<Line> = all_rows
        .into_iter()
        .skip(scroll)
        .take(visible_height)
        .map(|(line, _)| line)
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

/// Split `text` around the first case-insensitive match of `query`, applying
/// `match_style` to the matched substring and `base_style` elsewhere.
fn highlight_match(
    text: &str,
    query: &str,
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    let lower = text.to_lowercase();
    let lower_q = query.to_lowercase();
    if let Some(pos) = lower.find(&lower_q) {
        let before = &text[..pos];
        let matched = &text[pos..pos + query.len()];
        let after = &text[pos + query.len()..];
        let mut spans = Vec::new();
        if !before.is_empty() {
            spans.push(Span::styled(before.to_string(), base_style));
        }
        spans.push(Span::styled(matched.to_string(), match_style));
        if !after.is_empty() {
            spans.push(Span::styled(after.to_string(), base_style));
        }
        spans
    } else {
        vec![Span::styled(text.to_string(), base_style)]
    }
}

/// Render a single file line for the tree view.
fn render_file_line(
    file: &super::upload_app::FileDisplayEntry,
    theme: &Theme,
    dimmed: bool,
) -> Line<'static> {
    let indent = "    ";

    let (icon, icon_color, name_color) = match &file.status {
        FileDisplayStatus::Active => {
            // Check if rate-limited
            if file.upload_status.as_ref().is_some_and(|s| {
                matches!(
                    s,
                    UploadProgressStatus::WaitingRateLimit | UploadProgressStatus::Retrying
                )
            }) {
                ("\u{23f8}", theme.gold, theme.gold) // ⏸
            } else {
                ("", theme.green, theme.text) // no icon, show progress bar
            }
        }
        FileDisplayStatus::Completed => ("\u{2713}", theme.green, theme.text_muted),
        FileDisplayStatus::Skipped => ("~", theme.text_muted, theme.text_muted),
        FileDisplayStatus::Failed(_) => ("\u{2717}", theme.red, theme.red),
        FileDisplayStatus::Pending => ("\u{00b7}", theme.text_very_muted, theme.text_muted),
    };

    let actual_icon_color = if dimmed {
        theme.text_very_muted
    } else {
        icon_color
    };
    let actual_name_color = if dimmed {
        theme.text_very_muted
    } else {
        name_color
    };

    match &file.status {
        FileDisplayStatus::Active
            if !file.upload_status.as_ref().is_some_and(|s| {
                matches!(
                    s,
                    UploadProgressStatus::WaitingRateLimit | UploadProgressStatus::Retrying
                )
            }) =>
        {
            // Active file with progress bar
            let progress = if file.size > 0 {
                file.bytes_sent as f64 / file.size as f64
            } else {
                0.0
            };

            let bar_width = 12usize;
            let filled = (progress * bar_width as f64) as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar = format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty),);

            let pct = format!("{:.0}%", progress * 100.0);
            let bytes = format!(
                "{}/{}",
                widgets::format_bytes(file.bytes_sent),
                widgets::format_bytes(file.size)
            );

            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
                Span::raw("  "),
                Span::styled(
                    bar,
                    Style::default().fg(if dimmed {
                        theme.text_very_muted
                    } else {
                        theme.green
                    }),
                ),
                Span::styled(format!(" {pct}"), Style::default().fg(actual_name_color)),
                Span::styled(
                    format!(" \u{00b7} {bytes}"),
                    Style::default().fg(if dimmed {
                        theme.text_very_muted
                    } else {
                        theme.text_muted
                    }),
                ),
            ])
        }
        FileDisplayStatus::Active => {
            // Rate-limited active file
            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(format!("{icon} "), Style::default().fg(actual_icon_color)),
                Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
                Span::styled(
                    " \u{00b7} rate-limited".to_string(),
                    Style::default().fg(if dimmed {
                        theme.text_very_muted
                    } else {
                        theme.gold
                    }),
                ),
            ])
        }
        FileDisplayStatus::Completed => {
            let size = widgets::format_bytes(file.size);
            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(format!("{icon} "), Style::default().fg(actual_icon_color)),
                Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
                Span::styled(
                    format!(" \u{00b7} {size}"),
                    Style::default().fg(if dimmed {
                        theme.text_very_muted
                    } else {
                        theme.text_muted
                    }),
                ),
            ])
        }
        FileDisplayStatus::Skipped => Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(format!("{icon} "), Style::default().fg(actual_icon_color)),
            Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
            Span::styled(
                " \u{00b7} skipped (exists)".to_string(),
                Style::default().fg(if dimmed {
                    theme.text_very_muted
                } else {
                    theme.text_muted
                }),
            ),
        ]),
        FileDisplayStatus::Failed(err) => Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(format!("{icon} "), Style::default().fg(actual_icon_color)),
            Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
            Span::styled(
                format!(" \u{00b7} {err}"),
                Style::default().fg(if dimmed {
                    theme.text_very_muted
                } else {
                    theme.red
                }),
            ),
        ]),
        FileDisplayStatus::Pending => {
            let size = widgets::format_bytes(file.size);
            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(format!("{icon} "), Style::default().fg(actual_icon_color)),
                Span::styled(file.name.clone(), Style::default().fg(actual_name_color)),
                Span::styled(
                    format!(" \u{00b7} {size}"),
                    Style::default().fg(theme.text_very_muted),
                ),
            ])
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use ia_core::upload::UploadProgressStatus;

    fn make_state() -> Arc<Mutex<UploadTuiState>> {
        Arc::new(Mutex::new(UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
        ])))
    }

    fn make_s3_state() -> Arc<Mutex<S3TaskState>> {
        Arc::new(Mutex::new(S3TaskState::new()))
    }

    fn make_paused() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn progress(
        id: &str,
        key: &str,
        bytes_sent: u64,
        total_bytes: u64,
        status: UploadProgressStatus,
    ) -> ia_core::upload::UploadProgress {
        ia_core::upload::UploadProgress {
            identifier: id.to_string(),
            key: key.to_string(),
            bytes_sent,
            total_bytes,
            status,
        }
    }

    #[test]
    fn test_initial_state() {
        let tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        assert_eq!(tab.cursor, 0);
        assert!(tab.expanded.is_empty());
        assert!(!tab.search.is_active());
    }

    #[test]
    fn test_j_k_moves_cursor() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0); // clamped at 0
    }

    #[test]
    fn test_key_hints() {
        let tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "j/k"));
        assert!(hints.iter().any(|(k, _)| *k == "/"));
        // Tab should NOT be in hints anymore
        assert!(!hints.iter().any(|(k, _)| *k == "Tab"));
    }

    #[test]
    fn test_items_cursor_clamped() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        for _ in 0..5 {
            tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(tab.cursor, 1); // 2 items, max = 1
    }

    #[test]
    fn test_enter_sets_status_message() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        assert!(tab.status_text().is_none());
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let text = tab.status_text();
        assert!(text.is_some());
        assert!(text.unwrap().contains("archive.org"));
        assert!(text.unwrap().contains("item-a"));
    }

    #[test]
    fn test_tick_clears_expired_status_message() {
        use std::time::Duration;
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        tab.status_message = Some((
            "https://archive.org/details/item-a".to_string(),
            Instant::now() - Duration::from_secs(6),
        ));
        assert!(tab.status_text().unwrap().contains("archive.org/details"));
        tab.tick();
        assert!(tab.status_message.is_none());
    }

    #[test]
    fn test_expand_collapse() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        assert!(tab.expanded.is_empty());

        // Expand item-a (cursor at 0)
        tab.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(tab.expanded.contains("item-a"));

        // Collapse item-a
        tab.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert!(!tab.expanded.contains("item-a"));

        // l/h also work
        tab.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);
        assert!(tab.expanded.contains("item-a"));
        tab.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
        assert!(!tab.expanded.contains("item-a"));
    }

    #[test]
    fn test_search_activates_on_slash() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        assert!(!tab.search.is_active());
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
    }

    #[test]
    fn test_search_esc_cancels() {
        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(tab.search.query(), "a");
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!tab.search.is_active());
        assert!(tab.search.query().is_empty()); // canceled clears query
    }

    #[test]
    fn test_search_n_cycles_matches() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&[
            "nasa-photos".to_string(),
            "hubble-deep".to_string(),
            "nasa-data".to_string(),
        ])));
        let mut tab = UploadTab::new(state, make_s3_state(), make_paused());

        // Search for "nasa"
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        // Should have matches at indices 0 and 2 (nasa-photos and nasa-data)
        assert_eq!(tab.search_matches.len(), 2);
        assert_eq!(tab.cursor, tab.search_matches[0]); // jumped to first match

        // 'n' goes to next match
        tab.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, tab.search_matches[1]);

        // 'n' wraps to first match
        tab.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, tab.search_matches[0]);

        // 'N' goes backward (wraps to last)
        tab.handle_key(KeyCode::Char('N'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, tab.search_matches[1]);
    }

    #[test]
    fn test_active_items_pinned_to_top() {
        let state_inner = Arc::new(Mutex::new(UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
            "item-c".to_string(),
        ])));

        // Make item-b active
        {
            let mut s = state_inner.lock().unwrap();
            s.update(progress(
                "item-b",
                "file.txt",
                0,
                100,
                UploadProgressStatus::Verifying,
            ));
        }

        let _tab = UploadTab::new(state_inner.clone(), make_s3_state(), make_paused());
        let s = state_inner.lock().unwrap();
        let ord = ordered_items(&s);

        // item-b (index 1) should be pinned to top
        assert_eq!(ord[0], 1); // item-b
        assert_eq!(ord[1], 0); // item-a
        assert_eq!(ord[2], 2); // item-c
    }

    #[test]
    fn test_cursor_follows_item_on_reorder() {
        let state_inner = Arc::new(Mutex::new(UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
            "item-c".to_string(),
        ])));

        let mut tab = UploadTab::new(state_inner.clone(), make_s3_state(), make_paused());
        // Position cursor on item-c (index 2)
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 2);
        // cursor_identifier should be set
        assert_eq!(tab.cursor_identifier.as_deref(), Some("item-c"));

        // Make item-b active, causing reorder
        {
            let mut s = state_inner.lock().unwrap();
            s.update(progress(
                "item-b",
                "file.txt",
                0,
                100,
                UploadProgressStatus::Verifying,
            ));
        }

        tab.tick();
        // Cursor should still be on item-c despite reorder
        let s = state_inner.lock().unwrap();
        let ord = ordered_items(&s);
        assert_eq!(
            ord[tab.cursor], s.item_index["item-c"],
            "cursor should follow item-c"
        );
    }

    #[test]
    fn test_draw_does_not_panic_with_tree() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let state = make_state();
        // Add some file progress
        {
            let mut s = state.lock().unwrap();
            s.update(progress(
                "item-a",
                "file1.txt",
                0,
                1000,
                UploadProgressStatus::Verifying,
            ));
            s.update(progress(
                "item-a",
                "file1.txt",
                500,
                1000,
                UploadProgressStatus::Uploading,
            ));
        }

        let mut tab = UploadTab::new(state, make_s3_state(), make_paused());
        tab.expanded.insert("item-a".to_string());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::for_env("truecolor");

        terminal
            .draw(|frame| {
                let area = frame.area();
                tab.draw(frame, area, &theme);
            })
            .unwrap();
    }

    #[test]
    fn test_draw_with_search_active() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut tab = UploadTab::new(make_state(), make_s3_state(), make_paused());
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::for_env("truecolor");

        terminal
            .draw(|frame| {
                let area = frame.area();
                tab.draw(frame, area, &theme);
            })
            .unwrap();
    }
}
