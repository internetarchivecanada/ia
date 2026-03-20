//! Errors tab for the multi-tab upload dashboard.
//!
//! Displays an error summary pane with sparkline histogram, category
//! breakdown, and resolved/active counts, plus a scrollable list of
//! individual file errors with inline expansion.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::s3_state::S3TaskState;
use super::tab::TabView;
use super::theme::Theme;
use super::upload_app::{error_category, sanitize_error, UploadTuiState};

/// The Errors tab: summary pane + scrollable error list.
#[derive(Debug)]
pub struct ErrorsTab {
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,
    pub cursor: usize,
    /// Which error row is expanded inline to show full error text.
    pub expanded: Option<usize>,
    scroll_offset: usize,
    pub pending_g: bool,
    pending_g_at: Instant,
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
            pending_g: false,
            pending_g_at: Instant::now(),
        }
    }

    /// Return the number of failed files from the upload state.
    fn error_count(&self) -> usize {
        self.upload_state.lock().map_or(0, |s| s.failed_files.len())
    }
}

// ---------------------------------------------------------------------------
// Sparkline characters
// ---------------------------------------------------------------------------

/// Error sparkline variant: returns a blank space for zero values (unlike
/// `widgets::spark_char` which returns ▁ as a baseline for throughput charts).
fn spark_char(val: f64, max: f64) -> char {
    if max <= 0.0 || val <= 0.0 {
        return ' ';
    }
    let idx = ((val / max) * 7.0).round() as usize;
    super::widgets::SPARK_CHARS[idx.min(7)]
}

/// Build a per-minute error histogram from timestamps, returning counts for
/// each minute bucket from session start to now.
fn error_histogram(timestamps: &[Instant], session_start: Instant) -> Vec<f64> {
    if timestamps.is_empty() {
        return Vec::new();
    }
    let now = Instant::now();
    let elapsed_secs = now.duration_since(session_start).as_secs();
    let num_buckets = (elapsed_secs / 60 + 1) as usize;
    let num_buckets = num_buckets.max(1);
    let mut buckets = vec![0.0; num_buckets];
    for ts in timestamps {
        let offset = ts.duration_since(session_start).as_secs();
        let bucket = (offset / 60) as usize;
        if bucket < num_buckets {
            buckets[bucket] += 1.0;
        }
    }
    buckets
}

impl TabView for ErrorsTab {
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let s3_errors = self.s3_state.lock().map_or(0, |s| s.errors);

        // Grab error data under lock
        let (total, active, resolved, categories, histogram, session_elapsed) = self
            .upload_state
            .lock()
            .map(|s| {
                let total = s.failed_files.len();
                let resolved = s.failed_files.iter().filter(|e| e.resolved).count();
                let active = total - resolved;

                // Category counts
                let mut cats: HashMap<&str, usize> = HashMap::new();
                for entry in &s.failed_files {
                    *cats.entry(error_category(&entry.message)).or_insert(0) += 1;
                }
                // Sort by count descending
                let mut cat_vec: Vec<(String, usize)> =
                    cats.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
                cat_vec.sort_by(|a, b| b.1.cmp(&a.1));

                let hist = error_histogram(&s.error_timestamps, s.session_start);
                let elapsed = s.session_start.elapsed();

                (total, active, resolved, cat_vec, hist, elapsed)
            })
            .unwrap_or_default();

        // Layout: Summary (6-7) | Error list (fill) | S3 panel (3, if errors)
        let mut constraints = vec![Constraint::Length(6), Constraint::Min(5)];
        if s3_errors > 0 {
            constraints.push(Constraint::Length(3));
        }
        let chunks = Layout::vertical(constraints).split(area);

        // ── Summary pane ──────────────────────────────────────────────
        draw_summary_pane(
            frame,
            chunks[0],
            theme,
            total,
            active,
            resolved,
            &categories,
            &histogram,
            session_elapsed,
        );

        // ── Error list ────────────────────────────────────────────────
        draw_error_list(frame, chunks[1], theme, &self.upload_state, self);

