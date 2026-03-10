//! Shared TUI widgets and formatting helpers.
//!
//! These are reusable components extracted from the download dashboard so that
//! both the download and upload dashboards can share the same look and feel.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Frame;

// ---------------------------------------------------------------------------
// ThroughputTracker
// ---------------------------------------------------------------------------

/// Rolling-window throughput tracker.
///
/// Maintains a 5-second sliding window of `(timestamp, cumulative_bytes)` samples
/// to compute *current* throughput, plus a 60-entry sparkline history of those
/// instantaneous readings. The speed display reflects what's happening *now*,
/// not a lifetime average.
#[derive(Debug, Clone)]
pub struct ThroughputTracker {
    started_at: Instant,
    last_sample: Instant,
    total_bytes: u64,
    /// Sliding window of `(timestamp, cumulative_bytes)` samples, kept for the
    /// last [`WINDOW`] seconds. Used to compute rolling throughput.
    samples: VecDeque<(Instant, u64)>,
    /// Sparkline history (bytes/sec), most recent last. Max 60 entries.
    history: VecDeque<f64>,
}

/// Rolling window size for throughput calculation.
const WINDOW: Duration = Duration::from_secs(5);

impl ThroughputTracker {
    /// Create a new tracker, starting the clock now.
    #[must_use]
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_sample: now,
            total_bytes: 0,
            samples: VecDeque::with_capacity(8),
            history: VecDeque::with_capacity(60),
        }
    }

    /// Update the current total bytes transferred.
    pub fn set_bytes(&mut self, total: u64) {
        self.total_bytes = total;
    }

    /// If at least one second has elapsed since the last sample, record the
    /// current cumulative bytes and push the rolling throughput onto the
    /// sparkline history (keeping at most 60 entries).
    pub fn maybe_sample(&mut self) {
        if self.last_sample.elapsed() >= Duration::from_secs(1) {
            let now = Instant::now();
            self.samples.push_back((now, self.total_bytes));

            // Evict samples older than the rolling window.
            while self
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
            {
                self.samples.pop_front();
            }

            self.history.push_back(self.throughput());
            if self.history.len() > 60 {
                self.history.pop_front();
            }
            self.last_sample = now;
        }
    }

    /// Current bytes/sec computed over the rolling window.
    ///
    /// Falls back to a lifetime average when fewer than two samples exist.
    #[must_use]
    pub fn throughput(&self) -> f64 {
        if self.samples.len() >= 2 {
            let (t_old, b_old) = self.samples.front().unwrap();
            let (t_new, b_new) = self.samples.back().unwrap();
            let dt = t_new.duration_since(*t_old).as_secs_f64();
            if dt > 0.0 {
                return b_new.saturating_sub(*b_old) as f64 / dt;
            }
        }
        // Fallback: lifetime average until the window fills.
        let secs = self.started_at.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.total_bytes as f64 / secs
        } else {
            0.0
        }
    }

    /// Wall-clock time since the tracker was created.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Read-only access to the throughput history for sparkline rendering.
    #[must_use]
    pub fn history(&self) -> &VecDeque<f64> {
        &self.history
    }
}

impl Default for ThroughputTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// String truncation helpers
// ---------------------------------------------------------------------------

