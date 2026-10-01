//! Stall detection for download streams (#11).
//!
//! A datanode that keeps sending bytes, slowly, never trips the transport's
//! read timeout (it resets on every chunk) and never errors the body
//! stream, so nothing in the chunk loop would ever give up on it. The
//! [`StallDetector`] answers one question for the loop: averaged over the
//! last [`WINDOW`], is this stream below its floor? It is pure, taking the
//! clock as an argument, so the rule is unit-tested without a runtime and
//! the drip-feed integration tests run on tokio's paused clock.

use std::time::Duration;

use tokio::time::Instant;

/// The sliding window the throughput is averaged over.
pub(crate) const WINDOW: Duration = Duration::from_secs(60);

/// How old a stream must be before it is judged at all. A connection that
/// is still ramping up is not a stall.
pub(crate) const GRACE: Duration = Duration::from_secs(30);

const BUCKETS: usize = WINDOW.as_secs() as usize;

/// Sliding-window throughput check for one download stream.
///
/// Bytes are credited to per-second buckets covering the last [`WINDOW`].
/// [`check`](Self::check) is `Some` when the stream is older than [`GRACE`]
/// and the bytes in the window, divided by the window's length (or the
/// stream's age while it is younger than the window), fall below the floor.
///
/// Every method takes `now` so the caller owns the clock.
#[derive(Debug, Clone)]
pub(crate) struct StallDetector {
    min_bytes_per_sec: u64,
    started: Instant,
    /// Bytes received in each of the last [`BUCKETS`] whole seconds, indexed
    /// by `second % BUCKETS`.
    buckets: [u64; BUCKETS],
    /// Seconds since `started` of the bucket last written.
    current_second: u64,
}

impl StallDetector {
    /// A detector for a stream that started at `now` with the given floor
    /// in bytes per second. The floor must be above zero; a zero floor
    /// means detection is off and the caller should not build one.
    pub(crate) fn new(min_bytes_per_sec: u64, now: Instant) -> Self {
        debug_assert!(min_bytes_per_sec > 0, "a zero floor disables detection");
        Self {
            min_bytes_per_sec,
            started: now,
            buckets: [0; BUCKETS],
            current_second: 0,
        }
    }

    /// Credit `bytes` received at `now` to the window.
    pub(crate) fn record(&mut self, now: Instant, bytes: u64) {
        let second = self.roll_to(now);
        self.buckets[second as usize % BUCKETS] += bytes;
    }

    /// Judge the stream at `now`.
    ///
    /// `None` while the stream is younger than [`GRACE`] or is keeping up.
    /// `Some(observed)` when it has fallen below the floor, where `observed`
    /// is the average in bytes per second that failed the check.
    pub(crate) fn check(&mut self, now: Instant) -> Option<u64> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < GRACE {
            return None;
        }
        self.roll_to(now);
        let span_secs = elapsed.as_secs().min(WINDOW.as_secs()).max(1);
        let observed = self.buckets.iter().sum::<u64>() / span_secs;
        (observed < self.min_bytes_per_sec).then_some(observed)
    }

    /// Move the window forward to the second containing `now`, zeroing the
    /// buckets for any seconds skipped since the last call, and return that
    /// second.
    fn roll_to(&mut self, now: Instant) -> u64 {
        let second = now.saturating_duration_since(self.started).as_secs();
        if second > self.current_second {
            let gap = second - self.current_second;
            if gap >= BUCKETS as u64 {
                self.buckets = [0; BUCKETS];
            } else {
                for s in (self.current_second + 1)..=second {
                    self.buckets[s as usize % BUCKETS] = 0;
                }
            }
            self.current_second = second;
        }
        second
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1024;
    const FLOOR: u64 = 10 * KIB;

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    /// `rate` bytes once per second, at each whole second in `secs`.
    fn feed(d: &mut StallDetector, start: Instant, secs: std::ops::Range<u64>, rate: u64) {
        for s in secs {
            d.record(at(start, s), rate);
        }
    }

    #[test]
    fn no_check_during_grace() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        for s in 0..30 {
            assert_eq!(d.check(at(start, s)), None, "second {s}");
        }
    }

    #[test]
    fn silent_stream_stalls_at_end_of_grace() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        assert_eq!(d.check(at(start, 30)), Some(0));
    }

    #[test]
    fn steady_stream_above_floor_never_stalls() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        for s in 0..120 {
            d.record(at(start, s), 20 * KIB);
            assert_eq!(d.check(at(start, s)), None, "second {s}");
        }
    }

    #[test]
    fn average_uses_stream_age_before_window_fills() {
        // 15 KiB/s for 30 s is 450 KiB. Over the stream's 30 s age that is
        // 15 KiB/s and healthy; over a 60 s window it would read as
        // 7.5 KiB/s and stall a stream that is keeping up.
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        feed(&mut d, start, 0..30, 15 * KIB);
        assert_eq!(d.check(at(start, 30)), None);
    }

    #[test]
    fn burst_then_silence_stalls_once_the_burst_leaves_the_window() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        d.record(at(start, 1), 2 * 1024 * KIB);
        // At 60 s the burst is still in the window: 2 MiB / 60 s ≈ 34 KiB/s.
        assert_eq!(d.check(at(start, 60)), None);
        // At 62 s the bucket for second 1 has been rolled out.
        assert_eq!(d.check(at(start, 62)), Some(0));
    }

    #[test]
    fn drip_below_floor_stalls() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        for s in [0, 10, 20] {
            d.record(at(start, s), 1);
        }
        // 3 bytes over 30 s rounds down to 0 B/s.
        assert_eq!(d.check(at(start, 30)), Some(0));
    }

    #[test]
    fn fast_then_slow_stalls_when_the_window_average_drops() {
        // 100 KiB/s for seconds 0..60, then 1 KiB/s. At second t >= 60 the
        // window covers seconds t-59..=t: (119 - t) fast seconds and
        // (t - 59) slow ones, so the average in KiB/s is
        // (100 * (119 - t) + (t - 59)) / 60. At t = 113 that is 654 / 60 =
        // 10.9, still above the floor; at t = 114 it is 555 / 60 = 9.25.
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        feed(&mut d, start, 0..60, 100 * KIB);
        let mut first_stall = None;
        for s in 60..130 {
            d.record(at(start, s), KIB);
            if d.check(at(start, s)).is_some() {
                first_stall = Some(s);
                break;
            }
        }
        assert_eq!(first_stall, Some(114));
    }

    #[test]
    fn gap_longer_than_window_clears_every_bucket() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        d.record(at(start, 1), 1024 * KIB);
        d.record(at(start, 200), 1);
        assert_eq!(d.check(at(start, 200)), Some(0));
    }

    #[test]
    fn observed_rate_is_reported() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        feed(&mut d, start, 0..40, 5 * KIB);
        assert_eq!(d.check(at(start, 40)), Some(5 * KIB));
    }

    #[test]
    fn recording_in_the_same_second_accumulates() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, start);
        for _ in 0..4 {
            d.record(at(start, 30), 100 * KIB);
        }
        // 400 KiB over the stream's 30 s age is above the floor.
        assert_eq!(d.check(at(start, 30)), None);
    }
}