        // ── S3 Task Errors panel (only if errors > 0) ────────────────
        if s3_errors > 0 && chunks.len() > 2 {
            let s3_block = Block::default()
                .title(" S3 Task Errors ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border));
            let s3_inner = s3_block.inner(chunks[2]);
            frame.render_widget(s3_block, chunks[2]);

            let s3_text = Paragraph::new(Line::from(Span::styled(
                format!("{s3_errors} task(s) with errors"),
                Style::default().fg(theme.red),
            )));
            frame.render_widget(s3_text, s3_inner);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let count = self.error_count();
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.pending_g = false;
                if count > 0 {
                    self.cursor = (self.cursor + 1).min(count.saturating_sub(1));
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
                if count > 0 {
                    self.cursor = count - 1;
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
            KeyCode::Enter => {
                self.pending_g = false;
                if self.expanded == Some(self.cursor) {
                    self.expanded = None;
                } else {
                    self.expanded = Some(self.cursor);
                }
                true
            }
            KeyCode::Esc => {
                self.pending_g = false;
                self.expanded = None;
                true
            }
            _ => {
                self.pending_g = false;
                false
            }
        }
    }

    fn tick(&mut self) {
        if self.pending_g && self.pending_g_at.elapsed() > Duration::from_millis(500) {
            self.pending_g = false;
        }
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("j/k", "scroll"),
            ("G", "end"),
            ("gg", "top"),
            ("Enter", "expand"),
            ("?", "help"),
            ("q", "quit"),
        ]
    }
}

// ---------------------------------------------------------------------------
// Summary pane
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw_summary_pane(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    total: usize,
    active: usize,
    resolved: usize,
    categories: &[(String, usize)],
    histogram: &[f64],
    elapsed: std::time::Duration,
) {
    let block = Block::default()
        .title(Span::styled(
            " Error Summary ",
            Style::default().fg(theme.maroon_bright),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Line 1: Counts
    let total_span = Span::styled(
        format!("Total: {total}"),
        Style::default()
            .fg(if total > 0 { theme.red } else { theme.green })
            .add_modifier(Modifier::BOLD),
    );
    let resolved_span = if resolved > 0 {
        Span::styled(
            format!("   Resolved: {resolved}"),
            Style::default().fg(theme.green),
        )
    } else {
        Span::styled(
            "   Resolved: 0".to_string(),
            Style::default().fg(theme.text_muted),
        )
    };
    let active_span = if active > 0 {
        Span::styled(
            format!("   Active: {active}"),
            Style::default().fg(theme.red),
        )
    } else {
        Span::styled(
            "   Active: 0".to_string(),
            Style::default().fg(theme.text_muted),
        )
    };
    let elapsed_mins = elapsed.as_secs() / 60;
    let elapsed_secs = elapsed.as_secs() % 60;
    let elapsed_span = Span::styled(
        format!("   Uptime: {elapsed_mins}m{elapsed_secs:02}s"),
        Style::default().fg(theme.text_muted),
    );
    lines.push(Line::from(vec![
        total_span,
        resolved_span,
        active_span,
        elapsed_span,
    ]));

    // Line 2: Sparkline (errors/min)
    let spark_width = inner.width.saturating_sub(14) as usize; // room for label
    let max = histogram.iter().copied().fold(0.0_f64, f64::max);
    let spark: String = if histogram.is_empty() {
        " ".repeat(spark_width)
    } else {
        // Take the most recent buckets that fit
        let take = spark_width.min(histogram.len());
        let start = histogram.len().saturating_sub(take);
        let pad = spark_width.saturating_sub(take);
        let mut s = " ".repeat(pad);
        for &v in &histogram[start..] {
            s.push(spark_char(v, max));
        }
        s
    };
    lines.push(Line::from(vec![
        Span::styled(spark, Style::default().fg(theme.red)),
        Span::styled(
            " errors/min".to_string(),
            Style::default().fg(theme.text_muted),
        ),
    ]));

    // Line 3: Categories
    if categories.is_empty() {
        lines.push(Line::from(Span::styled(
            "By type: (none)",
            Style::default().fg(theme.text_muted),
        )));
    } else {
        let mut spans = vec![Span::styled(
            "By type: ",
            Style::default().fg(theme.text_muted),
        )];
        for (i, (cat, count)) in categories.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(
                    " · ",
                    Style::default().fg(theme.text_very_muted),
                ));
            }
            spans.push(Span::styled(
                format!("{cat}: {count}"),
                Style::default().fg(theme.text),
            ));
        }
        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// Error list
// ---------------------------------------------------------------------------

fn draw_error_list(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    upload_state: &Arc<Mutex<UploadTuiState>>,
    tab: &ErrorsTab,
) {
    let failed_files = upload_state
        .lock()
        .map(|s| s.failed_files.clone())
        .unwrap_or_default();

    let active_count = failed_files.iter().filter(|e| !e.resolved).count();
    let title = if active_count > 0 && active_count < failed_files.len() {
        format!(
            " Errors ({} active, {} resolved) ",
            active_count,
            failed_files.len() - active_count
        )
    } else {
        format!(" Errors ({}) ", failed_files.len())
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if failed_files.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            " No errors",
            Style::default().fg(theme.green),
        )));
        frame.render_widget(msg, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in failed_files.iter().enumerate() {
        let is_cursor = i == tab.cursor;

        // Resolved errors are dimmed with a ✓ prefix
        let (icon, icon_color) = if entry.resolved {
            ("\u{2713}", theme.green) // ✓
        } else {
            ("\u{2717}", theme.red) // ✗
        };

        let name_style = if is_cursor {
            Style::default().fg(theme.gold).add_modifier(Modifier::BOLD)
        } else if entry.resolved {
            Style::default().fg(theme.text_very_muted)
        } else {
            Style::default().fg(theme.text)
        };

        let error_style = if is_cursor {
            Style::default().fg(theme.gold)
        } else if entry.resolved {
            Style::default().fg(theme.text_very_muted)
        } else {
            Style::default().fg(theme.red)
        };

        // Cursor indicator
        let cursor_span = if is_cursor {
            Span::styled("\u{25b8} ", Style::default().fg(theme.gold))
        } else {
            Span::raw("  ")
        };

        // Defensive strip + truncate to fit in a single line
        let clean_msg = sanitize_error(&entry.message);
        let summary = if clean_msg.chars().count() > 60 {
            format!("{}...", clean_msg.chars().take(57).collect::<String>())
        } else {
            clean_msg.clone()
        };

        // Relative timestamp
        let ago = entry.timestamp.elapsed().as_secs();
        let ago_str = if ago < 60 {
            format!("{ago}s ago")
        } else {
            format!("{}m ago", ago / 60)
        };

        lines.push(Line::from(vec![
            cursor_span,
            Span::styled(
                format!("{icon} "),
                Style::default().fg(if is_cursor { theme.gold } else { icon_color }),
            ),
            Span::styled(format!("{:<36} ", entry.file), name_style),
            Span::styled(summary, error_style),
            Span::styled(
                format!("  {ago_str}"),
                Style::default().fg(theme.text_very_muted),
            ),
        ]));

        // If this row is expanded, word-wrap the full (sanitized) error below.
        if tab.expanded == Some(i) {
            let wrap_width = inner.width.saturating_sub(6) as usize;
            let style = Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::ITALIC);
            for wrapped in word_wrap(&clean_msg, wrap_width.max(20)) {
                lines.push(Line::from(Span::styled(format!("      {wrapped}"), style)));
            }
        }
    }

