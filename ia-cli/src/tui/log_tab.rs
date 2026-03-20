//! Log tab for the multi-tab upload dashboard.
//!
//! Displays a live-tailing job log with vim-style navigation, search, and
//! status filtering.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::joblog_state::{JoblogState, LogStatus};
use super::search::{FilterCycle, SearchState};
use super::tab::TabView;
use super::theme::Theme;

/// The Log tab: a tailable, searchable, filterable job log viewer.
#[derive(Debug)]
pub struct LogTab {
    joblog_state: Arc<Mutex<JoblogState>>,
    pub search: SearchState,
    pub filter_cycle: FilterCycle,
    pub status_filter: Option<LogStatus>,
    pub cursor: usize,
    scroll_offset: usize,
    pub pending_g: bool,
    pending_g_at: Instant,
    /// Cached visible entry indices, recomputed only when dirty.
    cached_visible: RefCell<Vec<usize>>,
    cache_dirty: RefCell<bool>,
}

impl LogTab {
    pub fn new(joblog_state: Arc<Mutex<JoblogState>>) -> Self {
        Self {
            joblog_state,
            search: SearchState::new(),
            filter_cycle: FilterCycle::new(&["All", "Uploaded", "Skipped", "Failed"]),
            status_filter: None,
            cursor: 0,
            scroll_offset: 0,
            pending_g: false,
            pending_g_at: Instant::now(),
            cached_visible: RefCell::new(Vec::new()),
            cache_dirty: RefCell::new(true),
        }
    }

    /// Mark the visible entries cache as dirty (call when filter/search/entries change).
    fn invalidate_cache(&self) {
        *self.cache_dirty.borrow_mut() = true;
    }

    /// Build the filtered + searched list of entry indices into the joblog entries vec.
    fn compute_visible(&self, state: &JoblogState) -> Vec<usize> {
        state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| match self.status_filter {
                None => true,
                Some(LogStatus::Uploaded) => e.display_status == LogStatus::Uploaded,
                Some(LogStatus::Skipped) => e.display_status == LogStatus::Skipped,
                Some(LogStatus::Failed) => e.display_status == LogStatus::Failed,
            })
            .filter(|(_, e)| {
                let text = format!("{} {} {}", e.item, e.file, e.time);
                self.search.matches(&text)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Get visible entries, recomputing from cache only when dirty.
    fn visible_entries(&self, state: &JoblogState) -> std::cell::Ref<'_, Vec<usize>> {
        if *self.cache_dirty.borrow() {
            *self.cached_visible.borrow_mut() = self.compute_visible(state);
            *self.cache_dirty.borrow_mut() = false;
        }
        self.cached_visible.borrow()
    }
}

impl TabView for LogTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Ok(state) = self.joblog_state.lock() else {
            return;
        };
        let visible = self.visible_entries(&state);
        let total = visible.len();

        // Build title with optional filter indicator.
        let title = if let Some(ref filter) = self.status_filter {
            let label = match filter {
                LogStatus::Uploaded => "uploaded",
                LogStatus::Skipped => "skipped",
                LogStatus::Failed => "failed",
            };
            format!(" Job Log [filter: {}] ", label)
        } else {
            " Job Log ".to_string()
        };

        // Layout: log table (fill), optional search bar (1 row).
        let show_search = self.search.is_active() || !self.search.query().is_empty();
        let search_height = if show_search { 1 } else { 0 };
        let chunks =
            Layout::vertical([Constraint::Min(4), Constraint::Length(search_height)]).split(area);

        // Reserve lines for borders + header inside the block.
        let border_lines = 2; // top + bottom border
        let header_lines = 1;
        let viewport_height = (chunks[0].height as usize)
            .saturating_sub(border_lines)
            .saturating_sub(header_lines);

        // Clamp cursor to visible range before computing scroll.
        let clamped_cursor = if total == 0 {
            0
        } else {
            self.cursor.min(total - 1)
        };

        // Compute scroll window.
        let scroll_offset = if viewport_height == 0 {
            0
        } else if clamped_cursor < self.scroll_offset {
            clamped_cursor
        } else if clamped_cursor >= self.scroll_offset + viewport_height {
            clamped_cursor.saturating_sub(viewport_height - 1)
        } else {
            self.scroll_offset
        };

        let end = (scroll_offset + viewport_height).min(total);
        let start = scroll_offset.min(end);

        // Build lines.
        let mut lines = Vec::new();

