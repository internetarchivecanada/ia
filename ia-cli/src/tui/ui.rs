use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Sparkline};
use ratatui::Frame;

use super::app::{ItemStatus, TuiState};

pub fn draw(f: &mut Frame, state: &TuiState) {
    let mut constraints = vec![Constraint::Length(3)]; // Header always

    let show_items = state.items.len() > 1;
    if show_items {
        constraints.push(Constraint::Min(5)); // Items panel
    }

    constraints.push(Constraint::Min(5)); // Workers panel (always)

    let show_disks = !state.disk_statuses.is_empty();
    if show_disks {
        constraints.push(Constraint::Length(3));
    }

    let show_errors = !state.failed_files.is_empty();
    if show_errors {
        constraints.push(Constraint::Length(5));
    }

    let show_throughput = !state.throughput_history.is_empty();
    if show_throughput {
        constraints.push(Constraint::Length(4)); // Throughput sparkline
    }

    constraints.push(Constraint::Length(3)); // Status bar

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(f.area());

    let mut idx = 0;

    draw_header(f, chunks[idx], state);
    idx += 1;

    if show_items {
        draw_items_panel(f, chunks[idx], state);
        idx += 1;
    }

    draw_active_files(f, chunks[idx], state);
    idx += 1;

    if show_disks {
        draw_disks_panel(f, chunks[idx], state);
        idx += 1;
    }

    if show_errors {
        draw_errors_panel(f, chunks[idx], state);
        idx += 1;
    }

    if show_throughput {
        draw_throughput_panel(f, chunks[idx], state);
        idx += 1;
    }

    draw_status_bar(f, chunks[idx], state);
}

fn draw_header(f: &mut Frame, area: Rect, state: &TuiState) {
    let progress = state.overall_progress();
    let throughput = state.throughput();

    let eta_str = state
        .eta_seconds()
        .map(|s| {
            let secs = s as u64;
            if secs >= 3600 {
                format!("  ETA {}h {:02}m", secs / 3600, (secs % 3600) / 60)
            } else if secs >= 60 {
                format!("  ETA {}m {:02}s", secs / 60, secs % 60)
            } else {
                format!("  ETA {}s", secs)
            }
        })
        .unwrap_or_default();

    let label = format!(
        " {} — {:.0}% ({})  {}/s{} ",
        state.identifier,
        progress * 100.0,
        format_bytes(state.bytes_downloaded),
        format_bytes(throughput as u64),
        eta_str,
    );

    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(format!(" ia download — {} ", state.identifier))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .ratio(progress.min(1.0))
        .label(label);

    f.render_widget(gauge, area);
}

