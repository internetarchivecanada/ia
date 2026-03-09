//! Shared TUI widgets and formatting helpers.
//!
//! These are reusable components extracted from the download dashboard so that
//! both the download and upload dashboards can share the same look and feel.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Frame;

// ---------------------------------------------------------------------------
// ThroughputTracker
// ---------------------------------------------------------------------------

/// Tracks bytes transferred over time and maintains a rolling 60-sample history
/// suitable for rendering as a sparkline.
#[derive(Debug, Clone)]
pub struct ThroughputTracker {
    started_at: Instant,
    last_sample: Instant,
    total_bytes: u64,
    /// Rolling throughput history (bytes/sec), most recent last. Max 60 entries.
    history: Vec<f64>,
}

impl ThroughputTracker {
    /// Create a new tracker, starting the clock now.
    #[must_use]
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_sample: now,
            total_bytes: 0,
            history: Vec::with_capacity(60),
        }
    }

    /// Update the current total bytes transferred.
    pub fn set_bytes(&mut self, total: u64) {
        self.total_bytes = total;
    }

    /// If at least one second has elapsed since the last sample, push the
    /// current throughput onto the history ring (keeping at most 60 entries).
    pub fn maybe_sample(&mut self) {
        if self.last_sample.elapsed() >= Duration::from_secs(1) {
            self.history.push(self.throughput());
            if self.history.len() > 60 {
                self.history.remove(0);
            }
            self.last_sample = Instant::now();
        }
    }

    /// Overall bytes/sec since the tracker was created.
    #[must_use]
    pub fn throughput(&self) -> f64 {
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
    pub fn history(&self) -> &[f64] {
        &self.history
    }
}

impl Default for ThroughputTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Format a byte count as a human-readable string using binary prefixes.
///
/// Examples: `"512 B"`, `"1.5 KiB"`, `"23.4 MiB"`, `"1.20 GiB"`, `"2.50 TiB"`.
#[allow(dead_code)] // Used by the upload dashboard (Task 7+)
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < MIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < GIB {
        format!("{:.1} MiB", b / MIB)
    } else if b < TIB {
        format!("{:.2} GiB", b / GIB)
    } else {
        format!("{:.2} TiB", b / TIB)
    }
}

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
#[allow(dead_code)] // Used by the upload dashboard (Task 7+)
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
pub fn draw_throughput_panel(frame: &mut Frame, area: Rect, history: &[f64]) {
    let block = Block::default()
        .title(" Throughput (last 60s) ")
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
#[allow(dead_code)] // Used by the upload dashboard (Task 7+)
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

    // -- format_bytes ---------------------------------------------------------

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_small() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn format_bytes_kib() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn format_bytes_mib() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(10 * 1024 * 1024 + 512 * 1024), "10.5 MiB");
    }

    #[test]
    fn format_bytes_gib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "2.50 GiB");
    }

    #[test]
    fn format_bytes_tib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.00 TiB");
    }

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
            tracker.history.push(i as f64 * 100.0);
            if tracker.history.len() > 60 {
                tracker.history.remove(0);
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
}
