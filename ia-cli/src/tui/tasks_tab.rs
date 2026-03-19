//! Tasks tab for the multi-tab dashboard.
//!
//! Displays S3 task status with a summary panel at top, a scrollable task
//! table, and vim-style search filtering.

use std::sync::{Arc, Mutex};
use std::time::Instant;

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
#[derive(Debug)]
pub struct TasksTab {
    s3_state: Arc<Mutex<S3TaskState>>,
    pub search: SearchState,
    pub cursor: usize,
    scroll_offset: usize,
    /// A URL opened via Enter, shown in the footer for 5 seconds.
    status_message: Option<(String, Instant)>,
    /// Current user's email for filtering tasks in user mode.
    submitter: Option<String>,
}

impl TasksTab {
    pub fn new(s3_state: Arc<Mutex<S3TaskState>>, submitter: Option<String>) -> Self {
        Self {
            s3_state,
            search: SearchState::new(),
            cursor: 0,
            scroll_offset: 0,
            status_message: None,
            submitter,
        }
    }

    /// Return the filtered task list length (for clamping cursor).
    fn task_count(&self) -> usize {
        let Ok(state) = self.s3_state.lock() else {
            return 0;
        };
        state
            .tasks
            .iter()
            .filter(|t| {
                let user_match = state.show_global
                    || self
                        .submitter
                        .as_ref()
                        .is_some_and(|email| t.submitter == *email);
                user_match
                    && (self.search.query().is_empty()
                        || self.search.matches(&t.identifier)
                        || self.search.matches(&t.cmd))
            })
            .count()
    }
}

impl TabView for TasksTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Ok(state) = self.s3_state.lock() else {
            return;
        };

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
        let (q, r, e) = if state.show_global {
            (
                state.global_queued,
                state.global_running,
                state.global_errors,
            )
        } else {
            (state.queued, state.running, state.errors)
        };
        widgets::draw_s3_panel(
            frame,
            chunks[0],
            theme,
            &widgets::S3PanelData {
                queued: q,
                running: r,
                errors: e,
                global_count: state.global_count,
                rate_limited: state.is_rate_limited,
                seconds_ago: state.seconds_since_poll(),
            },
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
            .filter(|t| {
                // In user mode, only show the user's tasks
                let user_match = state.show_global
                    || self
                        .submitter
                        .as_ref()
                        .is_some_and(|email| t.submitter == *email);
                user_match && (self.search.matches(&t.identifier) || self.search.matches(&t.cmd))
            })
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
                if let Ok(mut state) = self.s3_state.lock() {
                    state.toggle_view();
                }
                self.cursor = 0;
                self.scroll_offset = 0;
                true
            }
            KeyCode::Char('/') => {
                self.search.activate();
                true
            }
            KeyCode::Enter => {
                // Open the selected task's item on archive.org.
                // Also store the URL in status_message for 5 seconds so it's
                // visible on headless systems where open::that() fails silently.
                if let Ok(state) = self.s3_state.lock() {
                    let filtered: Vec<_> = state
                        .tasks
                        .iter()
                        .filter(|t| {
                            let user_match = state.show_global
                                || self
                                    .submitter
                                    .as_ref()
                                    .is_some_and(|email| t.submitter == *email);
                            user_match
                                && (self.search.matches(&t.identifier)
                                    || self.search.matches(&t.cmd))
                        })
                        .collect();
                    if let Some(task) = filtered.get(self.cursor) {
                        let url = format!("https://archive.org/history/{}", task.identifier);
                        let _ = open::that(&url);
                        self.status_message = Some((url, Instant::now()));
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self) {
        // Clear status message after 5 seconds.
        if let Some((_, ts)) = &self.status_message {
            if ts.elapsed().as_secs() >= 5 {
                self.status_message = None;
            }
        }
    }

    fn status_text(&self) -> Option<&str> {
        self.status_message.as_ref().map(|(url, _)| url.as_str())
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
        let mut state = S3TaskState::new();
        state.update_tasks(vec![
            S3TaskEntry {
                identifier: "item-a".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "running".into(),
                submittime: "14:01:23".into(),
            },
            S3TaskEntry {
                identifier: "item-b".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "queued".into(),
                submittime: "14:01:25".into(),
            },
            S3TaskEntry {
                identifier: "item-c".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "queued".into(),
                submittime: "14:01:30".into(),
            },
        ]);
        state.update_summary(2, 1, 0);
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn test_scroll_clamps() {
        // Pass submitter so all tasks are visible in user mode.
        let mut tab = TasksTab::new(make_s3_state(), Some("test@example.com".to_string()));
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
        let mut tab = TasksTab::new(s3.clone(), None);
        assert!(!s3.lock().unwrap().show_global);
        tab.handle_key(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(s3.lock().unwrap().show_global);
    }

    #[test]
    fn test_search_activation() {
        let mut tab = TasksTab::new(make_s3_state(), None);
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
        let tab = TasksTab::new(make_s3_state(), None);
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "u"));
        assert!(hints.iter().any(|(k, _)| *k == "Enter"));
    }

    #[test]
    fn test_enter_sets_status_message() {
        // Pass submitter so tasks are visible in default user mode.
        let mut tab = TasksTab::new(make_s3_state(), Some("test@example.com".to_string()));
        // No status message initially
        assert!(tab.status_text().is_none());
        // Press Enter — item-a is at cursor 0
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // Status message should now be set
        let text = tab.status_text();
        assert!(text.is_some());
        assert!(text.unwrap().contains("archive.org"));
        assert!(text.unwrap().contains("item-a"));
    }

    #[test]
    fn test_user_mode_filters_by_submitter() {
        let s3 = make_s3_state();
        // Add a task from a different submitter.
        s3.lock().unwrap().update_tasks(vec![
            S3TaskEntry {
                identifier: "item-a".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "running".into(),
                submittime: "14:01:23".into(),
            },
            S3TaskEntry {
                identifier: "item-other".into(),
                cmd: "s3-put".into(),
                submitter: "other@example.com".into(),
                status: "queued".into(),
                submittime: "14:01:25".into(),
            },
        ]);
        let tab = TasksTab::new(s3.clone(), Some("test@example.com".to_string()));
        // In user mode (default), should only see 1 task.
        assert_eq!(tab.task_count(), 1);
        // Toggle to global mode — should see both.
        s3.lock().unwrap().toggle_view();
        assert_eq!(tab.task_count(), 2);
    }

    #[test]
    fn test_tick_clears_expired_status_message() {
        use std::time::{Duration, Instant};
        let mut tab = TasksTab::new(make_s3_state(), None);
        // Manually inject an old status message (6 seconds ago)
        tab.status_message = Some((
            "https://archive.org/history/item-a".to_string(),
            Instant::now() - Duration::from_secs(6),
        ));
        assert!(tab.status_text().is_some());
        tab.tick();
        assert!(tab.status_text().is_none());
    }
}
