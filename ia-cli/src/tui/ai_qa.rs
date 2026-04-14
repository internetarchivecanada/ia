//! QA dashboard — interactive TUI for reviewing AI-extracted metadata QA results.
//!
//! Three-panel layout: header (progress), extracted metadata + QA results, status bar.
//! Uses the shared [`Dashboard`] trait from [`super::framework`].
//!
//! Wired into the CLI via `ia ai qa --dashboard`.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};

use ia_core::ai::qa::{FieldVerdict, QaResult, QaVerdict};

use super::framework::Dashboard;

/// State for the QA review dashboard.
pub struct QaDashboardState {
    /// Items waiting to be reviewed.
    pub pending: Vec<QaResult>,
    /// Currently displayed item.
    pub current: Option<QaResult>,
    /// Index of the selected field in the right panel.
    pub selected_field: usize,
    /// Whether the right panel has focus (left = extracted, right = QA results).
    pub focus_right: bool,
    /// Counts by verdict.
    pub pass_count: u64,
    pub fail_count: u64,
    pub review_count: u64,
    /// Total items expected.
    pub items_total: u64,
    /// Items processed so far.
    pub items_reviewed: u64,
    /// Whether user quit.
    pub quit: bool,
    /// Whether all items are done.
    pub done: bool,
}

impl QaDashboardState {
    pub fn new(items_total: u64) -> Self {
        Self {
            pending: Vec::new(),
            current: None,
            selected_field: 0,
            focus_right: true,
            pass_count: 0,
            fail_count: 0,
            review_count: 0,
            items_total,
            items_reviewed: 0,
            quit: false,
            done: false,
        }
    }

    /// Add a QA result to the pending queue.
    pub fn push_result(&mut self, result: QaResult) {
        self.pending.push(result);
        if self.current.is_none() {
            self.advance();
        }
    }

    /// Advance to the next item from the pending queue.
    pub fn advance(&mut self) {
        if let Some(current) = self.current.take() {
            // Count the finished item
            match current.verdict {
                QaVerdict::Pass => self.pass_count += 1,
                QaVerdict::Fail => self.fail_count += 1,
                QaVerdict::NeedsReview => self.review_count += 1,
            }
            self.items_reviewed += 1;
        }

        self.current = if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        };
        self.selected_field = 0;
    }

    /// Number of fields in the current item.
    fn field_count(&self) -> usize {
        self.current.as_ref().map(|r| r.fields.len()).unwrap_or(0)
    }

    fn move_up(&mut self) {
        if self.selected_field > 0 {
            self.selected_field -= 1;
        }
    }

    fn move_down(&mut self) {
        let max = self.field_count().saturating_sub(1);
        if self.selected_field < max {
            self.selected_field += 1;
        }
    }
}

impl Dashboard for QaDashboardState {
    fn draw(&self, frame: &mut ratatui::Frame) {
        let area = frame.area();

        // Three-part vertical layout: header, main, status bar
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Min(10),   // main content
                Constraint::Length(1), // status bar
            ])
            .split(area);

        draw_header(self, frame, chunks[0]);
        draw_main(self, frame, chunks[1]);
        draw_status_bar(self, frame, chunks[2]);
    }

    fn handle_key(&mut self, code: KeyCode, _modifiers: KeyModifiers) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_up();
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_down();
                true
            }
            KeyCode::Tab => {
                self.focus_right = !self.focus_right;
                true
            }
            KeyCode::Char('s') | KeyCode::Enter => {
                // Skip / advance to next item
                self.advance();
                true
            }
            KeyCode::Char('a') => {
                // Accept all — advance
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn is_done(&self) -> bool {
        self.done && self.current.is_none() && self.pending.is_empty()
    }

    fn quit_requested(&self) -> bool {
        self.quit
    }
}

fn draw_header(state: &QaDashboardState, frame: &mut ratatui::Frame, area: Rect) {
    let total = state.items_total.max(1);
    let reviewed = state.items_reviewed;
    let pct = (reviewed as f64 / total as f64).min(1.0);

    let label = format!(
        "Item {}/{} | Pass: {}  Review: {}  Fail: {} | Pending: {}",
        reviewed + if state.current.is_some() { 1 } else { 0 },
        total,
        state.pass_count,
        state.review_count,
        state.fail_count,
        state.pending.len(),
    );

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("QA Dashboard"))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(pct)
        .label(label);

    frame.render_widget(gauge, area);
}

fn draw_main(state: &QaDashboardState, frame: &mut ratatui::Frame, area: Rect) {
    // Split into left (extracted metadata) and right (QA results) panels
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    draw_metadata_panel(state, frame, chunks[0]);
    draw_qa_panel(state, frame, chunks[1]);
}

