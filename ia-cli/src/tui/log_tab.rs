#![allow(dead_code)]

//! Log tab for the multi-tab upload dashboard.
//!
//! Displays a live-tailing job log with vim-style navigation, search, and
//! status filtering.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::joblog_state::{JoblogState, LogStatus};
use super::search::{FilterCycle, SearchState};
use super::tab::TabView;
use super::theme::Theme;

/// The Log tab: a tailable, searchable, filterable job log viewer.
pub struct LogTab {
    joblog_state: Arc<Mutex<JoblogState>>,
    pub search: SearchState,
    pub filter_cycle: FilterCycle,
    pub status_filter: Option<LogStatus>,
    pub cursor: usize,
    scroll_offset: usize,
    pub pending_g: bool,
    pending_g_at: Instant,
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
        }
    }

    /// Build the filtered + searched list of entry indices into the joblog entries vec.
    fn visible_entries(&self, state: &JoblogState) -> Vec<usize> {
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

    /// Ensure the scroll_offset keeps the cursor visible within the viewport.
    fn adjust_scroll(&mut self, viewport_height: usize, total: usize) {
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        if self.cursor >= total {
            self.cursor = total.saturating_sub(1);
        }
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        }
        if self.cursor >= self.scroll_offset + viewport_height {
            self.scroll_offset = self
                .cursor
                .saturating_sub(viewport_height.saturating_sub(1));
        }
    }
}

impl TabView for LogTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let state = self.joblog_state.lock().unwrap();
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

        // Reserve 1 line for header, 1 for search input (if active), 1 for borders top, 1 for borders bottom.
        let border_lines = 2; // top + bottom border
        let header_lines = 1;
        let search_lines = if self.search.is_active() { 1 } else { 0 };
        let viewport_height = (area.height as usize)
            .saturating_sub(border_lines)
            .saturating_sub(header_lines)
            .saturating_sub(search_lines);

        // Compute scroll window.
        let scroll_offset = if self.cursor < self.scroll_offset {
            self.cursor
        } else if self.cursor >= self.scroll_offset + viewport_height {
            self.cursor
                .saturating_sub(viewport_height.saturating_sub(1))
        } else {
            self.scroll_offset
        };

        let end = (scroll_offset + viewport_height).min(total);
        let start = scroll_offset;

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
            let is_cursor = idx == self.cursor;

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

        // Search input line.
        if self.search.is_active() {
            lines.push(Line::from(vec![
                Span::styled("/", Style::default().fg(theme.gold)),
                Span::styled(
                    self.search.query().to_string(),
                    Style::default().fg(theme.text),
                ),
                Span::styled("\u{2588}", Style::default().fg(theme.gold)),
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
        frame.render_widget(paragraph, area);
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // When search is active, route input to search.
        if self.search.is_active() {
            match code {
                KeyCode::Char(c) => {
                    self.search.push(c);
                    return true;
                }
                KeyCode::Backspace => {
                    self.search.backspace();
                    return true;
                }
                KeyCode::Enter => {
                    self.search.confirm();
                    return true;
                }
                KeyCode::Esc => {
                    self.search.cancel();
                    return true;
                }
                _ => return false,
            }
        }

        // Normal mode.
        let entry_count = {
            let state = self.joblog_state.lock().unwrap();
            self.visible_entries(&state).len()
        };

        match code {
            KeyCode::Char('j') => {
                self.pending_g = false;
                if entry_count > 0 && self.cursor < entry_count - 1 {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Char('k') => {
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
                true
            }
            _ => {
                self.pending_g = false;
                false
            }
        }
    }

    fn tick(&mut self) {
        let mut state = self.joblog_state.lock().unwrap();
        if state.needs_tail() {
            state.tail();
        }
        drop(state);

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
}
