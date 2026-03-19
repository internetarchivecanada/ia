//! Help overlay for the multi-tab dashboard.
//!
//! Renders a centered popup showing context-sensitive keybindings for the
//! currently active tab, plus global keys that apply everywhere.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::tab::TabId;
use super::theme::Theme;

/// Returns formatted help text for a given tab.
///
/// Always starts with global keys, then appends tab-specific keys.
pub fn help_content(tab: TabId) -> String {
    let mut text = String::from(
        "Global:\n\
         \x20 1-4     Switch tabs\n\
         \x20 Tab     Cycle panel focus\n\
         \x20 ?       Toggle this help\n\
         \x20 q       Quit\n",
    );

    let tab_section = match tab {
        TabId::Upload => {
            "\n\
             Upload:\n\
             \x20 j/k     Scroll focused panel\n\
             \x20 Enter   Open item history\n"
        }
        TabId::Tasks => {
            "\n\
             Tasks:\n\
             \x20 j/k     Scroll task list\n\
             \x20 /       Search by identifier\n\
             \x20 u       Toggle user/global tasks\n\
             \x20 Enter   Open item history\n"
        }
        TabId::Log => {
            "\n\
             Log:\n\
             \x20 j/k     Scroll entries\n\
             \x20 G       Jump to end\n\
             \x20 gg      Jump to top\n\
             \x20 /       Search\n\
             \x20 f       Filter by status\n"
        }
        TabId::Errors => {
            "\n\
             Errors:\n\
             \x20 j/k     Scroll errors\n\
             \x20 Enter   Expand/collapse detail\n"
        }
    };

    text.push_str(tab_section);
    text
}

/// Renders a centered help overlay on top of the dashboard.
///
/// The overlay is 60% of the available width and height, centered in `area`.
/// It clears the underlying content first, then draws a bordered popup with
/// syntax-highlighted keybindings.
pub fn draw_help_overlay(frame: &mut Frame, area: Rect, theme: &Theme, tab: TabId) {
    let popup_area = centered_rect(60, 60, area);

    // Clear the area behind the popup.
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(Span::styled(
            " Help (press any key) ",
            Style::default().fg(theme.maroon_bright),
        ));

    let content = help_content(tab);
    let lines: Vec<Line> = content
        .lines()
        .map(|line| {
            // Section headers end with ':' and have no leading whitespace.
            if !line.starts_with(' ') && line.ends_with(':') {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default()
                        .fg(theme.maroon_bright)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if line.starts_with(' ') {
                // Key-binding lines: "  key     description"
                // Split at the first run of 2+ spaces after the key name.
                let trimmed = line.trim_start();
                let leading_spaces = line.len() - trimmed.len();
                let prefix = &line[..leading_spaces];

                if let Some(split_pos) = find_description_start(trimmed) {
                    let key_part = &trimmed[..split_pos];
                    let desc_part = trimmed[split_pos..].trim_start();
                    Line::from(vec![
                        Span::raw(prefix.to_string()),
                        Span::styled(key_part.to_string(), Style::default().fg(theme.gold)),
                        Span::raw("  "),
                        Span::styled(
                            desc_part.to_string(),
                            Style::default().fg(theme.text_secondary),
                        ),
                    ])
                } else {
                    Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(theme.text_secondary),
                    ))
                }
            } else if line.is_empty() {
                Line::from("")
            } else {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.text),
                ))
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup_area);
}

/// Find the byte offset where the description starts in a trimmed key-binding line.
///
/// Looks for a run of two or more spaces after the key name, which separates the
/// key label from its description.
fn find_description_start(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    // Skip past the key name (non-space characters at the start).
    while i < bytes.len() && bytes[i] != b' ' {
        i += 1;
    }
    // Look for the first run of 2+ spaces.
    while i < bytes.len() {
        if bytes[i] == b' ' {
            let start = i;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i - start >= 2 {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Returns a centered rectangle within `area`, occupying `percent_x`% width
/// and `percent_y`% height.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    let horizontal_layout = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical_layout[1]);

    horizontal_layout[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_hints_always_present() {
        let hints = help_content(TabId::Upload);
        assert!(hints.contains("1-4"));
        assert!(hints.contains("Tab"));
        assert!(hints.contains("q"));
    }

    #[test]
    fn test_upload_tab_hints() {
        let hints = help_content(TabId::Upload);
        assert!(hints.contains("j/k"));
        assert!(hints.contains("Enter"));
    }

    #[test]
    fn test_tasks_tab_hints() {
        let hints = help_content(TabId::Tasks);
        assert!(hints.contains("u"));
        assert!(hints.contains("user/global"));
    }

    #[test]
    fn test_log_tab_hints() {
        let hints = help_content(TabId::Log);
        assert!(hints.contains("gg"));
        assert!(hints.contains("f"));
    }

    #[test]
    fn test_errors_tab_hints() {
        let hints = help_content(TabId::Errors);
        assert!(hints.contains("Enter"));
        assert!(hints.contains("Expand"));
    }
}
