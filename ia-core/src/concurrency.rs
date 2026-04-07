//! Adaptive concurrency limiter using AIMD (Additive Increase, Multiplicative Decrease).
//!
//! Provides [`AdaptiveLimiter`], a concurrency gate that automatically adjusts
//! the number of in-flight operations based on server feedback (429 responses).
//! Designed for reuse across any batch operation — metadata export, download, writes.
//!
//! # Modes
//!
//! - **Adaptive** ([`AdaptiveLimiter::new`]): starts at `initial` concurrency,
//!   ramps up on sustained success, backs off on 429.
//! - **Fixed** ([`AdaptiveLimiter::fixed`]): holds concurrency constant,
//!   still pauses all workers on 429.
//!
//! # Algorithm
//!
//! Same principle as TCP congestion control:
//! - **Additive increase**: after `target` consecutive successes, increment
//!   concurrency by 1 (up to `ceiling`).
//! - **Multiplicative decrease**: on 429, halve concurrency (down to `floor`)
//!   and pause all workers for the server's `Retry-After` duration.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::rate_limit::RateLimiter;

/// Adaptive concurrency limiter.
///
/// Clone is cheap (inner state is `Arc`-shared).
#[derive(Clone)]
pub struct AdaptiveLimiter {
    inner: Arc<Inner>,
}

struct Inner {
    /// Current concurrency target (the "window").
    target: AtomicUsize,
    /// Number of permits currently held by workers.
    active: AtomicUsize,
    /// Minimum concurrency — never goes below this.
    floor: usize,
    /// Maximum concurrency — never goes above this.
    ceiling: usize,
    /// Whether AIMD adjustment is enabled.
    adaptive: bool,
    /// Global pause on 429 — shared across all workers.
    rate_limiter: RateLimiter,
    /// Wakes waiters when a permit is released or target increases.
    notify: Notify,
    /// Successes since last concurrency increase.
    successes_since_increase: AtomicUsize,
    /// Incremented on every backoff; lets `on_success` detect stale counts.
    backoff_generation: AtomicUsize,
}

/// RAII permit returned by [`AdaptiveLimiter::acquire`].
///
/// Decrements the active count and notifies waiters on drop.
pub struct AdaptivePermit {
    inner: Arc<Inner>,
}

impl Drop for AdaptivePermit {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

impl AdaptiveLimiter {
    /// Create an adaptive limiter that adjusts concurrency via AIMD.
    ///
    /// - `initial`: starting concurrency
    /// - `floor`: minimum concurrency (never drops below this on backoff)
    /// - `ceiling`: maximum concurrency (never exceeds this on ramp-up)
    ///
    /// # Panics
    ///
    /// Panics if `floor == 0`, `initial < floor`, or `initial > ceiling`.
    pub fn new(initial: usize, floor: usize, ceiling: usize) -> Self {
        assert!(floor > 0, "floor must be > 0");
        assert!(initial >= floor, "initial must be >= floor");
        assert!(initial <= ceiling, "initial must be <= ceiling");

        Self {
            inner: Arc::new(Inner {
                target: AtomicUsize::new(initial),
                active: AtomicUsize::new(0),
                floor,
                ceiling,
                adaptive: true,
                rate_limiter: RateLimiter::new(),
                notify: Notify::new(),
                successes_since_increase: AtomicUsize::new(0),
                backoff_generation: AtomicUsize::new(0),
            }),
        }
    }

    /// Create a fixed-concurrency limiter (no AIMD adjustment).
    ///
    /// Still pauses all workers on 429 via [`on_rate_limited`](Self::on_rate_limited).
    ///
    /// # Panics
    ///
    /// Panics if `n == 0`.
    pub fn fixed(n: usize) -> Self {
        assert!(n > 0, "fixed concurrency must be > 0");

        Self {
            inner: Arc::new(Inner {
                target: AtomicUsize::new(n),
                active: AtomicUsize::new(0),
                floor: n,
                ceiling: n,
                adaptive: false,
                rate_limiter: RateLimiter::new(),
                notify: Notify::new(),
                successes_since_increase: AtomicUsize::new(0),
                backoff_generation: AtomicUsize::new(0),
            }),
        }
    }

    /// Acquire a concurrency permit.
    ///
    /// Blocks until `active < target` and not paused by a 429. Returns an
    /// RAII [`AdaptivePermit`] that releases the slot on drop.
    pub async fn acquire(&self) -> AdaptivePermit {
        loop {
            // Respect global 429 pause before trying to acquire.
            self.inner.rate_limiter.wait_if_paused().await;

            let active = self.inner.active.load(Ordering::SeqCst);
            let target = self.inner.target.load(Ordering::SeqCst);

            if active < target {
                // Try to claim a slot via CAS.
                if self
                    .inner
                    .active
                    .compare_exchange(active, active + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return AdaptivePermit {
                        inner: Arc::clone(&self.inner),
                    };
                }
                // CAS failed — another task grabbed the slot, retry immediately.
            } else {
                // At capacity — wait for a release or target increase.
                self.inner.notify.notified().await;
            }
        }
    }

