//! Stall detection for download streams (#11).
//!
//! A datanode that keeps sending bytes, slowly, never trips the transport's
//! read timeout (it resets on every chunk) and never errors the body
//! stream, so nothing in the chunk loop would ever give up on it. The
//! [`StallDetector`] answers one question for the loop: averaged over the
//! last [`WINDOW`], is this stream below its floor? It is pure, taking the
//! clock as an argument, so the rule is unit-tested without a runtime.
//!
//! The window and grace are fixed for users ([`policy`]). The drip-feed
//! tests in the parent module shrink them through a `cfg(test)`-only
//! override so a real-time test takes a second or two instead of a minute;
//! tokio's paused clock is not an option there because its auto-advance
//! runs the connect timeout out before a real TCP connect can complete.

use std::time::Duration;

use tokio::time::Instant;

/// The sliding window the throughput is averaged over.
pub(crate) const WINDOW: Duration = Duration::from_secs(60);

/// How old a stream must be before it is judged at all. A connection that
/// is still ramping up is not a stall.
pub(crate) const GRACE: Duration = Duration::from_secs(30);

/// How often the chunk loop asks the detector, so a stream that sends
/// nothing at all is judged on time.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The window and grace in force: the fixed constants, except under
/// `cfg(test)` while a [`PolicyOverride`] is alive on this thread.
pub(crate) fn policy() -> (Duration, Duration) {
    #[cfg(test)]
    if let Some(policy) = test_policy::OVERRIDE.with(|cell| cell.get()) {
        return policy;
    }
    (WINDOW, GRACE)
}

#[cfg(test)]
pub(crate) use test_policy::PolicyOverride;

#[cfg(test)]
mod test_policy {
    use std::cell::Cell;
    use std::time::Duration;

    thread_local! {
        pub(super) static OVERRIDE: Cell<Option<(Duration, Duration)>> = const { Cell::new(None) };
    }

    /// Shrinks the window and grace for the rest of the test that holds it.
    /// Works for the single-threaded `#[tokio::test]` runtime, where the
    /// download code runs on the test's own thread.
    pub(crate) struct PolicyOverride;

    impl PolicyOverride {
        pub(crate) fn new(window: Duration, grace: Duration) -> Self {
            OVERRIDE.with(|cell| cell.set(Some((window, grace))));
            PolicyOverride
        }
    }

    impl Drop for PolicyOverride {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(None));
        }
    }
}

/// Sliding-window throughput check for one download stream.
///
/// Bytes are credited to per-second buckets covering the last `window`.
/// [`check`](Self::check) is `Some` when the stream is older than `grace`
/// and the bytes in the closed buckets, divided by how many whole seconds
/// they span (the window less one, or the stream's age while younger than
/// that), fall below the floor. The bucket for the second in progress is
/// left out: the check runs right after a rollover, so counting that
/// nearly empty bucket would read a stream at exactly the floor as 59/60
/// of it.
///
/// Every method takes `now` so the caller owns the clock.
#[derive(Debug, Clone)]
pub(crate) struct StallDetector {
    min_bytes_per_sec: u64,
    window_secs: u64,
    grace: Duration,
    started: Instant,
    /// Bytes received in each of the last `window_secs` whole seconds,
    /// indexed by `second % window_secs`.
    buckets: Vec<u64>,
    /// Seconds since `started` of the bucket last written.
    current_second: u64,
}

impl StallDetector {
    /// A detector for a stream that started at `now` with the given floor
    /// in bytes per second, averaging over `window` after `grace`. The floor
    /// must be above zero; a zero floor means detection is off and the
    /// caller should not build one. `window` is rounded down to whole
    /// seconds, at least two, so that one closed second is always available
    /// besides the one in progress.
    pub(crate) fn new(
        min_bytes_per_sec: u64,
        window: Duration,
        grace: Duration,
        now: Instant,
    ) -> Self {
        debug_assert!(min_bytes_per_sec > 0, "a zero floor disables detection");
        let window_secs = window.as_secs().max(2);
        Self {
            min_bytes_per_sec,
            window_secs,
            grace,
            started: now,
            buckets: vec![0; window_secs as usize],
            current_second: 0,
        }
    }

    /// The window length in whole seconds.
    pub(crate) fn window_secs(&self) -> u64 {
        self.window_secs
    }

    /// Credit `bytes` received at `now` to the window.
    pub(crate) fn record(&mut self, now: Instant, bytes: u64) {
        let second = self.roll_to(now);
        self.buckets[(second % self.window_secs) as usize] += bytes;
    }