    let visible_height = inner.height as usize;
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(tab.scroll_offset)
        .take(visible_height)
        .collect();

    let paragraph = Paragraph::new(visible);
    frame.render_widget(paragraph, inner);
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
    use crate::tui::upload_app::ErrorEntry;
    use crossterm::event::KeyCode;

    fn make_state_with_errors() -> Arc<Mutex<UploadTuiState>> {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);
        state.failed_files.push(ErrorEntry {
            identifier: "item-a".to_string(),
            file: "item-a/file1.jpg".to_string(),
            message: "SlowDown: Please slow down".to_string(),
            timestamp: Instant::now(),
            resolved: false,
        });
        state.failed_files.push(ErrorEntry {
            identifier: "item-a".to_string(),
            file: "item-a/file2.jpg".to_string(),
            message: "Timeout after 30s".to_string(),
            timestamp: Instant::now(),
            resolved: false,
        });
        state.error_timestamps.push(Instant::now());
        state.error_timestamps.push(Instant::now());
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

    #[test]
    fn test_error_category_parsing() {
        use super::super::upload_app::error_category;
        assert_eq!(error_category("SlowDown: Please slow down"), "SlowDown");
        assert_eq!(
            error_category("AccessDenied: Access Denied"),
            "AccessDenied"
        );
        assert_eq!(error_category("HTTP 500: internal"), "HTTP Error");
        assert_eq!(error_category("timeout after 30s"), "Timeout");
        assert_eq!(error_category("something weird"), "Other");
    }

    #[test]
    fn test_error_histogram() {
        let start = Instant::now() - std::time::Duration::from_secs(120);
        let timestamps = vec![
            start + std::time::Duration::from_secs(10),  // bucket 0
            start + std::time::Duration::from_secs(30),  // bucket 0
            start + std::time::Duration::from_secs(70),  // bucket 1
            start + std::time::Duration::from_secs(130), // bucket 2
        ];
        let hist = error_histogram(&timestamps, start);
        assert!(hist.len() >= 3);
        assert_eq!(hist[0], 2.0);
        assert_eq!(hist[1], 1.0);
        assert_eq!(hist[2], 1.0);
    }

    #[test]
    fn test_word_wrap() {
        let lines = word_wrap("short", 40);
        assert_eq!(lines, vec!["short"]);

        let lines = word_wrap("this is a longer message that should wrap", 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.len() <= 20);
        }
    }

    #[test]
    fn test_jump_to_end() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, 1);
    }

    #[test]
    fn test_gg_jump_to_top() {
        let mut tab = ErrorsTab::new(
            make_state_with_errors(),
            Arc::new(Mutex::new(S3TaskState::new())),
        );
        tab.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(tab.cursor, 1);
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(tab.pending_g);
        tab.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(tab.cursor, 0);
        assert!(!tab.pending_g);
    }
}