    /// Record a successful request.
    ///
    /// In adaptive mode, increments the success counter. After `target`
    /// consecutive successes, increases concurrency by 1 (up to ceiling).
    /// No-op in fixed mode.
    pub fn on_success(&self) {
        if !self.inner.adaptive {
            return;
        }

        let gen = self.inner.backoff_generation.load(Ordering::SeqCst);
        let count = self
            .inner
            .successes_since_increase
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let target = self.inner.target.load(Ordering::SeqCst);

        // After `target` successes, try to bump concurrency by 1.
        // CAS on the counter ensures only one thread wins the reset.
        if count >= target && target < self.inner.ceiling {
            // A backoff between our fetch_add and this check invalidated
            // our count — the counter was already reset to 0.
            if self.inner.backoff_generation.load(Ordering::SeqCst) != gen {
                return;
            }
            if self
                .inner
                .successes_since_increase
                .compare_exchange(count, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // Another thread already reset — they'll handle the bump.
                return;
            }

            // CAS loop to increment target by 1, respecting ceiling.
            loop {
                let current = self.inner.target.load(Ordering::SeqCst);
                if current >= self.inner.ceiling {
                    break;
                }
                if self
                    .inner
                    .target
                    .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    // Wake a waiter so they can use the new slot.
                    self.inner.notify.notify_waiters();
                    break;
                }
            }
        }
    }

    /// Record a 429 rate-limit response.
    ///
    /// In adaptive mode, halves the concurrency target (respecting floor).
    /// In both modes, pauses all workers for `retry_after` seconds.
    ///
    /// The `on_pause` callback fires when this task wins the pause race
    /// (only one task actually sleeps; others just wait). Use it for
    /// status messages.
    pub async fn on_rate_limited(
        &self,
        retry_after: u64,
        on_pause: impl FnOnce(usize, usize, u64),
    ) {
        let old_target = self.inner.target.load(Ordering::SeqCst);

        if self.inner.adaptive {
            // Multiplicative decrease: halve target, respecting floor.
            loop {
                let current = self.inner.target.load(Ordering::SeqCst);
                let new_target = (current / 2).max(self.inner.floor);
                if new_target == current {
                    break;
                }
                if self
                    .inner
                    .target
                    .compare_exchange(current, new_target, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    // Reset success counter and bump generation so stale
                    // on_success calls don't undo the backoff.
                    self.inner
                        .successes_since_increase
                        .store(0, Ordering::SeqCst);
                    self.inner.backoff_generation.fetch_add(1, Ordering::SeqCst);
                    break;
                }
            }
        }

        let new_target = self.inner.target.load(Ordering::SeqCst);
        self.inner
            .rate_limiter
            .pause_for(retry_after, |secs| {
                on_pause(old_target, new_target, secs);
            })
            .await;
    }

    /// Current concurrency target.
    pub fn target(&self) -> usize {
        self.inner.target.load(Ordering::SeqCst)
    }

