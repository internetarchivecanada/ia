#![allow(dead_code)]
//! Tasks tab for the multi-tab dashboard.
//!
//! Displays S3 task status with a summary panel at top, a scrollable task
//! table, and vim-style search filtering.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use super::s3_state::S3TaskState;
use super::search::SearchState;
use super::tab::TabView;
use super::theme::Theme;
use super::widgets;

/// Tasks tab — shows the S3 task queue with summary counts, a scrollable task
/// table, and search/filter support.
pub struct TasksTab {
    s3_state: Arc<Mutex<S3TaskState>>,
    pub search: SearchState,
    pub cursor: usize,
    scroll_offset: usize,
}

impl TasksTab {
    pub fn new(s3_state: Arc<Mutex<S3TaskState>>) -> Self {
        Self {
            s3_state,
            search: SearchState::new(),
            cursor: 0,
            scroll_offset: 0,
        }
    }

    /// Return the filtered task list length (for clamping cursor).
    fn task_count(&self) -> usize {
        let state = self.s3_state.lock().unwrap();
        if self.search.query().is_empty() {
            state.tasks.len()
        } else {
            state
                .tasks
                .iter()
                .filter(|t| self.search.matches(&t.identifier) || self.search.matches(&t.cmd))
                .count()
        }
    }
}

impl TabView for TasksTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let state = self.s3_state.lock().unwrap();

        // Layout: S3 summary panel (3 rows), task table (fill), optional search bar (1 row).
        let search_height = if self.search.is_active() { 1 } else { 0 };
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(search_height),
        ])
        .split(area);

        // -- S3 summary panel with user/global toggle indicator --
        let view_label = if state.show_global { "global" } else { "user" };
        widgets::draw_s3_panel(
            frame,
            chunks[0],
            theme,
            state.queued,
            state.running,
            state.errors,
            state.global_count,
            state.is_rate_limited,
            state.seconds_since_poll(),
        );
        // Overlay the toggle indicator in the top-right area of the S3 panel.
        let toggle_text = format!("[{}] ", view_label);
        let toggle_span = Span::styled(&toggle_text, Style::default().fg(theme.text_muted));
        let toggle_para = Paragraph::new(Line::from(toggle_span));
        if chunks[0].width > toggle_text.len() as u16 + 2 {
            let toggle_area = Rect {
                x: chunks[0].x + chunks[0].width - toggle_text.len() as u16 - 1,
                y: chunks[0].y + 1,
                width: toggle_text.len() as u16,
                height: 1,
            };
            frame.render_widget(toggle_para, toggle_area);
        }

        // -- Task table --
        let filtered_tasks: Vec<_> = state
            .tasks
            .iter()
            .filter(|t| self.search.matches(&t.identifier) || self.search.matches(&t.cmd))
            .collect();

        let header = Row::new(vec!["SUBMITTED", "IDENTIFIER", "CMD", "STATUS"]).style(
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        );

        let visible_height = chunks[1].height.saturating_sub(2) as usize; // borders
        let rows: Vec<Row> = filtered_tasks
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(visible_height)
            .map(|(i, task)| {
                let status_style = match task.status.as_str() {
                    "running" => Style::default().fg(theme.blue),
                    "queued" => Style::default().fg(theme.gold),
                    "error" => Style::default().fg(theme.red),
                    _ => Style::default().fg(theme.text_secondary),
                };

                let row = Row::new(vec![
                    Span::styled(&task.submittime, Style::default().fg(theme.text_secondary)),
                    Span::styled(&task.identifier, Style::default().fg(theme.text)),
                    Span::styled(&task.cmd, Style::default().fg(theme.text_secondary)),
                    Span::styled(&task.status, status_style),
                ]);

                if i == self.cursor {
                    row.style(Style::default().fg(theme.gold).add_modifier(Modifier::BOLD))
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Percentage(40),
                Constraint::Percentage(20),
                Constraint::Percentage(15),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .title(format!(" Tasks ({}) ", filtered_tasks.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border)),
        );

        frame.render_widget(table, chunks[1]);

        // -- Search bar --
        if self.search.is_active() {
            let search_line = Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme.maroon_bright)),
                Span::styled(self.search.query(), Style::default().fg(theme.text)),
            ]);
            let search_bar = Paragraph::new(search_line);
            frame.render_widget(search_bar, chunks[2]);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.search.is_active() {
            match code {
                KeyCode::Char(c) => {
                    self.search.push(c);
                    // Reset cursor when search changes.
                    self.cursor = 0;
                    self.scroll_offset = 0;
                }
                KeyCode::Backspace => {
                    self.search.backspace();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                }
                KeyCode::Enter => self.search.confirm(),
                KeyCode::Esc => {
                    self.search.cancel();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                }
                _ => return false,
            }
            return true;
        }

        // Normal mode.
        let _ = modifiers; // reserved for future shift/ctrl combos
        match code {
            KeyCode::Char('j') => {
                let len = self.task_count();
                if len > 0 && self.cursor < len - 1 {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Char('u') => {
                let mut state = self.s3_state.lock().unwrap();
                state.toggle_view();
                drop(state);
                self.cursor = 0;
                self.scroll_offset = 0;
                true
            }
            KeyCode::Char('/') => {
                self.search.activate();
                true
            }
            KeyCode::Enter => {
                // Browser open will be wired later.
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self) {
        // No-op — polling is driven by the outer dashboard loop.
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("/", "search"),
            ("u", "user/global"),
            ("Enter", "history"),
            ("?", "help"),
            ("q", "quit"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::super::s3_state::S3TaskEntry;
    use super::*;
    use crossterm::event::KeyCode;

    fn make_s3_state() -> Arc<Mutex<S3TaskState>> {
        let mut state = S3TaskState::new("test@example.com".to_string());
        state.update_tasks(vec![
            S3TaskEntry {
                identifier: "item-a".into(),
                cmd: "s3-put".into(),
                status: "running".into(),
                submittime: "14:01:23".into(),
            },
            S3TaskEntry {
                identifier: "item-b".into(),
                cmd: "s3-put".into(),
                status: "queued".into(),
                submittime: "14:01:25".into(),
            },
            S3TaskEntry {
                identifier: "item-c".into(),
                cmd: "s3-put".into(),
                status: "queued".into(),
                submittime: "14:01:30".into(),
            },
        ]);
        state.update_summary(2, 1, 0);
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn test_scroll_clamps() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 2);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 2); // clamped at len-1
    }

    #[test]
    fn test_toggle_view() {
        let s3 = make_s3_state();
        let mut tab = TasksTab::new(s3.clone());
        assert!(!s3.lock().unwrap().show_global);
        tab.handle_key(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(s3.lock().unwrap().show_global);
    }

    #[test]
    fn test_search_activation() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(tab.search.is_active());
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(tab.search.query(), "a");
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!tab.search.is_active());
        assert!(tab.search.query().is_empty());
    }

    #[test]
    fn test_key_hints() {
        let tab = TasksTab::new(make_s3_state());
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "u"));
        assert!(hints.iter().any(|(k, _)| *k == "Enter"));
    }
}
