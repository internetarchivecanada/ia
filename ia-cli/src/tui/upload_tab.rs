//! Upload tab implementation for the multi-tab dashboard.
//!
//! Implements [`TabView`] for the Upload tab, rendering S3 task status,
//! per-item progress, active file transfers with progress bars, and a
//! throughput sparkline. Migrates the existing upload panel rendering to
//! the new themed, tabbed layout.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use ia_core::upload::UploadProgressStatus;

use super::s3_state::S3TaskState;
use super::tab::TabView;
use super::theme::Theme;
use super::upload_app::{UploadItemStatus, UploadTuiState};
use super::widgets;

// ---------------------------------------------------------------------------
// Focus panel
// ---------------------------------------------------------------------------

/// Which panel within the Upload tab currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Items,
    Transfers,
}

// ---------------------------------------------------------------------------
// Upload tab
// ---------------------------------------------------------------------------

/// Upload tab state: wraps shared upload and S3 task state, plus local UI
/// state for panel focus and scroll positions.
#[derive(Debug)]
pub struct UploadTab {
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,
    pub focused_panel: FocusPanel,
    pub items_scroll: usize,
    pub transfers_scroll: usize,
}

impl UploadTab {
    /// Create a new Upload tab with the given shared state.
    pub fn new(
        upload_state: Arc<Mutex<UploadTuiState>>,
        s3_state: Arc<Mutex<S3TaskState>>,
    ) -> Self {
        Self {
            upload_state,
            s3_state,
            focused_panel: FocusPanel::Items,
            items_scroll: 0,
            transfers_scroll: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// TabView impl
// ---------------------------------------------------------------------------

impl TabView for UploadTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Lock shared state for the duration of the draw.
        let upload = match self.upload_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let s3 = match self.s3_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        // Vertical layout: S3 panel | Items+Transfers | Throughput
        let chunks = Layout::vertical([
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);

        // ── S3 Tasks panel ──────────────────────────────────────────
        widgets::draw_s3_panel(
            frame,
            chunks[0],
            theme,
            &widgets::S3PanelData {
                queued: s3.queued,
                running: s3.running,
                errors: s3.errors,
                global_count: s3.global_count,
                rate_limited: s3.is_rate_limited,
                seconds_ago: s3.seconds_since_poll(),
            },
        );

        // ── Items + Transfers (horizontal 50/50) ────────────────────
        let h_chunks = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);

        draw_items_panel(
            frame,
            h_chunks[0],
            theme,
            &upload,
            self.focused_panel == FocusPanel::Items,
            self.items_scroll,
        );
        draw_transfers_panel(
            frame,
            h_chunks[1],
            theme,
            &upload,
            self.focused_panel == FocusPanel::Transfers,
            self.transfers_scroll,
        );

        // ── Throughput sparkline ────────────────────────────────────
        widgets::draw_themed_throughput_panel(frame, chunks[2], theme, upload.throughput.history());
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let _ = modifiers;
        match code {
            KeyCode::Tab => {
                self.focused_panel = match self.focused_panel {
                    FocusPanel::Items => FocusPanel::Transfers,
                    FocusPanel::Transfers => FocusPanel::Items,
                };
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Ok(state) = self.upload_state.lock() {
                    match self.focused_panel {
                        FocusPanel::Items => {
                            let max = state.items.len().saturating_sub(1);
                            self.items_scroll = self.items_scroll.saturating_add(1).min(max);
                        }
                        FocusPanel::Transfers => {
                            let max = state.active_files.len().saturating_sub(1);
                            self.transfers_scroll =
                                self.transfers_scroll.saturating_add(1).min(max);
                        }
                    }
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focused_panel {
                    FocusPanel::Items => {
                        self.items_scroll = self.items_scroll.saturating_sub(1);
                    }
                    FocusPanel::Transfers => {
                        self.transfers_scroll = self.transfers_scroll.saturating_sub(1);
                    }
                }
                true
            }
            KeyCode::Enter => {
                // Open the selected item on archive.org in the default browser.
                if let Ok(state) = self.upload_state.lock() {
                    if let Some(item) = state.items.get(self.items_scroll) {
                        let url = format!("https://archive.org/details/{}", item.identifier);
                        let _ = open::that(url);
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self) {
        // No-op — upload state is updated externally via progress callbacks.
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("Tab", "panel"),
            ("Enter", "history"),
            ("?", "help"),
            ("q", "quit"),
        ]
    }
}

// ---------------------------------------------------------------------------
// Items panel
// ---------------------------------------------------------------------------

/// Render the Items panel: per-item status list with completion icons,
/// bytes uploaded, and file counts. The active (uploading) item gets a
/// gold left border; completed items are green; failed items are red.
fn draw_items_panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    state: &UploadTuiState,
    focused: bool,
    scroll: usize,
) {
    let border_color = if focused { theme.gold } else { theme.border };
    let block = Block::default()
        .title(Span::styled(
            " Items ",
            Style::default().fg(theme.maroon_bright),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(visible_height);

    for item in state.items.iter().skip(scroll) {
        if lines.len() >= visible_height {
            break;
        }

        let is_active = matches!(
            item.status,
            UploadItemStatus::Uploading
                | UploadItemStatus::Verifying
                | UploadItemStatus::RateLimited
        );

        // Status icon
        let (icon, icon_color) = match &item.status {
            UploadItemStatus::Pending => ("\u{00b7}", theme.text_muted), // ·
            UploadItemStatus::Verifying => ("\u{25b8}", theme.gold),     // ▸
            UploadItemStatus::Uploading => ("\u{25b8}", theme.gold),     // ▸
            UploadItemStatus::RateLimited => ("\u{23f8}", theme.gold),   // ⏸
            UploadItemStatus::Complete => ("\u{2713}", theme.green),     // ✓
            UploadItemStatus::Failed(_) => ("\u{2717}", theme.red),      // ✗
        };

        // File counts
        let files_info = if item.files_total > 0 {
            format!(
                "{}/{}",
                item.files_completed + item.files_skipped + item.files_failed,
                item.files_total,
            )
        } else {
            "\u{2014}".to_string() // —
        };

        // Bytes uploaded
        let bytes_info = if item.bytes_uploaded > 0 {
            widgets::format_bytes(item.bytes_uploaded)
        } else {
            "\u{2014}".to_string() // —
        };

        let name = widgets::truncate_tail(&item.identifier, 25);

        // Active item gets gold left border indicator
        let left_border = if is_active {
            Span::styled("\u{2503} ", Style::default().fg(theme.gold)) // ┃
        } else {
            Span::raw("  ")
        };

        let name_color = if is_active {
            theme.gold
        } else if matches!(item.status, UploadItemStatus::Complete) {
            theme.green
        } else if matches!(item.status, UploadItemStatus::Failed(_)) {
            theme.red
        } else {
            theme.text
        };

        lines.push(Line::from(vec![
            left_border,
            Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
            Span::styled(name, Style::default().fg(name_color)),
            Span::raw("  "),
            Span::styled(
                format!("{:>8}", files_info),
                Style::default().fg(theme.text_muted),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>10}", bytes_info),
                Style::default().fg(theme.text_muted),
            ),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " Waiting for uploads...",
            Style::default().fg(theme.text_muted),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Transfers panel
// ---------------------------------------------------------------------------

/// Render the Transfers panel: active file uploads with progress bars.
/// Rate-limited files show `⏸ rate-limited` instead of a progress bar.
/// Below a `──` divider, completed files are shown (up to 10).
fn draw_transfers_panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    state: &UploadTuiState,
    focused: bool,
    scroll: usize,
) {
    let border_color = if focused { theme.gold } else { theme.border };
    let block = Block::default()
        .title(Span::styled(
            format!(" Transfers ({}) ", state.active_files.len()),
            Style::default().fg(theme.maroon_bright),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(visible_height);

    // Active file uploads, sorted by name.
    let mut active: Vec<_> = state.active_files.values().collect();
    active.sort_by(|a, b| a.name.cmp(&b.name));

    for fp in active.iter().skip(scroll) {
        if lines.len() >= visible_height {
            break;
        }

        let name = widgets::truncate_tail(&fp.name, 20);

        if matches!(
            fp.status,
            UploadProgressStatus::WaitingRateLimit | UploadProgressStatus::Retrying
        ) {
            // Rate-limited: show pause icon instead of progress bar.
            lines.push(Line::from(vec![
                Span::styled(" \u{23f8} ", Style::default().fg(theme.gold)),
                Span::styled(name, Style::default().fg(theme.text)),
                Span::styled("  rate-limited", Style::default().fg(theme.gold)),
            ]));
        } else {
            // Normal: progress bar.
            let progress = if fp.total_bytes > 0 {
                fp.bytes_sent as f64 / fp.total_bytes as f64
            } else {
                0.0
            };

            let bar_width = 16usize;
            let filled = (progress * bar_width as f64) as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar = format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty),);

            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(bar, Style::default().fg(theme.green)),
                Span::raw(format!(" {:>5.1}% ", progress * 100.0)),
                Span::styled(name, Style::default().fg(theme.text)),
            ]));
        }
    }

    // Divider + completed files (up to 10), if space permits.
    if lines.len() < visible_height && !state.completed_files.is_empty() {
        // Divider line
        if lines.len() + 1 < visible_height {
            let divider_width = inner.width as usize;
            let divider = "\u{2500}".repeat(divider_width); // ─
            lines.push(Line::from(Span::styled(
                divider,
                Style::default().fg(theme.border),
            )));
        }

        let remaining_space = visible_height.saturating_sub(lines.len());
        let completed_to_show = remaining_space.min(10).min(state.completed_files.len());

        for name in state.completed_files.iter().rev().take(completed_to_show) {
            if lines.len() >= visible_height {
                break;
            }
            let display_name = widgets::truncate_tail(name, 25);
            lines.push(Line::from(vec![
                Span::styled(" \u{2713} ", Style::default().fg(theme.green)),
                Span::styled(display_name, Style::default().fg(theme.text_muted)),
            ]));
        }
    }

    // Empty state
    if lines.is_empty() {
        let msg = if state.done {
            " All uploads complete."
        } else {
            " Waiting for transfers..."
        };
        lines.push(Line::from(Span::styled(
            msg,
            Style::default()
                .fg(if state.done {
                    theme.green
                } else {
                    theme.text_muted
                })
                .add_modifier(if state.done {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_state() -> Arc<Mutex<UploadTuiState>> {
        Arc::new(Mutex::new(UploadTuiState::new(&[
            "item-a".to_string(),
            "item-b".to_string(),
        ])))
    }

    fn make_s3_state() -> Arc<Mutex<S3TaskState>> {
        Arc::new(Mutex::new(S3TaskState::new()))
    }

    #[test]
    fn test_initial_focus() {
        let tab = UploadTab::new(make_state(), make_s3_state());
        assert_eq!(tab.focused_panel, FocusPanel::Items);
    }

    #[test]
    fn test_tab_cycles_focus() {
        let mut tab = UploadTab::new(make_state(), make_s3_state());
        assert_eq!(tab.focused_panel, FocusPanel::Items);
        tab.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(tab.focused_panel, FocusPanel::Transfers);
        tab.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(tab.focused_panel, FocusPanel::Items);
    }

    #[test]
    fn test_j_k_scrolls() {
        let mut tab = UploadTab::new(make_state(), make_s3_state());
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 1);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 0);
        tab.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 0);
    }

    #[test]
    fn test_key_hints() {
        let tab = UploadTab::new(make_state(), make_s3_state());
        let hints = tab.key_hints();
        assert!(hints.iter().any(|(k, _)| *k == "j/k"));
        assert!(hints.iter().any(|(k, _)| *k == "Tab"));
        assert!(hints.iter().any(|(k, _)| *k == "Enter"));
    }

    #[test]
    fn test_items_scroll_clamped() {
        let mut tab = UploadTab::new(make_state(), make_s3_state());
        // State has 2 items (item-a, item-b), so max scroll = 1
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 1);
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 1); // clamped at len-1
        tab.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(tab.items_scroll, 1); // still clamped
    }
}