    /// Whether this limiter uses adaptive AIMD.
    pub fn is_adaptive(&self) -> bool {
        self.inner.adaptive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn acquire_respects_target() {
        let lim = AdaptiveLimiter::fixed(2);

        let _p1 = lim.acquire().await;
        let _p2 = lim.acquire().await;

        // Third acquire should not complete immediately.
        let result = tokio::time::timeout(Duration::from_millis(50), lim.acquire()).await;
        assert!(result.is_err(), "should have timed out at capacity");
    }

    #[tokio::test]
    async fn permit_drop_releases_slot() {
        let lim = AdaptiveLimiter::fixed(1);

        let p = lim.acquire().await;
        drop(p);

        // Should be able to acquire again immediately.
        let result = tokio::time::timeout(Duration::from_millis(50), lim.acquire()).await;
        assert!(result.is_ok(), "should acquire after permit dropped");
    }

    #[tokio::test]
    async fn on_success_increases_target() {
        let lim = AdaptiveLimiter::new(2, 2, 100);
        assert_eq!(lim.target(), 2);

        // Need `target` (2) successes to bump by 1.
        lim.on_success();
        assert_eq!(lim.target(), 2); // not yet
        lim.on_success();
        assert_eq!(lim.target(), 3); // bumped!

        // Now need 3 successes for next bump.
        lim.on_success();
        lim.on_success();
        assert_eq!(lim.target(), 3); // not yet
        lim.on_success();
        assert_eq!(lim.target(), 4); // bumped!
    }

    #[tokio::test]
    async fn on_rate_limited_halves_target() {
        let lim = AdaptiveLimiter::new(20, 2, 200);
        assert_eq!(lim.target(), 20);

        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 10);

        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 5);

        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 2); // floor is 2, 5/2=2

        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 2); // stays at floor
    }

    #[tokio::test]
    async fn fixed_mode_no_aimd() {
        let lim = AdaptiveLimiter::fixed(10);
        assert_eq!(lim.target(), 10);
        assert!(!lim.is_adaptive());

        // Successes should not change target.
        for _ in 0..100 {
            lim.on_success();
        }
        assert_eq!(lim.target(), 10);

        // Rate limit should not change target (but still pauses).
        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 10);
    }

    #[tokio::test]
    async fn ceiling_respected() {
        let lim = AdaptiveLimiter::new(3, 2, 4);
        assert_eq!(lim.target(), 3);

        // 3 successes → bump to 4
        for _ in 0..3 {
            lim.on_success();
        }
        assert_eq!(lim.target(), 4);

        // 4 more successes → should NOT exceed ceiling of 4
        for _ in 0..4 {
            lim.on_success();
        }
        assert_eq!(lim.target(), 4);
    }

    #[tokio::test]
    async fn on_rate_limited_pauses_all() {
        let lim = AdaptiveLimiter::new(10, 2, 200);

        // Start a 1-second pause.
        let lim2 = lim.clone();
        let pause_handle = tokio::spawn(async move {
            lim2.on_rate_limited(1, |_, _, _| {}).await;
        });

        // Give it time to enter the pause.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Acquire should block during the pause.
        let lim3 = lim.clone();
        let acquire_result = tokio::time::timeout(Duration::from_millis(200), lim3.acquire()).await;
        assert!(acquire_result.is_err(), "acquire should block while paused");

        // Wait for pause to end.
        pause_handle.await.unwrap();

        // Now acquire should succeed.
        let acquire_result = tokio::time::timeout(Duration::from_millis(200), lim.acquire()).await;
        assert!(
            acquire_result.is_ok(),
            "acquire should succeed after pause ends"
        );
    }

    #[tokio::test]
    async fn concurrent_acquire_release() {
        let lim = AdaptiveLimiter::fixed(5);
        let mut handles = Vec::new();

        for _ in 0..20 {
            let lim = lim.clone();
            handles.push(tokio::spawn(async move {
                let _permit = lim.acquire().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }));
        }

        // All 20 tasks should complete (5 at a time).
        let result =
            tokio::time::timeout(Duration::from_secs(2), futures::future::join_all(handles)).await;
        assert!(result.is_ok(), "all tasks should complete");
    }

    #[test]
    #[should_panic(expected = "floor must be > 0")]
    fn zero_floor_panics() {
        AdaptiveLimiter::new(1, 0, 10);
    }

    #[test]
    #[should_panic(expected = "initial must be >= floor")]
    fn initial_below_floor_panics() {
        AdaptiveLimiter::new(1, 5, 10);
    }

    #[test]
    #[should_panic(expected = "initial must be <= ceiling")]
    fn initial_above_ceiling_panics() {
        AdaptiveLimiter::new(20, 2, 10);
    }

    #[test]
    #[should_panic(expected = "fixed concurrency must be > 0")]
    fn fixed_zero_panics() {
        AdaptiveLimiter::fixed(0);
    }

    #[tokio::test]
    async fn on_success_after_backoff_does_not_bump() {
        let lim = AdaptiveLimiter::new(10, 2, 100);
        assert_eq!(lim.target(), 10);

        // 9 successes — one short of the threshold.
        for _ in 0..9 {
            lim.on_success();
        }
        assert_eq!(lim.target(), 10);

        // Backoff: halves target to 5, resets counter, bumps generation.
        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 5);

        // The "stale 10th" success — should NOT bump target.
        lim.on_success();
        assert_eq!(lim.target(), 5, "stale success must not undo backoff");
    }

    #[tokio::test]
    async fn drain_after_backoff() {
        // After backoff halves target, re-acquiring should respect the new limit.
        let lim = AdaptiveLimiter::new(10, 2, 100);

        // Backoff: halves target to 5.
        lim.on_rate_limited(0, |_, _, _| {}).await;
        assert_eq!(lim.target(), 5);

        // Acquire 5 permits — should all succeed.
        let mut permits = Vec::new();
        for _ in 0..5 {
            let p = tokio::time::timeout(Duration::from_millis(50), lim.acquire()).await;
            assert!(p.is_ok(), "should acquire within new target");
            permits.push(p.unwrap());
        }

        // 6th acquire should block (active=5, target=5).
        let blocked = tokio::time::timeout(Duration::from_millis(50), lim.acquire()).await;
        assert!(blocked.is_err(), "should block at capacity after backoff");
    }
}
