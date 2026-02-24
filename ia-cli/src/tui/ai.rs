use std::io::IsTerminal;
use std::time::{Duration, Instant};

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;

use ia_core::ai::types::{ChangeCategory, ChangeStatus, ItemAnalysis};

/// Guard that restores the terminal on drop (even during panic).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
        let _ = std::io::stdout().execute(Show);
    }
}

/// AI TUI review dashboard state.
#[derive(Debug)]
pub struct AiTuiState {
    /// Queue of items waiting for review.
    pub pending_items: Vec<ItemAnalysis>,
    /// Currently displayed item (taken from pending_items).
    pub current_item: Option<ItemAnalysis>,
    /// Index of the selected change in the right panel.
    pub selected_change: usize,
    /// Which panel has focus (left = metadata, right = changes).
    pub focus_right: bool,
    /// Whether to show file list in the left panel.
    pub show_files: bool,
    /// Scroll offset for the left metadata panel.
    pub meta_scroll: usize,
    /// Items processed so far.
    pub items_reviewed: u64,
    /// Total items expected.
    pub items_total: u64,
    /// Total tokens used.
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    /// Items that were confirmed (sent to writer).
    pub confirmed_items: Vec<ItemAnalysis>,
    /// Whether the user has quit.
    pub quit_requested: bool,
    /// Whether all items have been processed.
    pub done: bool,
    /// Start time for elapsed display.
    pub started_at: Instant,
    /// Edit mode: if Some, the user is editing a change value.
    pub edit_buffer: Option<String>,
}

impl AiTuiState {
    pub fn new(total_items: u64) -> Self {
        Self {
            pending_items: Vec::new(),
            current_item: None,
            selected_change: 0,
            focus_right: true,
            show_files: false,
            meta_scroll: 0,
            items_reviewed: 0,
            items_total: total_items,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            confirmed_items: Vec::new(),
            quit_requested: false,
            done: false,
            started_at: Instant::now(),
            edit_buffer: None,
        }
    }

    /// Load the next item for review. Returns false if no items available.
    pub fn advance(&mut self) -> bool {
        if let Some(item) = self.pending_items.pop() {
            if let Some(ref usage) = item.token_usage {
                self.total_prompt_tokens += usage.prompt_tokens;
                self.total_completion_tokens += usage.completion_tokens;
            }
            self.current_item = Some(item);
            self.selected_change = 0;
            self.meta_scroll = 0;
            true
        } else {
            false
        }
    }

    /// Accept the currently selected change.
    pub fn accept_selected(&mut self) {
        if let Some(ref mut item) = self.current_item {
            if self.selected_change < item.changes.len() {
                item.changes[self.selected_change].status = ChangeStatus::Accepted;
            }
        }
    }

    /// Reject the currently selected change.
    pub fn reject_selected(&mut self) {
        if let Some(ref mut item) = self.current_item {
            if self.selected_change < item.changes.len() {
                item.changes[self.selected_change].status = ChangeStatus::Rejected;
            }
        }
    }

    /// Accept all changes for the current item.
    pub fn accept_all(&mut self) {
        if let Some(ref mut item) = self.current_item {
            for change in &mut item.changes {
                if change.status == ChangeStatus::Pending {
                    change.status = ChangeStatus::Accepted;
                }
            }
        }
    }

    /// Start editing the selected change (enter edit mode).
    pub fn start_edit(&mut self) {
        if let Some(ref item) = self.current_item {
            if self.selected_change < item.changes.len() {
                let value = &item.changes[self.selected_change].new_value;
                self.edit_buffer = Some(
                    serde_json::to_string(value).unwrap_or_default(),
                );
            }
        }
    }

    /// Confirm edit and apply to the change.
    pub fn confirm_edit(&mut self) {
        if let Some(buffer) = self.edit_buffer.take() {
            if let Some(ref mut item) = self.current_item {
                if self.selected_change < item.changes.len() {
                    if let Ok(value) = serde_json::from_str(&buffer) {
                        item.changes[self.selected_change].status =
                            ChangeStatus::Edited(value);
                    } else {
                        // Treat as raw string
                        item.changes[self.selected_change].status =
                            ChangeStatus::Edited(serde_json::Value::String(buffer));
                    }
                }
            }
        }
    }

