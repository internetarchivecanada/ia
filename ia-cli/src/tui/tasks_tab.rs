//! Tasks tab for the multi-tab dashboard.
//!
//! Displays S3 task status with a summary panel at top, a scrollable task
//! table, and vim-style search filtering.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Tasks tab — shows the S3 task queue with summary counts, a scrollable task
/// table, and search/filter support.
#[derive(Debug)]
pub struct TasksTab {
    s3_state: Arc<Mutex<S3TaskState>>,
    pub search: SearchState,
    pub cursor: usize,
    scroll_offset: usize,
    pub pending_g: bool,
    pending_g_at: Instant,
    /// A URL opened via Enter, shown in the footer for 5 seconds.
    status_message: Option<(String, Instant)>,
}

impl TasksTab {
    pub fn new(s3_state: Arc<Mutex<S3TaskState>>) -> Self {
        Self {
            s3_state,
            search: SearchState::new(),
            cursor: 0,
            scroll_offset: 0,
            pending_g: false,
            pending_g_at: Instant::now(),
            status_message: None,
        }
    }

    /// Return the filtered task list length (for clamping cursor).
    fn task_count(&self) -> usize {
        let Ok(state) = self.s3_state.lock() else {
            return 0;
        };
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
        let Ok(state) = self.s3_state.lock() else {
            return;
        };

        // Layout: task table (fill), optional search bar (1 row).
        // S3+Progress panes are rendered by dashboard.rs above this area.
        let search_height = if self.search.is_active() { 1 } else { 0 };
        let chunks =
            Layout::vertical([Constraint::Min(4), Constraint::Length(search_height)]).split(area);

        // -- Task table --
        let filtered_tasks: Vec<_> = state
            .tasks
            .iter()
            .filter(|t| self.search.matches(&t.identifier) || self.search.matches(&t.cmd))
            .collect();

        let header = Row::new(vec!["IDENTIFIER", "TASK_ID", "SUBMITTIME", "STATUS"]).style(
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        );

        let visible_height = chunks[0].height.saturating_sub(2) as usize; // borders
        let rows: Vec<Row> = filtered_tasks
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(visible_height)
            .map(|(i, task)| {
                let status_style = match task.status.as_str() {
                    "queued" => Style::default().fg(theme.green),
                    "running" => Style::default().fg(theme.blue),
                    "error" => Style::default().fg(theme.red),
                    "paused" => Style::default().fg(theme.gold),
                    _ => Style::default().fg(theme.text_secondary),
                };

                let row = Row::new(vec![
                    Span::styled(&task.identifier, Style::default().fg(theme.text)),
                    Span::styled(
                        task.task_id.to_string(),
                        Style::default().fg(theme.text_secondary),
                    ),
                    Span::styled(&task.submittime, Style::default().fg(theme.text_secondary)),
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
                Constraint::Percentage(35),
                Constraint::Length(12),
                Constraint::Length(19),
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

        frame.render_widget(table, chunks[0]);

        // -- Search bar --
        if self.search.is_active() {
            let search_line = Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme.maroon_bright)),
                Span::styled(self.search.query(), Style::default().fg(theme.text)),
            ]);
            let search_bar = Paragraph::new(search_line);
            frame.render_widget(search_bar, chunks[1]);
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
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.pending_g = false;
                let len = self.task_count();
                if len > 0 && self.cursor < len - 1 {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.pending_g = false;
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Char('G') if modifiers.contains(KeyModifiers::SHIFT) => {
                self.pending_g = false;
                let len = self.task_count();
                if len > 0 {
                    self.cursor = len - 1;
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
                true
            }
            KeyCode::Esc => {
                self.pending_g = false;
                // Clear confirmed search filter.
                if !self.search.query().is_empty() {
                    self.search.cancel();
                    self.cursor = 0;
                    self.scroll_offset = 0;
                    return true;
                }
                false
            }
            KeyCode::Enter => {
                self.pending_g = false;
                // Open the selected task's log on archive.org.
                // Also store the URL in status_message for 5 seconds so it's
                // visible on headless systems where open::that() fails silently.
                if let Ok(state) = self.s3_state.lock() {
                    let filtered: Vec<_> = state
                        .tasks
                        .iter()
                        .filter(|t| {
                            self.search.matches(&t.identifier) || self.search.matches(&t.cmd)
                        })
                        .collect();
                    if let Some(task) = filtered.get(self.cursor) {
                        let url = format!("https://archive.org/{}", task.task_id);
                        #[cfg(not(test))]
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

        if self.pending_g && self.pending_g_at.elapsed() > Duration::from_millis(500) {
            self.pending_g = false;
        }
    }

    fn status_text(&self) -> Option<&str> {
        self.status_message.as_ref().map(|(url, _)| url.as_str())
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("G", "end"),
            ("gg", "top"),
            ("/", "search"),
            ("Enter", "task log"),
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
                submittime: "2026-03-19 14:01:23".into(),
                task_id: 100000001,
            },
            S3TaskEntry {
                identifier: "item-b".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "queued".into(),
                submittime: "2026-03-19 14:01:25".into(),
                task_id: 100000002,
            },
            S3TaskEntry {
                identifier: "item-c".into(),
                cmd: "s3-put".into(),
                submitter: "test@example.com".into(),
                status: "queued".into(),
                submittime: "2026-03-19 14:01:30".into(),
                task_id: 100000003,
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
        assert!(!hints.iter().any(|(k, _)| *k == "u"));
        assert!(hints.iter().any(|(k, v)| *k == "Enter" && *v == "task log"));
    }

    #[test]
    fn test_enter_sets_status_message() {
        let mut tab = TasksTab::new(make_s3_state());
        // No status message initially
        assert!(tab.status_text().is_none());
        // Press Enter — item-a is at cursor 0, task_id 100000001
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // Status message should now be set with task_id URL
        let text = tab.status_text();
        assert!(text.is_some());
        assert!(text.unwrap().contains("archive.org/100000001"));
    }

    #[test]
    fn test_all_tasks_visible_without_filter() {
        let s3 = make_s3_state();
        // All tasks should be visible — filtering is done in upload_app.rs now.
        let tab = TasksTab::new(s3.clone());
        assert_eq!(tab.task_count(), 3);
    }

    #[test]
    fn test_tick_clears_expired_status_message() {
        use std::time::{Duration, Instant};
        let mut tab = TasksTab::new(make_s3_state());
        // Manually inject an old status message (6 seconds ago)
        tab.status_message = Some((
            "https://archive.org/100000001".to_string(),
            Instant::now() - Duration::from_secs(6),
        ));
        assert!(tab.status_text().is_some());
        tab.tick();
        assert!(tab.status_text().is_none());
    }

    #[test]
    fn test_jump_to_end() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, 2);
    }

    #[test]
    fn test_gg_jump_to_top() {
        let mut tab = TasksTab::new(make_s3_state());
        // Move to end first
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, 2);
        // gg to top
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(tab.pending_g);
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
        assert!(!tab.pending_g);
    }

    #[test]
    fn test_esc_clears_confirmed_search() {
        let mut tab = TasksTab::new(make_s3_state());
        // Activate search, type, confirm
        tab.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!tab.search.is_active());
        assert_eq!(tab.search.query(), "a");
        // Esc in normal mode clears the confirmed search
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(tab.search.query().is_empty());
    }

    #[test]
    fn test_arrow_keys() {
        let mut tab = TasksTab::new(make_s3_state());
        tab.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
    }
}
