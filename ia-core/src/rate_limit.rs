use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Shared rate limiter for metadata write operations.
///
/// When any task receives a 429, all concurrent tasks pause until
/// the wait period expires. Uses AtomicBool for pause state
/// and Notify for wake-up signaling.
#[derive(Clone)]
pub struct RateLimiter {
    paused: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Check if we're rate-limited. If so, wait until resumed.
    pub async fn wait_if_paused(&self) {
        loop {
            // Create the Notified future *before* checking the flag, so a
            // notify_waiters() that fires between the check and the await
            // is still observed. Checking first loses that wake-up and
            // deadlocks the waiter (TOCTOU race).
            let notified = self.notify.notified();
            if !self.paused.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Signal that a 429 was received. Pauses all tasks for the
    /// specified duration, then resumes. Only the first task to
    /// call this triggers the actual wait — other callers just
    /// wait for the pause to end.
    ///
    /// The `on_pause` callback is invoked with the pause duration
    /// (in seconds) when this task wins the pause race, allowing
    /// the caller to display a status message before the sleep begins.
    pub async fn pause_for(&self, seconds: u64, on_pause: impl FnOnce(u64)) {
        // Only one task should trigger the pause
        if self
            .paused
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            on_pause(seconds);
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            self.paused.store(false, Ordering::SeqCst);
            self.notify.notify_waiters();
        } else {
            // Another task already triggered a pause — just wait
            self.wait_if_paused().await;
        }
    }

    /// Whether we're currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_paused_by_default() {
        let rl = RateLimiter::new();
        assert!(!rl.is_paused());
        // wait_if_paused should return immediately when not paused
        rl.wait_if_paused().await;
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let rl = RateLimiter::new();
        let rl2 = rl.clone();

        let handle = tokio::spawn(async move {
            rl2.pause_for(1, |_| {}).await;
        });

        // Give it a moment to start the pause
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(rl.is_paused());

        handle.await.unwrap();
        assert!(!rl.is_paused());
    }

    #[tokio::test]
    async fn pause_for_invokes_on_pause_callback() {
        let rl = RateLimiter::new();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let rl2 = rl.clone();
        let handle = tokio::spawn(async move {
            rl2.pause_for(1, |secs| {
                let _ = tx.send(secs);
            })
            .await;
        });

        let reported_secs = rx.await.unwrap();
        assert_eq!(reported_secs, 1);

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn wait_if_paused_blocks_until_resume() {
        let rl = RateLimiter::new();
        let rl_pauser = rl.clone();
        let rl_waiter = rl.clone();

        // Start a short pause
        let pause_handle = tokio::spawn(async move {
            rl_pauser.pause_for(1, |_| {}).await;
        });

        // Give it a moment to start the pause
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(rl.is_paused());

        // Another task waits for the pause to end
        let wait_handle = tokio::spawn(async move {
            rl_waiter.wait_if_paused().await;
        });

        // Both should complete after the pause duration
        pause_handle.await.unwrap();
        wait_handle.await.unwrap();
        assert!(!rl.is_paused());
    }

    #[tokio::test]
    async fn multiple_pause_calls_only_one_triggers() {
        let rl = RateLimiter::new();
        let rl1 = rl.clone();
        let rl2 = rl.clone();

        let start = std::time::Instant::now();

        // Two tasks try to pause simultaneously
        let h1 = tokio::spawn(async move {
            rl1.pause_for(1, |_| {}).await;
        });
        let h2 = tokio::spawn(async move {
            // Small delay so first task wins the compare_exchange
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            rl2.pause_for(1, |_| {}).await;
        });

        h1.await.unwrap();
        h2.await.unwrap();

        let elapsed = start.elapsed();
        // Should complete in ~1 second, not ~2 (only one pause)
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "took {elapsed:?}, expected ~1s"
        );
        assert!(!rl.is_paused());
    }

    /// Regression test for a lost-wakeup (TOCTOU) race: if resume fires
    /// between the waiter's `paused` check and its `Notified` registration,
    /// the waiter must not block forever. Runs many iterations on a
    /// multi-threaded runtime to give the race window real parallelism.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resume_racing_with_wait_does_not_lose_wakeup() {
        for i in 0..50_000u32 {
            let rl = RateLimiter::new();
            rl.paused.store(true, Ordering::SeqCst);

            let rl_waiter = rl.clone();
            let waiter = tokio::spawn(async move {
                rl_waiter.wait_if_paused().await;
            });

            // Resume from this thread — races with the waiter's
            // load/registration on another worker thread. The spin sweep
            // varies the timing offset so the store+notify lands at
            // different points within the waiter's execution.
            for _ in 0..(i % 2000) {
                std::hint::spin_loop();
            }
            rl.paused.store(false, Ordering::SeqCst);
            rl.notify.notify_waiters();

            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .unwrap_or_else(|_| panic!("waiter deadlocked on iteration {i}: wake-up lost"))
                .unwrap();
        }
    }

    #[tokio::test]
    async fn is_paused_reflects_state() {
        let rl = RateLimiter::new();
        assert!(!rl.is_paused());

        let rl2 = rl.clone();
        let handle = tokio::spawn(async move {
            rl2.pause_for(1, |_| {}).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(rl.is_paused());

        handle.await.unwrap();
        assert!(!rl.is_paused());
    }
}
