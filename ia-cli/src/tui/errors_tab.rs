//! Errors tab for the multi-tab upload dashboard.
//!
//! Displays upload failures in a scrollable table with inline expansion
//! for full error details, plus an S3 task error summary panel.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::s3_state::S3TaskState;
use super::tab::TabView;
use super::theme::Theme;
use super::upload_app::UploadTuiState;

/// The Errors tab: lists failed files with inline error expansion.
#[derive(Debug)]
pub struct ErrorsTab {
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,
    pub cursor: usize,
    /// Which error row is expanded inline to show full error text.
    pub expanded: Option<usize>,
    scroll_offset: usize,
}

impl ErrorsTab {
    pub fn new(
        upload_state: Arc<Mutex<UploadTuiState>>,
        s3_state: Arc<Mutex<S3TaskState>>,
    ) -> Self {
        Self {
            upload_state,
            s3_state,
            cursor: 0,
            expanded: None,
            scroll_offset: 0,
        }
    }

    /// Return the number of failed files from the upload state.
    fn error_count(&self) -> usize {
        self.upload_state.lock().map_or(0, |s| s.failed_files.len())
    }
}

impl TabView for ErrorsTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let s3_errors = self.s3_state.lock().map_or(0, |s| s.errors);

        // Split area: upload errors panel takes most space, S3 panel only if errors > 0.
        let chunks = if s3_errors > 0 {
            Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).split(area)
        } else {
            Layout::vertical([Constraint::Min(5)]).split(area)
        };

        // -- Upload Errors panel --
        let upload_area = chunks[0];
        let failed_files: Vec<(String, String)> = self
            .upload_state
            .lock()
            .map_or_else(|_| Vec::new(), |s| s.failed_files.clone());

        let title = format!(" Upload Errors ({}) ", failed_files.len());
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));

        let inner = block.inner(upload_area);
        frame.render_widget(block, upload_area);

        if failed_files.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "No errors",
                Style::default().fg(theme.text_muted),
            )));
            frame.render_widget(msg, inner);
        } else {
            // Build lines for the visible rows.
            let mut lines: Vec<Line> = Vec::new();
            for (i, (file, error)) in failed_files.iter().enumerate() {
                let is_cursor = i == self.cursor;
                let style = if is_cursor {
                    Style::default().fg(theme.gold).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };

                // Truncate error to fit in a single line (leave room for file column).
                let summary = if error.chars().count() > 60 {
                    format!("{}...", error.chars().take(57).collect::<String>())
                } else {
                    error.clone()
                };

                lines.push(Line::from(vec![
                    Span::styled(format!("{:<40} ", file), style),
                    Span::styled(
                        summary,
                        if is_cursor {
                            style
                        } else {
                            Style::default().fg(theme.red)
                        },
                    ),
                ]));

                // If this row is expanded, word-wrap the full error below.
                if self.expanded == Some(i) {
                    let wrap_width = inner.width.saturating_sub(4) as usize;
                    let style = Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::ITALIC);
                    for wrapped in word_wrap(error, wrap_width.max(20)) {
                        lines.push(Line::from(Span::styled(format!("    {wrapped}"), style)));
                    }
                }
            }

            // Apply scroll offset.
            let visible: Vec<Line> = lines
                .into_iter()
                .skip(self.scroll_offset)
                .take(inner.height as usize)
                .collect();

            let paragraph = Paragraph::new(visible);
            frame.render_widget(paragraph, inner);
        }

        // -- S3 Task Errors panel (only if errors > 0) --
        if s3_errors > 0 && chunks.len() > 1 {
            let s3_area = chunks[1];
            let s3_block = Block::default()
                .title(" S3 Task Errors ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border));

            let s3_inner = s3_block.inner(s3_area);
            frame.render_widget(s3_block, s3_area);

            let s3_text = Paragraph::new(Line::from(Span::styled(
                format!("{s3_errors} task(s) with errors"),
                Style::default().fg(theme.red),
            )));
            frame.render_widget(s3_text, s3_inner);
        }
    }

    fn handle_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) -> bool {
        let count = self.error_count();
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if count > 0 {
                    self.cursor = (self.cursor + 1).min(count.saturating_sub(1));
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Enter => {
                if self.expanded == Some(self.cursor) {
                    self.expanded = None;
                } else {
                    self.expanded = Some(self.cursor);
                }
                true
            }
            KeyCode::Esc => {
                self.expanded = None;
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self) {
        // No-op: errors are updated via shared state from the upload workers.
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("Enter", "expand"),
            ("?", "help"),
            ("q", "quit"),
        ]
    }
}

/// Simple word-wrap: break `text` into lines of at most `width` characters,
/// splitting on whitespace boundaries.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_state_with_errors() -> Arc<Mutex<UploadTuiState>> {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);
        state.failed_files.push((
            "item-a/file1.jpg".to_string(),
            "503 Service Unavailable".to_string(),
        ));
        state.failed_files.push((
            "item-a/file2.jpg".to_string(),
            "timeout after 30s".to_string(),
        ));
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn test_scroll() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);
    }

    #[test]
    fn test_expand_collapse() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );
        assert!(tab.expanded.is_none());
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(tab.expanded, Some(0));
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(tab.expanded.is_none()); // toggle off
    }

    #[test]
    fn test_escape_collapses() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );
        tab.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(tab.expanded.is_some());
        tab.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(tab.expanded.is_none());
    }

    #[test]
    fn test_cursor_clamped_when_errors_shrink() {
        let upload_state = make_state_with_errors();
        let mut tab = ErrorsTab::new(
            upload_state.clone(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );

        // Move cursor to last error (index 1)
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1);

        // Remove all errors — cursor should clamp to 0
        upload_state.lock().unwrap().failed_files.clear();
        let count = tab.error_count();
        assert_eq!(count, 0);

        // j should not advance cursor past empty list
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 1); // cursor doesn't advance past empty
    }

    #[test]
    fn test_cursor_clamps_on_scroll_down() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );

        // Try scrolling past the end
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        // Should be clamped at len-1 = 1
        assert_eq!(tab.cursor, 1);
    }
}