fn draw_metadata_panel(state: &QaDashboardState, frame: &mut ratatui::Frame, area: Rect) {
    let border_style = if !state.focus_right {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title("Extracted Metadata");

    let Some(result) = &state.current else {
        let empty = Paragraph::new("Waiting for QA results...").block(block);
        frame.render_widget(empty, area);
        return;
    };

    let mut lines = Vec::new();

    // Show identifier and models
    lines.push(Line::from(vec![
        Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &result.identifier,
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Extraction: ", Style::default().fg(Color::DarkGray)),
        Span::raw(&result.extraction_model),
        Span::styled(" → ", Style::default().fg(Color::DarkGray)),
        Span::raw(&result.qa_model),
        Span::styled(" (QA)", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    // Show extracted field values
    for (name, field_result) in &result.fields {
        let value_str = format_value(&field_result.extracted_value);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{name}: "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(truncate_str(&value_str, 50)),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_qa_panel(state: &QaDashboardState, frame: &mut ratatui::Frame, area: Rect) {
    let border_style = if state.focus_right {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title("QA Results");

    let Some(result) = &state.current else {
        let empty = Paragraph::new("").block(block);
        frame.render_widget(empty, area);
        return;
    };

    let items: Vec<ListItem> = result
        .fields
        .iter()
        .enumerate()
        .map(|(i, (name, field))| {
            let icon = match field.verdict {
                FieldVerdict::Correct => "✓",
                FieldVerdict::Incorrect => "✗",
                FieldVerdict::Uncertain => "?",
            };
            let icon_color = match field.verdict {
                FieldVerdict::Correct => Color::Green,
                FieldVerdict::Incorrect => Color::Red,
                FieldVerdict::Uncertain => Color::Yellow,
            };
            let verdict_str = match field.verdict {
                FieldVerdict::Correct => "correct",
                FieldVerdict::Incorrect => "incorrect",
                FieldVerdict::Uncertain => "uncertain",
            };

            let style = if i == state.selected_field && state.focus_right {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                Span::styled(
                    format!("{name:<20}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:.0}%  ", field.confidence * 100.0)),
                Span::styled(verdict_str, Style::default().fg(icon_color)),
            ]);

            ListItem::new(line).style(style)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_status_bar(state: &QaDashboardState, frame: &mut ratatui::Frame, area: Rect) {
    let help = if state.current.is_some() {
        "[a]ccept all  [s]kip  [↑↓/jk] navigate  [Tab] switch panel  [q]uit"
    } else {
        "Waiting for results... [q]uit"
    };

    // Overall verdict badge for current item
    let verdict_text = state.current.as_ref().map(|r| match r.verdict {
        QaVerdict::Pass => {
            Span::styled(" PASS ", Style::default().fg(Color::Black).bg(Color::Green))
        }
        QaVerdict::Fail => Span::styled(" FAIL ", Style::default().fg(Color::White).bg(Color::Red)),
        QaVerdict::NeedsReview => Span::styled(
            " REVIEW ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
    });

    let mut spans = Vec::new();
    if let Some(badge) = verdict_text {
        spans.push(badge);
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(help, Style::default().fg(Color::DarkGray)));

    let bar = Paragraph::new(Line::from(spans));
    frame.render_widget(bar, area);
}

fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Null => "—".to_string(),
        other => other.to_string(),
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max_len.saturating_sub(3))
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ia_core::ai::qa::{FieldQaResult, QaResult, QaVerdict};
    use indexmap::IndexMap;

    fn make_result(identifier: &str, verdict: QaVerdict) -> QaResult {
        let mut fields = IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Test Title"),
                verdict: FieldVerdict::Correct,
                confidence: 0.95,
                suggested_correction: None,
                note: None,
            },
        );
        QaResult {
            identifier: identifier.to_string(),
            overall_confidence: 0.95,
            verdict,
            extraction_model: "gpt-5-nano".to_string(),
            qa_model: "claude-sonnet-4-6".to_string(),
            fields,
            token_usage: None,
            elapsed_ms: 100,
            existing_metadata: None,
            pages_sent: None,
        }
    }

    #[test]
    fn initial_state() {
        let state = QaDashboardState::new(10);
        assert!(state.current.is_none());
        assert_eq!(state.items_total, 10);
        assert_eq!(state.items_reviewed, 0);
        assert!(!state.quit);
    }

    #[test]
    fn push_result_sets_current() {
        let mut state = QaDashboardState::new(5);
        state.push_result(make_result("item1", QaVerdict::Pass));
        assert!(state.current.is_some());
        assert_eq!(state.current.as_ref().unwrap().identifier, "item1");
    }

    #[test]
    fn advance_counts_verdict() {
        let mut state = QaDashboardState::new(5);
        state.push_result(make_result("item1", QaVerdict::Pass));
        state.push_result(make_result("item2", QaVerdict::Fail));
        state.advance(); // finish item1
        assert_eq!(state.pass_count, 1);
        assert_eq!(state.items_reviewed, 1);
        assert_eq!(state.current.as_ref().unwrap().identifier, "item2");
    }

    #[test]
    fn navigate_fields() {
        let mut state = QaDashboardState::new(5);
        let mut result = make_result("item1", QaVerdict::Pass);
        result.fields.insert(
            "date".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("2020"),
                verdict: FieldVerdict::Correct,
                confidence: 0.9,
                suggested_correction: None,
                note: None,
            },
        );
        state.push_result(result);
        assert_eq!(state.selected_field, 0);
        state.move_down();
        assert_eq!(state.selected_field, 1);
        state.move_down(); // at max, stays
        assert_eq!(state.selected_field, 1);
        state.move_up();
        assert_eq!(state.selected_field, 0);
    }

    #[test]
    fn quit_requested_after_q() {
        let mut state = QaDashboardState::new(5);
        assert!(!state.quit_requested());
        state.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(state.quit_requested());
    }

    #[test]
    fn format_value_string() {
        assert_eq!(format_value(&serde_json::json!("hello")), "hello");
    }

    #[test]
    fn format_value_array() {
        let val = serde_json::json!(["a", "b"]);
        assert_eq!(format_value(&val), "[a, b]");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate_str("hello world foo", 10), "hello w...");
    }
}