    /// Cancel edit mode.
    pub fn cancel_edit(&mut self) {
        self.edit_buffer = None;
    }

    /// Confirm the current item (send accepted changes, advance to next).
    pub fn confirm_item(&mut self) {
        if let Some(item) = self.current_item.take() {
            self.items_reviewed += 1;
            self.confirmed_items.push(item);
            if !self.advance() {
                // No more items ready; will wait for more or finish
            }
        }
    }

    /// Skip the current item (reject all changes, advance to next).
    pub fn skip_item(&mut self) {
        if let Some(mut item) = self.current_item.take() {
            for change in &mut item.changes {
                change.status = ChangeStatus::Rejected;
            }
            self.items_reviewed += 1;
            self.confirmed_items.push(item);
            if !self.advance() {
                // No more items ready
            }
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected_change > 0 {
            self.selected_change -= 1;
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if let Some(ref item) = self.current_item {
            if self.selected_change + 1 < item.changes.len() {
                self.selected_change += 1;
            }
        }
    }

    /// Number of pending changes in the current item.
    pub fn pending_count(&self) -> usize {
        self.current_item
            .as_ref()
            .map(|item| {
                item.changes
                    .iter()
                    .filter(|c| c.status == ChangeStatus::Pending)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Elapsed time as a formatted string.
    pub fn elapsed_str(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else {
            format!("{}m{}s", secs / 60, secs % 60)
        }
    }

    /// Handle a keyboard event. Returns true if the event was handled.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Edit mode has its own key handling
        if self.edit_buffer.is_some() {
            match code {
                KeyCode::Enter => self.confirm_edit(),
                KeyCode::Esc => self.cancel_edit(),
                KeyCode::Backspace => {
                    if let Some(ref mut buf) = self.edit_buffer {
                        buf.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(ref mut buf) = self.edit_buffer {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return true;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit_requested = true;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit_requested = true;
            }
            KeyCode::Char('A') => {
                self.accept_all();
                self.confirm_item();
            }
            KeyCode::Char('a') => self.accept_selected(),
            KeyCode::Char('r') => self.reject_selected(),
            KeyCode::Char('e') => self.start_edit(),
            KeyCode::Char('s') => self.skip_item(),
            KeyCode::Enter => self.confirm_item(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Tab => self.focus_right = !self.focus_right,
            KeyCode::Char('f') => self.show_files = !self.show_files,
            _ => return false,
        }
        true
    }
}

/// Draw the AI review TUI.
pub fn draw(f: &mut ratatui::Frame, state: &AiTuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(5),   // Main content
            Constraint::Length(1), // Status bar
        ])
        .split(f.area());

    draw_header(f, state, chunks[0]);
    draw_main(f, state, chunks[1]);
    draw_status_bar(f, state, chunks[2]);
}

fn draw_header(f: &mut ratatui::Frame, state: &AiTuiState, area: Rect) {
    let progress = if state.items_total > 0 {
        state.items_reviewed as f64 / state.items_total as f64
    } else {
        0.0
    };

    let total_tokens = state.total_prompt_tokens + state.total_completion_tokens;
    let tokens_str = if total_tokens > 0 {
        format!("  {} tokens", total_tokens)
    } else {
        String::new()
    };

    let ready = state.pending_items.len();
    let ready_str = if ready > 0 {
        format!("  {} ready", ready)
    } else {
        String::new()
    };

    let label = format!(
        "  ia ai  {}/{}  {}{}{}",
        state.items_reviewed,
        state.items_total,
        state.elapsed_str(),
        tokens_str,
        ready_str,
    );

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(progress.min(1.0))
        .label(label);

    f.render_widget(gauge, area);
}

fn draw_main(f: &mut ratatui::Frame, state: &AiTuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    draw_metadata_panel(f, state, chunks[0]);
    draw_changes_panel(f, state, chunks[1]);
}

fn draw_metadata_panel(f: &mut ratatui::Frame, state: &AiTuiState, area: Rect) {
    let border_style = if !state.focus_right {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = state
        .current_item
        .as_ref()
        .map(|i| format!(" Item: {} ", i.identifier))
        .unwrap_or_else(|| " No item ".to_string());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let Some(ref item) = state.current_item else {
        let waiting = Paragraph::new("Waiting for analysis...")
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(waiting, area);
        return;
    };

    // Build metadata display lines
    let mut lines = Vec::new();
    let metadata = &item.metadata;

    // Show known fields in order
    let field_order = [
        "title",
        "description",
        "date",
        "creator",
        "mediatype",
        "collection",
        "subject",
        "language",
    ];

    if let Some(meta_obj) = metadata.get("metadata") {
        for field in &field_order {
            let value = meta_obj.get(*field);
            let display = match value {
                Some(v) if v.is_null() => "(empty)".to_string(),
                Some(serde_json::Value::String(s)) => truncate_str(s, 60),
                Some(v) => {
                    let s = v.to_string();
                    truncate_str(&s, 60)
                }
                None => "(empty)".to_string(),
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}: ", field),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::raw(display),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

fn draw_changes_panel(f: &mut ratatui::Frame, state: &AiTuiState, area: Rect) {
    let border_style = if state.focus_right {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let change_count = state
        .current_item
        .as_ref()
        .map(|i| i.changes.len())
        .unwrap_or(0);

    let block = Block::default()
        .title(format!(" Suggested Changes ({}) ", change_count))
        .borders(Borders::ALL)
        .border_style(border_style);

    let Some(ref item) = state.current_item else {
        f.render_widget(block, area);
        return;
    };

    if item.changes.is_empty() {
        let no_changes = Paragraph::new("No changes suggested for this item.")
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(no_changes, area);
        return;
    }

    let items: Vec<ListItem> = item
        .changes
        .iter()
        .enumerate()
        .map(|(i, change)| {
            let is_selected = i == state.selected_change;
            let (icon, status_color) = match &change.status {
                ChangeStatus::Pending => ("○", Color::Yellow),
                ChangeStatus::Accepted => ("✓", Color::Green),
                ChangeStatus::Rejected => ("✗", Color::Red),
                ChangeStatus::Edited(_) => ("✎", Color::Cyan),
            };

            let category_icon = match change.category {
                ChangeCategory::Schema => "⚙",
                ChangeCategory::Content => "✏",
                ChangeCategory::CrossField => "↔",
                ChangeCategory::MissingField => "✚",
            };

            let old_display = change
                .old_value
                .as_ref()
                .map(format_value)
                .unwrap_or_else(|| "(empty)".to_string());

            let new_display = match &change.status {
                ChangeStatus::Edited(v) => format_value(v),
                _ => format_value(&change.new_value),
            };

            let style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!("{} {} ", icon, category_icon),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(
                        change.field.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(old_display, Style::default().fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::raw("    → "),
                    Span::styled(new_display, Style::default().fg(Color::Green)),
                ]),
            ];

            if !change.reason.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        &change.reason,
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }

            ListItem::new(lines).style(style)
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_status_bar(f: &mut ratatui::Frame, state: &AiTuiState, area: Rect) {
    let text = if let Some(buf) = &state.edit_buffer {
        Line::from(vec![
            Span::styled(" EDIT: ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(format!(" {} ", buf)),
            Span::styled(
                " Enter=confirm  Esc=cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        let pending = state.pending_count();
        let pending_str = if pending > 0 {
            format!(" ({} pending)", pending)
        } else {
            String::new()
        };
        Line::from(vec![
            Span::styled(" A", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("ccept All  "),
            Span::styled("a", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("ccept  "),
            Span::styled("r", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("eject  "),
            Span::styled("e", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("dit  "),
            Span::styled("s", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("kip  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("=confirm  "),
            Span::styled("q", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("uit  ↑↓=navigate"),
            Span::styled(pending_str, Style::default().fg(Color::DarkGray)),
        ])
    };

    let bar = Paragraph::new(text);
    f.render_widget(bar, area);
}

/// Truncate a string at a character boundary.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}...", &s[..end])
    }
}

/// Format a serde_json::Value for display (truncated).
fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            if s.chars().count() > 50 {
                format!("\"{}\"", truncate_str(s, 47))
            } else {
                format!("\"{}\"", s)
            }
        }
        serde_json::Value::Null => "(null)".to_string(),
        other => {
            let s = other.to_string();
            if s.chars().count() > 50 {
                truncate_str(&s, 47)
            } else {
                s
            }
        }
    }
}

/// Run the interactive AI review TUI.
///
/// Receives `ItemAnalysis` objects from the pipeline's analyzer via `analysis_rx`,
/// presents them for interactive review, and sends confirmed items to the
/// writer stage via `confirmed_tx`.
///
/// The `shutdown_tx` is used to signal the pipeline to stop immediately when
/// the user quits (so it doesn't keep fetching metadata or calling the LLM).
pub async fn run_ai_tui(
    mut analysis_rx: tokio::sync::mpsc::Receiver<ItemAnalysis>,
    confirmed_tx: tokio::sync::mpsc::Sender<ItemAnalysis>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    total_items: u64,
) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Interactive mode requires a terminal.\n\
             Hint: use --headless for non-interactive processing."
        );
    }

    let mut state = AiTuiState::new(total_items);

    // Set up terminal
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(100);

    loop {
        // Try to receive new items from the pipeline (non-blocking)
        loop {
            match analysis_rx.try_recv() {
                Ok(item) => {
                    state.pending_items.push(item);
                    if state.current_item.is_none() {
                        state.advance();
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    state.done = true;
                    break;
                }
            }
        }

        // Draw
        terminal.draw(|f| draw(f, &state))?;

        if state.quit_requested {
            break;
        }
        if state.done && state.current_item.is_none() && state.pending_items.is_empty() {
            break;
        }

        // Handle input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                state.handle_key(key.code, key.modifiers);

                // Send confirmed items to the writer
                let confirmed: Vec<ItemAnalysis> = state.confirmed_items.drain(..).collect();
                for item in confirmed {
                    if confirmed_tx.send(item).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Flush any remaining confirmed items before exiting
    for item in state.confirmed_items.drain(..) {
        let _ = confirmed_tx.send(item).await;
    }

    // Signal the pipeline to stop immediately (so source/analyzer don't
    // keep fetching/calling the LLM after the user has quit)
    let _ = shutdown_tx.send(true);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ia_core::ai::types::{ChangeCategory, MetadataChange};

    fn test_analysis() -> ItemAnalysis {
        ItemAnalysis {
            identifier: "test_item".to_string(),
            metadata: serde_json::json!({"metadata": {"title": "test"}}),
            changes: vec![
                MetadataChange {
                    field: "title".to_string(),
                    old_value: Some(serde_json::json!("test")),
                    new_value: serde_json::json!("Test"),
                    reason: "Capitalization".to_string(),
                    category: ChangeCategory::Content,
                    status: ChangeStatus::Pending,
                },
                MetadataChange {
                    field: "date".to_string(),
                    old_value: None,
                    new_value: serde_json::json!("2026-01-01"),
                    reason: "Added date".to_string(),
                    category: ChangeCategory::MissingField,
                    status: ChangeStatus::Pending,
                },
            ],
            token_usage: None,
            analyzed_at: "2026-02-23T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn new_state_starts_empty() {
        let state = AiTuiState::new(5);
        assert!(state.current_item.is_none());
        assert_eq!(state.items_reviewed, 0);
        assert_eq!(state.items_total, 5);
        assert!(!state.quit_requested);
    }

    #[test]
    fn advance_loads_item() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        assert!(state.advance());
        assert!(state.current_item.is_some());
        assert_eq!(state.current_item.as_ref().unwrap().identifier, "test_item");
    }

    #[test]
    fn advance_returns_false_when_empty() {
        let mut state = AiTuiState::new(1);
        assert!(!state.advance());
    }

    #[test]
    fn accept_selected_changes_status() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        state.accept_selected();
        assert_eq!(
            state.current_item.as_ref().unwrap().changes[0].status,
            ChangeStatus::Accepted
        );
    }

    #[test]
    fn reject_selected_changes_status() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        state.reject_selected();
        assert_eq!(
            state.current_item.as_ref().unwrap().changes[0].status,
            ChangeStatus::Rejected
        );
    }

    #[test]
    fn accept_all_accepts_pending_changes() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        state.accept_all();
        for change in &state.current_item.as_ref().unwrap().changes {
            assert_eq!(change.status, ChangeStatus::Accepted);
        }
    }

    #[test]
    fn accept_all_skips_non_pending() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        // Reject first change, then accept all
        state.reject_selected();
        state.accept_all();

        assert_eq!(
            state.current_item.as_ref().unwrap().changes[0].status,
            ChangeStatus::Rejected
        );
        assert_eq!(
            state.current_item.as_ref().unwrap().changes[1].status,
            ChangeStatus::Accepted
        );
    }

    #[test]
    fn move_up_down_navigation() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        assert_eq!(state.selected_change, 0);
        state.move_down();
        assert_eq!(state.selected_change, 1);
        state.move_down(); // Should not go past last
        assert_eq!(state.selected_change, 1);
        state.move_up();
        assert_eq!(state.selected_change, 0);
        state.move_up(); // Should not go below 0
        assert_eq!(state.selected_change, 0);
    }

    #[test]
    fn confirm_item_moves_to_confirmed() {
        let mut state = AiTuiState::new(2);
        state.pending_items.push(test_analysis());
        state.advance();

        state.accept_all();
        state.confirm_item();

        assert_eq!(state.items_reviewed, 1);
        assert_eq!(state.confirmed_items.len(), 1);
        assert!(state.current_item.is_none());
    }

    #[test]
    fn skip_item_rejects_all_and_advances() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        state.skip_item();

        assert_eq!(state.items_reviewed, 1);
        assert_eq!(state.confirmed_items.len(), 1);
        let confirmed = &state.confirmed_items[0];
        for change in &confirmed.changes {
            assert_eq!(change.status, ChangeStatus::Rejected);
        }
    }

    #[test]
    fn edit_mode_flow() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        // Enter edit mode
        state.start_edit();
        assert!(state.edit_buffer.is_some());

        // Type some characters
        state.handle_key(KeyCode::Char('H'), KeyModifiers::NONE);
        state.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);

        // Confirm edit
        state.confirm_edit();
        assert!(state.edit_buffer.is_none());
        assert!(matches!(
            state.current_item.as_ref().unwrap().changes[0].status,
            ChangeStatus::Edited(_)
        ));
    }

    #[test]
    fn cancel_edit() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        state.start_edit();
        assert!(state.edit_buffer.is_some());

        state.cancel_edit();
        assert!(state.edit_buffer.is_none());
        assert_eq!(
            state.current_item.as_ref().unwrap().changes[0].status,
            ChangeStatus::Pending
        );
    }

    #[test]
    fn keyboard_q_quits() {
        let mut state = AiTuiState::new(1);
        state.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(state.quit_requested);
    }

    #[test]
    fn keyboard_ctrl_c_quits() {
        let mut state = AiTuiState::new(1);
        state.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(state.quit_requested);
    }

    #[test]
    fn keyboard_tab_toggles_focus() {
        let mut state = AiTuiState::new(1);
        assert!(state.focus_right);
        state.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert!(!state.focus_right);
        state.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert!(state.focus_right);
    }

    #[test]
    fn keyboard_f_toggles_files() {
        let mut state = AiTuiState::new(1);
        assert!(!state.show_files);
        state.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(state.show_files);
    }

    #[test]
    fn pending_count_tracks_unreviewed() {
        let mut state = AiTuiState::new(1);
        state.pending_items.push(test_analysis());
        state.advance();

        assert_eq!(state.pending_count(), 2);
        state.accept_selected();
        assert_eq!(state.pending_count(), 1);
        state.move_down();
        state.reject_selected();
        assert_eq!(state.pending_count(), 0);
    }
}