        // Header row.
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<10}", "TIME"),
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<20}", "ITEM"),
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<30}", "FILE"),
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "STATUS",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // Data rows.
        for (idx, &entry_idx) in visible.iter().enumerate().skip(start).take(end - start) {
            let entry = &state.entries[entry_idx];
            let is_cursor = idx == clamped_cursor;

            let (status_text, status_color) = match entry.display_status {
                LogStatus::Uploaded => ("\u{2713} uploaded", theme.green),
                LogStatus::Skipped => ("\u{2013} skipped", theme.text_muted),
                LogStatus::Failed => ("\u{2717} failed", theme.red),
            };

            let row_style = if is_cursor {
                Style::default()
                    .fg(theme.text)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(theme.text)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", entry.time), row_style),
                Span::styled(format!("{:<20}", entry.item), row_style),
                Span::styled(format!("{:<30}", entry.file), row_style),
                Span::styled(
                    status_text.to_string(),
                    if is_cursor {
                        row_style
                    } else {
                        Style::default().fg(status_color)
                    },
                ),
            ]));
        }

        // Bottom border content: entry count + position indicator.
        let position_info = if total > 0 {
            format!(" {} entries \u{2502} showing {}-{} ", total, start + 1, end)
        } else {
            " 0 entries ".to_string()
        };

        let block = Block::default()
            .title(title)
            .title_bottom(Line::from(position_info).centered())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, chunks[0]);

        // Search bar rendered in its own area (separate from the Paragraph).
        if self.search.is_active() {
            let search_line = Line::from(vec![
                Span::styled("/", Style::default().fg(theme.gold)),
                Span::styled(
                    self.search.query().to_string(),
                    Style::default().fg(theme.text),
                ),
                Span::styled("\u{2588}", Style::default().fg(theme.gold)),
            ]);
            frame.render_widget(Paragraph::new(search_line), chunks[1]);
        } else if !self.search.query().is_empty() {
            let search_line = Line::from(vec![
                Span::styled("/", Style::default().fg(theme.text_muted)),
                Span::styled(
                    self.search.query().to_string(),
                    Style::default().fg(theme.text_muted),
                ),
                Span::styled(
                    format!("  ({} matches)", total),
                    Style::default().fg(theme.text_very_muted),
                ),
            ]);
            frame.render_widget(Paragraph::new(search_line), chunks[1]);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // When search is active, route input to search.
        if self.search.is_active() {
            match code {
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.invalidate_cache();
                    return true;
                }
                KeyCode::Backspace => {
                    self.search.backspace();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.invalidate_cache();
                    return true;
                }
                KeyCode::Enter => {
                    self.search.confirm();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    return true;
                }
                KeyCode::Esc => {
                    self.search.cancel();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.invalidate_cache();
                    return true;
                }
                _ => return false,
            }
        }

        // Normal mode.
        let entry_count = {
            let Ok(state) = self.joblog_state.lock() else {
                return false;
            };
            self.visible_entries(&state).len()
        };

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.pending_g = false;
                if entry_count > 0 && self.cursor < entry_count - 1 {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.pending_g = false;
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                true
            }
            KeyCode::Char('G') if modifiers.contains(KeyModifiers::SHIFT) => {
                self.pending_g = false;
                if entry_count > 0 {
                    self.cursor = entry_count - 1;
                }
                true
            }
            KeyCode::Char('g') => {
                if self.pending_g && self.pending_g_at.elapsed() < Duration::from_millis(500) {
                    // gg: jump to top
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.pending_g = false;
                } else {
                    self.pending_g = true;
                    self.pending_g_at = Instant::now();
                }
                true
            }
            KeyCode::Char('/') => {
                self.pending_g = false;
                self.search.activate();
                self.invalidate_cache();
                true
            }
            KeyCode::Char('f') => {
                self.pending_g = false;
                self.filter_cycle.next();
                self.status_filter = match self.filter_cycle.current() {
                    "Uploaded" => Some(LogStatus::Uploaded),
                    "Skipped" => Some(LogStatus::Skipped),
                    "Failed" => Some(LogStatus::Failed),
                    _ => None, // "All"
                };
                // Reset cursor when filter changes.
                self.cursor = 0;
                self.scroll_offset = 0;
                self.invalidate_cache();
                true
            }
            KeyCode::Esc => {
                // Clear confirmed search filter.
                if !self.search.query().is_empty() {
                    self.search.cancel();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    self.invalidate_cache();
                    return true;
                }
                false
            }
            _ => {
                self.pending_g = false;
                false
            }
        }
    }

    fn tick(&mut self) {
        if let Ok(mut state) = self.joblog_state.lock() {
            if state.needs_tail() {
                let before = state.entries.len();
                state.tail();
                if state.entries.len() != before {
                    self.invalidate_cache();
                }
            }
        }

        if self.pending_g && self.pending_g_at.elapsed() > Duration::from_millis(500) {
            self.pending_g = false;
        }
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("G", "end"),
            ("gg", "top"),
            ("/", "search"),
            ("f", "filter"),
            ("?", "help"),
            ("q", "quit"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_joblog_state() -> (Arc<Mutex<JoblogState>>, tempfile::TempPath) {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..20 {
            writeln!(
                f,
                r#"{{"ts":"2026-03-18T14:{:02}:00Z","op":"upload","item":"item-{}","file":"file-{}.txt","status":"ok"}}"#,
                i, i, i
            )
            .unwrap();
        }
        f.flush().unwrap();
        let state = JoblogState::open(f.path()).unwrap();
        let path = f.into_temp_path();
        (Arc::new(Mutex::new(state)), path)
    }

    #[test]
    fn test_scroll_j_k() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        assert_eq!(tab.cursor, 0);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
    }

    #[test]
    fn test_jump_to_end() {
        let (state, _path) = make_joblog_state();
        let entry_count = state.lock().unwrap().entries.len();
        let mut tab = LogTab::new(state);
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, entry_count - 1);
    }

    #[test]
    fn test_gg_jump_to_top() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        for _ in 0..5 {
            tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(tab.cursor, 5);
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(tab.pending_g);
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
        assert!(!tab.pending_g);
    }

    #[test]
    fn test_filter_cycle() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        assert!(tab.status_filter.is_none());
        tab.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(tab.filter_cycle.current(), "Uploaded");
        tab.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(tab.filter_cycle.current(), "Skipped");
    }

    #[test]
    fn test_search_activation() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
    }

    fn make_mixed_joblog() -> (Arc<Mutex<JoblogState>>, tempfile::TempPath) {
        let mut f = NamedTempFile::new().unwrap();
        // 3 uploaded, 2 skipped, 1 failed — "nasa" appears in items 0,1
        writeln!(f, r#"{{"ts":"2026-03-18T14:00:00Z","op":"upload","item":"nasa-photos","file":"a.jpg","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:01:00Z","op":"upload","item":"nasa-data","file":"b.csv","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:02:00Z","op":"upload","item":"hubble","file":"c.fits","status":"ok"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:03:00Z","op":"upload","item":"nasa-photos","file":"d.jpg","status":"skipped"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:04:00Z","op":"upload","item":"hubble","file":"e.fits","status":"skipped"}}"#).unwrap();
        writeln!(f, r#"{{"ts":"2026-03-18T14:05:00Z","op":"upload","item":"nasa-photos","file":"f.jpg","status":"error","error":"503"}}"#).unwrap();
        f.flush().unwrap();
        let state = JoblogState::open(f.path()).unwrap();
        let path = f.into_temp_path();
        (Arc::new(Mutex::new(state)), path)
    }

    #[test]
    fn test_search_plus_filter_intersects() {
        let (state, _path) = make_mixed_joblog();
        let mut tab = LogTab::new(state.clone());

        // Filter to "Uploaded" only (3 entries: nasa-photos, nasa-data, hubble)
        tab.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(tab.status_filter, Some(LogStatus::Uploaded));
        {
            let s = state.lock().unwrap();
            let visible = tab.visible_entries(&s);
            assert_eq!(visible.len(), 3);
        }

        // Now search for "nasa" — should intersect: only 2 uploaded nasa items
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        {
            let s = state.lock().unwrap();
            let visible = tab.visible_entries(&s);
            assert_eq!(visible.len(), 2, "search + filter should intersect");
        }
    }

    #[test]
    fn test_search_resets_cursor() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        // Move cursor down
        for _ in 0..5 {
            tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(tab.cursor, 5);
        // Start search — cursor should reset when typing
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
    }

    #[test]
    fn test_esc_clears_confirmed_search() {
        let (state, _path) = make_mixed_joblog();
        let mut tab = LogTab::new(state.clone());
        // Search and confirm
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(tab.search.query(), "nasa");
        // Esc in normal mode should clear the search
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(tab.search.query().is_empty());
        // All entries should be visible again
        {
            let s = state.lock().unwrap();
            let visible = tab.visible_entries(&s);
            assert_eq!(visible.len(), 6);
        }
    }

    #[test]
    fn test_arrow_keys() {
        let (state, _path) = make_joblog_state();
        let mut tab = LogTab::new(state);
        tab.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
    }
}