fn draw_items_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .title(" Items ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for item in &state.items {
        let status_icon = match &item.status {
            ItemStatus::Pending => Span::styled("  ", Style::default().fg(Color::DarkGray)),
            ItemStatus::Downloading => Span::styled(" \u{25b8}", Style::default().fg(Color::Cyan)),
            ItemStatus::Complete => {
                Span::styled(" \u{2713}", Style::default().fg(Color::Green))
            }
            ItemStatus::Failed(_) => Span::styled(" \u{2717}", Style::default().fg(Color::Red)),
        };

        let files_info = if item.files_total > 0 {
            format!(
                "{}/{}",
                item.files_completed + item.files_skipped + item.files_failed,
                item.files_total
            )
        } else {
            "\u{2014}".to_string()
        };

        let bytes_info = if item.bytes_downloaded > 0 {
            format_bytes(item.bytes_downloaded)
        } else {
            "\u{2014}".to_string()
        };

        let speed_info = if matches!(item.status, ItemStatus::Downloading) {
            format!("{}/s", format_bytes(item.throughput() as u64))
        } else {
            "\u{2014}".to_string()
        };

        let name = if item.identifier.len() > 25 {
            format!(
                "\u{2026}{}",
                &item.identifier[item.identifier.len() - 24..]
            )
        } else {
            format!("{:<25}", item.identifier)
        };

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
            Span::raw("  "),
            Span::styled(
                format!("{:>10}", speed_info),
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

fn draw_active_files(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .title(format!(" Workers ({}) ", state.active_files.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // Active files with progress
    let mut active: Vec<_> = state.active_files.values().collect();
    active.sort_by(|a, b| a.name.cmp(&b.name));

    for fp in active.iter().skip(state.scroll_offset) {
        let progress = fp
            .total_bytes
            .map(|total| {
                if total > 0 {
                    fp.bytes_downloaded as f64 / total as f64
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);

        let bar_width = 20;
        let filled = (progress * bar_width as f64) as usize;
        let empty = bar_width - filled;
        let bar = format!(
            "{}{}",
            "\u{2588}".repeat(filled),
            "\u{2591}".repeat(empty)
        );

        let speed = {
            let secs = fp.started_at.elapsed().as_secs_f64();
            if secs > 0.0 {
                format!(
                    "{}/s",
                    format_bytes((fp.bytes_downloaded as f64 / secs) as u64)
                )
            } else {
                "\u{2014}".to_string()
            }
        };

        let name = if fp.name.len() > 30 {
            format!("\u{2026}{}", &fp.name[fp.name.len() - 29..])
        } else {
            format!("{:<30}", fp.name)
        };

        lines.push(Line::from(vec![
            Span::styled(" \u{25b8} ", Style::default().fg(Color::Green)),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::raw(" "),
            Span::styled(bar, Style::default().fg(Color::Cyan)),
            Span::raw(format!(" {:>5.1}%  ", progress * 100.0)),
            Span::styled(speed, Style::default().fg(Color::DarkGray)),
        ]));

        if lines.len() >= inner.height as usize {
            break;
        }
    }

    // Recent completions
    if lines.len() < inner.height as usize && !state.completed_files.is_empty() {
        let remaining = inner.height as usize - lines.len();
        let recent = state
            .completed_files
            .iter()
            .rev()
            .take(remaining.min(3));

        for name in recent {
            let display_name = if name.len() > 30 {
                format!("\u{2026}{}", &name[name.len() - 29..])
            } else {
                format!("{:<30}", name)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    " \u{2713} ",
                    Style::default().fg(Color::Green),
                ),
                Span::styled(display_name, Style::default().fg(Color::DarkGray)),
                Span::styled(" done", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if lines.is_empty() {
        if state.done {
            lines.push(Line::from(Span::styled(
                " All downloads complete.",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                " Waiting for downloads to start...",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

fn draw_disks_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .title(" Disks ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (path, free) in &state.disk_statuses {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(path, Style::default().fg(Color::White)),
            Span::raw(": "),
            Span::styled(
                format!("{} free", format_bytes(*free)),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

fn draw_errors_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .title(format!(" Errors ({}) ", state.failed_files.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (name, err) in state.failed_files.iter().rev().take(inner.height as usize) {
        lines.push(Line::from(vec![
            Span::styled(
                " \u{2717} ",
                Style::default().fg(Color::Red),
            ),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::raw(" "),
            Span::styled(err, Style::default().fg(Color::DarkGray)),
        ]));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

fn draw_throughput_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .title(" Throughput (last 60s) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    // Convert f64 throughput to u64 for sparkline
    let data: Vec<u64> = state
        .throughput_history
        .iter()
        .map(|v| *v as u64)
        .collect();

    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .style(Style::default().fg(Color::Cyan));

    f.render_widget(sparkline, area);
}

fn draw_status_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let elapsed = state.elapsed();
    let elapsed_str = if elapsed.as_secs() >= 3600 {
        format!(
            "{}h {:02}m {:02}s",
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() % 3600) / 60,
            elapsed.as_secs() % 60
        )
    } else if elapsed.as_secs() >= 60 {
        format!(
            "{}m {:02}s",
            elapsed.as_secs() / 60,
            elapsed.as_secs() % 60
        )
    } else {
        format!("{}s", elapsed.as_secs())
    };

    let remaining = state.files_total
        - state.files_completed
        - state.files_skipped
        - state.files_failed;

    let status = Line::from(vec![
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
    ]);

    let keys = Line::from(vec![
        Span::raw(" "),
        Span::styled("[j/k]", Style::default().fg(Color::Cyan)),
        Span::raw(" scroll  "),
        Span::styled("[q]", Style::default().fg(Color::Cyan)),
        Span::raw("uit"),
    ]);

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
    f.render_widget(Paragraph::new(keys), layout[1]);
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