/// Truncate a string to fit within `max_chars` (by character count, not bytes).
///
/// If the string exceeds the limit, the *start* is elided and replaced with
/// an ellipsis (`…`), keeping the last `max_chars - 1` characters visible.
/// This is useful for identifiers and file paths where the tail is most
/// informative.
#[must_use]
pub fn truncate_tail(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        let keep = max_chars.saturating_sub(1);
        let start = s
            .char_indices()
            .rev()
            .nth(keep.saturating_sub(1))
            .map_or(0, |(i, _)| i);
        format!("\u{2026}{}", &s[start..])
    } else {
        format!("{s:<max_chars$}")
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

// Re-export from output.rs — single source of truth for byte formatting.
pub use crate::output::format_bytes;

/// Format a duration as a compact elapsed-time string.
///
/// - `>= 1h` → `"XhYYmZZs"`
/// - `>= 1m` → `"YmZZs"`
/// - otherwise → `"Zs"`
#[must_use]
pub fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Compute an ETA string from remaining bytes and current throughput.
///
/// Returns an empty string when `bytes_per_sec` is zero or negative.
#[must_use]
pub fn format_eta(remaining_bytes: u64, bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 || remaining_bytes == 0 {
        return String::new();
    }
    let secs = (remaining_bytes as f64 / bytes_per_sec) as u64;
    if secs >= 3600 {
        format!("ETA {}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("ETA {}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("ETA {secs}s")
    }
}

// ---------------------------------------------------------------------------
// Common panel widgets
// ---------------------------------------------------------------------------

/// Draw a sparkline throughput panel with a dark-gray border and cyan data.
///
/// `history` is a slice of bytes/sec values (most recent last), typically from
/// [`ThroughputTracker::history`].
pub fn draw_throughput_panel(frame: &mut Frame, area: Rect, history: &VecDeque<f64>) {
    let block = Block::default()
        .title(" Speed (last 60s) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let data: Vec<u64> = history.iter().map(|v| *v as u64).collect();

    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(sparkline, area);
}

/// Draw an error panel with a red border. Errors are shown most-recent first,
/// each prefixed with a red cross icon.
///
/// Each entry is `(name, error_message)`.
pub fn draw_errors_panel(frame: &mut Frame, area: Rect, errors: &[(String, String)]) {
    let block = Block::default()
        .title(format!(" Errors ({}) ", errors.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = errors
        .iter()
        .rev()
        .take(inner.height as usize)
        .map(|(name, err)| {
            Line::from(vec![
                Span::styled(" \u{2717} ", Style::default().fg(Color::Red)),
                Span::styled(name, Style::default().fg(Color::White)),
                Span::raw(" "),
                Span::styled(err, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Draw a key-hints bar at the bottom of a panel.
///
/// Each hint is `(key, label)` — for example `("[q]", "uit")`. Keys are
/// rendered in cyan brackets, labels in the default (gray) style.
pub fn draw_key_hints(frame: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(*key, Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(*label));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- format_elapsed -------------------------------------------------------

    #[test]
    fn format_elapsed_seconds_only() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59m59s");
    }

    #[test]
    fn format_elapsed_hours() {
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1h00m00s");
        assert_eq!(format_elapsed(Duration::from_secs(7384)), "2h03m04s");
    }

    // -- format_eta -----------------------------------------------------------

    #[test]
    fn format_eta_zero_speed() {
        assert_eq!(format_eta(1000, 0.0), "");
    }

    #[test]
    fn format_eta_zero_remaining() {
        assert_eq!(format_eta(0, 100.0), "");
    }

    #[test]
    fn format_eta_negative_speed() {
        assert_eq!(format_eta(1000, -5.0), "");
    }

    #[test]
    fn format_eta_seconds() {
        // 500 bytes at 100 B/s = 5s
        assert_eq!(format_eta(500, 100.0), "ETA 5s");
    }

    #[test]
    fn format_eta_minutes() {
        // 6000 bytes at 100 B/s = 60s = 1m 00s
        assert_eq!(format_eta(6000, 100.0), "ETA 1m 00s");
    }

    #[test]
    fn format_eta_hours() {
        // 360000 bytes at 100 B/s = 3600s = 1h 00m
        assert_eq!(format_eta(360_000, 100.0), "ETA 1h 00m");
    }

    // -- ThroughputTracker ----------------------------------------------------

    #[test]
    fn tracker_starts_at_zero() {
        let tracker = ThroughputTracker::new();
        assert_eq!(tracker.history().len(), 0);
        // throughput is 0 because no time has passed (or very little)
        // and no bytes have been set
        assert!(tracker.throughput() < 1.0);
    }

    #[test]
    fn tracker_set_bytes() {
        let mut tracker = ThroughputTracker::new();
        tracker.set_bytes(1024);
        // Throughput should be > 0 now (elapsed is tiny but nonzero)
        // Just verify it doesn't panic and bytes are recorded
        let _ = tracker.throughput();
    }

    #[test]
    fn tracker_default_matches_new() {
        let a = ThroughputTracker::new();
        let b = ThroughputTracker::default();
        assert_eq!(a.history().len(), b.history().len());
    }

    #[test]
    fn tracker_history_max_60() {
        let mut tracker = ThroughputTracker::new();
        // Manually push 65 samples to verify the cap
        for i in 0..65 {
            tracker.history.push_back(i as f64 * 100.0);
            if tracker.history.len() > 60 {
                tracker.history.pop_front();
            }
        }
        assert_eq!(tracker.history().len(), 60);
        // The oldest entry should be sample 5 (indices 0-4 were removed)
        assert!((tracker.history()[0] - 500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tracker_elapsed_increases() {
        let tracker = ThroughputTracker::new();
        std::thread::sleep(Duration::from_millis(10));
        assert!(tracker.elapsed() >= Duration::from_millis(5));
    }

    // -- truncate_tail --------------------------------------------------------

    #[test]
    fn truncate_tail_short_string() {
        assert_eq!(truncate_tail("hello", 10), "hello     ");
    }

    #[test]
    fn truncate_tail_exact_length() {
        assert_eq!(truncate_tail("hello", 5), "hello");
    }

    #[test]
    fn truncate_tail_long_string() {
        let result = truncate_tail("abcdefghijklmnop", 10);
        assert!(result.starts_with('\u{2026}'));
        assert_eq!(result.chars().count(), 10);
        // 9 chars from end + ellipsis = 10
        assert!(result.ends_with("hijklmnop"));
    }

    #[test]
    fn truncate_tail_multibyte_utf8() {
        // Japanese characters (3 bytes each in UTF-8)
        let s = "\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}"; // あいうえお
        let result = truncate_tail(s, 4);
        assert!(result.starts_with('\u{2026}'));
        assert_eq!(result.chars().count(), 4);
        // Should not panic — this is the key property
    }
}