    /// Judge the stream at `now`.
    ///
    /// `None` while the stream is younger than the grace or is keeping up.
    /// `Some(observed)` when it has fallen below the floor, where `observed`
    /// is the average in bytes per second over the closed seconds that
    /// failed the check.
    pub(crate) fn check(&mut self, now: Instant) -> Option<u64> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < self.grace {
            return None;
        }
        let current = (self.roll_to(now) % self.window_secs) as usize;
        let closed: u64 = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != current)
            .map(|(_, b)| *b)
            .sum();
        let span_secs = elapsed.as_secs().min(self.window_secs - 1).max(1);
        let observed = closed / span_secs;
        (observed < self.min_bytes_per_sec).then_some(observed)
    }

    /// Move the window forward to the second containing `now`, zeroing the
    /// buckets for any seconds skipped since the last call, and return that
    /// second.
    fn roll_to(&mut self, now: Instant) -> u64 {
        let second = now.saturating_duration_since(self.started).as_secs();
        if second > self.current_second {
            let gap = second - self.current_second;
            if gap >= self.window_secs {
                self.buckets.iter_mut().for_each(|b| *b = 0);
            } else {
                for s in (self.current_second + 1)..=second {
                    self.buckets[(s % self.window_secs) as usize] = 0;
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
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        for s in 0..30 {
            assert_eq!(d.check(at(start, s)), None, "second {s}");
        }
    }

    #[test]
    fn silent_stream_stalls_at_end_of_grace() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        assert_eq!(d.check(at(start, 30)), Some(0));
    }

    #[test]
    fn steady_stream_above_floor_never_stalls() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
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
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        feed(&mut d, start, 0..30, 15 * KIB);
        assert_eq!(d.check(at(start, 30)), None);
    }

    #[test]
    fn burst_then_silence_stalls_once_the_burst_leaves_the_window() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        d.record(at(start, 1), 2 * 1024 * KIB);
        // At 60 s the burst is still in the window: 2 MiB / 60 s ≈ 34 KiB/s.
        assert_eq!(d.check(at(start, 60)), None);
        // At 62 s the bucket for second 1 has been rolled out.
        assert_eq!(d.check(at(start, 62)), Some(0));
    }

    #[test]
    fn drip_below_floor_stalls() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        for s in [0, 10, 20] {
            d.record(at(start, s), 1);
        }
        // 3 bytes over 30 s rounds down to 0 B/s.
        assert_eq!(d.check(at(start, 30)), Some(0));
    }

    #[test]
    fn fast_then_slow_stalls_when_the_window_average_drops() {
        // 100 KiB/s for seconds 0..60, then 1 KiB/s. At second t >= 60 the
        // closed buckets cover seconds t-59..t-1: (119 - t) fast seconds and
        // (t - 60) slow ones, so the average in KiB/s is
        // (100 * (119 - t) + (t - 60)) / 59. At t = 113 that is 653 / 59 =
        // 11.1, still above the floor; at t = 114 it is 554 / 59 = 9.4.
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
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
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        d.record(at(start, 1), 1024 * KIB);
        d.record(at(start, 200), 1);
        assert_eq!(d.check(at(start, 200)), Some(0));
    }

    #[test]
    fn observed_rate_is_reported() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        feed(&mut d, start, 0..40, 5 * KIB);
        assert_eq!(d.check(at(start, 40)), Some(5 * KIB));
    }

    #[test]
    fn policy_is_the_fixed_constants_unless_overridden() {
        assert_eq!(policy(), (WINDOW, GRACE));
        {
            let _o = PolicyOverride::new(Duration::from_secs(2), Duration::from_secs(1));
            assert_eq!(policy(), (Duration::from_secs(2), Duration::from_secs(1)));
        }
        assert_eq!(policy(), (WINDOW, GRACE));
    }

    #[test]
    fn short_window_uses_its_own_bucket_count() {
        let start = Instant::now();
        let mut d =
            StallDetector::new(FLOOR, Duration::from_secs(2), Duration::from_secs(1), start);
        assert_eq!(d.window_secs(), 2);
        d.record(at(start, 0), 50 * KIB);
        // At 1 s the closed second 0 holds the 50 KiB: 50 KiB over 1 s.
        assert_eq!(d.check(at(start, 1)), None);
        // At 3 s the window is seconds 2..=3 and the burst is gone.
        assert_eq!(d.check(at(start, 3)), Some(0));
    }

    #[test]
    fn window_is_at_least_two_seconds() {
        let start = Instant::now();
        let d = StallDetector::new(FLOOR, Duration::from_millis(300), Duration::ZERO, start);
        assert_eq!(d.window_secs(), 2);
    }

    #[test]
    fn recording_in_the_same_second_accumulates() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        for _ in 0..4 {
            d.record(at(start, 29), 100 * KIB);
        }
        // 400 KiB over the stream's 30 s age is above the floor.
        assert_eq!(d.check(at(start, 30)), None);
    }

    /// The check runs right after a bucket rolls over, so the current
    /// second is nearly empty. Counting it would read a stream at exactly
    /// the floor as 59/60 of the floor and stall it.
    #[test]
    fn rate_exactly_at_floor_is_not_a_stall() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        for s in 0..180 {
            d.record(at(start, s), FLOOR);
            assert_eq!(d.check(at(start, s)), None, "second {s}");
        }
    }

    #[test]
    fn bytes_in_the_current_partial_second_do_not_count_yet() {
        let start = Instant::now();
        let mut d = StallDetector::new(FLOOR, WINDOW, GRACE, start);
        d.record(at(start, 30), 100 * KIB);
        // Nothing before second 30, and second 30 is still open.
        assert_eq!(d.check(at(start, 30)), Some(0));
        // One second later it has closed and counts: 100 KiB over 31 s.
        assert_eq!(d.check(at(start, 31)), Some(100 * KIB / 31));
    }
}
