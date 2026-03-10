//! Upload dashboard panel rendering.
//!
//! Draws all panels for the upload TUI dashboard. Layout mirrors the download
//! dashboard (`tui/ui.rs`) with upload-specific panels: S3 tasks, rate limits,
//! and verification status.

use ia_core::upload::UploadProgressStatus;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use super::upload_app::{UploadItemStatus, UploadTuiState};
use super::widgets;

/// Draw the complete upload dashboard UI.
pub fn draw(f: &mut Frame, state: &UploadTuiState) {
    let mut constraints = vec![Constraint::Length(3)]; // Header (always)

    constraints.push(Constraint::Min(5)); // Workers panel (always)

    let show_items = state.items.len() > 1;
    if show_items {
        constraints.push(Constraint::Min(5)); // Items panel
    }

    let show_s3_tasks = state.tasks_queued > 0 || state.tasks_running > 0 || state.tasks_error > 0;
    if show_s3_tasks {
        constraints.push(Constraint::Length(3)); // S3 Tasks
    }

    let has_rate_limited = state
        .items
        .iter()
        .any(|i| i.status == UploadItemStatus::RateLimited);
    if has_rate_limited {
        constraints.push(Constraint::Length(3)); // Rate Limit
    }

    let show_errors = !state.failed_files.is_empty();
    if show_errors {
        constraints.push(Constraint::Length(5)); // Errors
    }

    let show_throughput = !state.throughput.history().is_empty();
    if show_throughput {
        constraints.push(Constraint::Length(4)); // Throughput sparkline
    }

    constraints.push(Constraint::Length(3)); // Status bar (always)

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(f.area());

    let mut idx = 0;

    draw_header(f, chunks[idx], state);
    idx += 1;

    draw_active_files(f, chunks[idx], state);
    idx += 1;

    if show_items {
        draw_items_panel(f, chunks[idx], state);
        idx += 1;
    }

    if show_s3_tasks {
        draw_s3_tasks_panel(f, chunks[idx], state);
        idx += 1;
    }

    if has_rate_limited {
        draw_rate_limit_panel(f, chunks[idx], state);
        idx += 1;
    }

    if show_errors {
        widgets::draw_errors_panel(f, chunks[idx], &state.failed_files);
        idx += 1;
    }

    if show_throughput {
        widgets::draw_throughput_panel(f, chunks[idx], state.throughput.history());
        idx += 1;
    }

    draw_status_bar(f, chunks[idx], state);
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn draw_header(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let progress = state.overall_progress();
    let throughput = state.throughput.throughput();
    let remaining = state.bytes_total.saturating_sub(state.bytes_uploaded);
    let eta_str = widgets::format_eta(remaining, throughput);
    let eta_display = if eta_str.is_empty() {
        String::new()
    } else {
        format!("  {eta_str}")
    };

    // Title: single-item shows identifier, batch shows count.
    let title_ident = if state.items.len() == 1 {
        state.items[0].identifier.clone()
    } else {
        format!("{} items", state.items.len())
    };

    let label = format!(
        " {} \u{2014} {:.0}% ({})  {}/s{} ",
        title_ident,
        progress * 100.0,
        widgets::format_bytes(state.bytes_uploaded),
        widgets::format_bytes(throughput as u64),
        eta_display,
    );

    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(" ia upload ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .ratio(progress.min(1.0))
        .label(label);

    f.render_widget(gauge, area);
}

// ---------------------------------------------------------------------------
// Items panel
// ---------------------------------------------------------------------------

fn draw_items_panel(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .title(" Items ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for item in &state.items {
        let status_icon = match &item.status {
            UploadItemStatus::Pending => {
                Span::styled("  ", Style::default().fg(Color::DarkGray))
            }
            UploadItemStatus::Verifying => {
                Span::styled(" \u{25c7}", Style::default().fg(Color::Yellow))
            }
            UploadItemStatus::Uploading => {
                Span::styled(" \u{25b8}", Style::default().fg(Color::Cyan))
            }
            UploadItemStatus::RateLimited => {
                Span::styled(" \u{23f8}", Style::default().fg(Color::Yellow))
            }
            UploadItemStatus::Complete => {
                Span::styled(" \u{2713}", Style::default().fg(Color::Green))
            }
            UploadItemStatus::Failed(_) => {
                Span::styled(" \u{2717}", Style::default().fg(Color::Red))
            }
        };

        let files_info = if item.files_total > 0 {
            format!(
                "{}/{}",
                item.files_completed + item.files_skipped + item.files_failed,
                item.files_total,
            )
        } else {
            "\u{2014}".to_string()
        };

        let bytes_info = if item.bytes_uploaded > 0 {
            widgets::format_bytes(item.bytes_uploaded)
        } else {
            "\u{2014}".to_string()
        };

        let name = widgets::truncate_tail(&item.identifier, 25);

        lines.push(Line::from(vec![
            status_icon,
            Span::raw(" "),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(
                format!("{:>8}", files_info),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>10}", bytes_info),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        if lines.len() >= inner.height as usize {
            break;
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Active files (Workers) panel
// ---------------------------------------------------------------------------

fn draw_active_files(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .title(format!(" Workers ({}) ", state.active_files.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // Active files sorted by name.
    let mut active: Vec<_> = state.active_files.values().collect();
    active.sort_by(|a, b| a.name.cmp(&b.name));

    for fp in active.iter().skip(state.scroll_offset) {
        let (icon, icon_color) = match fp.status {
            UploadProgressStatus::Verifying => ("\u{25c7}", Color::Yellow),
            UploadProgressStatus::Uploading => ("\u{2191}", Color::Cyan),
            UploadProgressStatus::WaitingRateLimit => ("\u{23f8}", Color::Yellow),
            UploadProgressStatus::Complete => ("\u{2713}", Color::Green),
            UploadProgressStatus::Skipped => ("\u{2013}", Color::DarkGray),
            UploadProgressStatus::Failed => ("\u{2717}", Color::Red),
        };

        let progress = if fp.total_bytes > 0 {
            fp.bytes_sent as f64 / fp.total_bytes as f64
        } else {
            0.0
        };

        let bar_width = 20;
        let filled = (progress * bar_width as f64) as usize;
        let empty = bar_width - filled;
        let bar = format!(
            "{}{}",
            "\u{2588}".repeat(filled),
            "\u{2591}".repeat(empty),
        );

        let name = widgets::truncate_tail(&fp.name, 30);

        lines.push(Line::from(vec![
            Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
            Span::styled(bar, Style::default().fg(Color::Cyan)),
            Span::raw(format!(" {:>5.1}% ", progress * 100.0)),
            Span::styled(name, Style::default().fg(Color::White)),
        ]));

        if lines.len() >= inner.height as usize {
            break;
        }
    }

    // Show recent completions if space permits.
    if lines.len() < inner.height as usize && !state.completed_files.is_empty() {
        let remaining = inner.height as usize - lines.len();
        let recent = state
            .completed_files
            .iter()
            .rev()
            .take(remaining.min(3));

        for name in recent {
            let display_name = widgets::truncate_tail(name, 30);
            lines.push(Line::from(vec![
                Span::styled(" \u{2713} ", Style::default().fg(Color::Green)),
                Span::styled(display_name, Style::default().fg(Color::DarkGray)),
                Span::styled(" done", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if lines.is_empty() {
        if state.done {
            lines.push(Line::from(Span::styled(
                " All uploads complete.",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                " Waiting for uploads to start...",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// S3 Tasks panel
// ---------------------------------------------------------------------------

fn draw_s3_tasks_panel(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .title(" S3 Tasks ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled(" Queued: ", Style::default().fg(Color::White)),
        Span::styled(
            state.tasks_queued.to_string(),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled("Running: ", Style::default().fg(Color::White)),
        Span::styled(
            state.tasks_running.to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  "),
        Span::styled("Errors: ", Style::default().fg(Color::White)),
        Span::styled(
            state.tasks_error.to_string(),
            if state.tasks_error > 0 {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]);

    f.render_widget(Paragraph::new(line), inner);
}

// ---------------------------------------------------------------------------
// Rate Limit panel
// ---------------------------------------------------------------------------

fn draw_rate_limit_panel(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let block = Block::default()
        .title(" Rate Limited ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = state
        .items
        .iter()
        .filter(|i| i.status == UploadItemStatus::RateLimited)
        .take(inner.height as usize)
        .map(|item| {
            Line::from(vec![
                Span::styled(" \u{23f8} ", Style::default().fg(Color::Yellow)),
                Span::styled(&*item.identifier, Style::default().fg(Color::White)),
                Span::styled(
                    "  polling check_limit...",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(f: &mut Frame, area: Rect, state: &UploadTuiState) {
    let elapsed_str = widgets::format_elapsed(state.throughput.elapsed());

    let is_batch = state.items.len() > 1;

    let status = if is_batch {
        let items_done = state
            .items
            .iter()
            .filter(|i| matches!(i.status, UploadItemStatus::Complete))
            .count();
        let items_failed = state
            .items
            .iter()
            .filter(|i| matches!(i.status, UploadItemStatus::Failed(_)))
            .count();
        let items_total = state.items.len();

        let mut parts = vec![
            Span::styled(" Items: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{items_done}/{items_total} done"),
                Style::default().fg(if items_done == items_total {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ];

        if items_failed > 0 {
            parts.push(Span::styled(
                format!(", {items_failed} failed"),
                Style::default().fg(Color::Red),
            ));
        }

        parts.push(Span::raw("  "));
        parts.push(Span::styled(
            format!(
                "Files: {} done, {} skipped, {} failed",
                state.files_completed, state.files_skipped, state.files_failed,
            ),
            Style::default().fg(Color::DarkGray),
        ));
        parts.push(Span::raw("  "));
        parts.push(Span::styled(
            elapsed_str,
            Style::default().fg(Color::DarkGray),
        ));

        Line::from(parts)
    } else {
        let remaining = state
            .files_total
            .saturating_sub(state.files_completed)
            .saturating_sub(state.files_skipped)
            .saturating_sub(state.files_failed);

        Line::from(vec![
            Span::styled(" Queue: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{remaining} remaining"),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "Done: {}  Skipped: {}  Failed: {}",
                    state.files_completed, state.files_skipped, state.files_failed,
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(elapsed_str, Style::default().fg(Color::DarkGray)),
        ])
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    f.render_widget(Paragraph::new(status), layout[0]);
    widgets::draw_key_hints(f, layout[1], &[("j/k", " scroll  "), ("q", "uit")]);
}
