//! The per-server cap on download connections (`--max-connections-per-server`).
//!
//! IA operations asked that one client open at most 6 connections to one
//! server at once. Every request a download sends takes a slot on the
//! server it goes to before it is sent and keeps it until its response is
//! dropped; a request that finds the server full waits for a slot. See
//! "Per-server cap" in `docs/plans/2026-11-03-parallel-range-downloads.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The cap unless the caller sets another: IA operations' limit for one
/// client.
pub const DEFAULT_MAX_CONNECTIONS_PER_SERVER: usize = 6;

/// A shared table of download slots, `max` per server.
///
/// Clones share the table, so every download that clones one
/// [`DownloadOpts`](super::DownloadOpts) shares the cap; a new
/// `ServerSlots` starts with every slot free. A server is a URL authority:
/// its host, and its port when the URL gives one.
///
/// ```
/// use ia_core::download::{DownloadOpts, ServerSlots};
///
/// let mut opts = DownloadOpts::default();
/// opts.server_slots = ServerSlots::new(2);
/// assert_eq!(opts.clone().server_slots.max(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct ServerSlots {
    inner: Arc<Table>,
}

#[derive(Debug)]
struct Table {
    max: usize,
    per_server: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ServerSlots {
    /// At most `max` download connections to each server at once; `0` is
    /// treated as `1`.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self {
            inner: Arc::new(Table {
                max: max.max(1),
                per_server: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The most connections to one server at once.
    #[must_use]
    pub fn max(&self) -> usize {
        self.inner.max
    }

    /// Wait for a slot on `server` and hold it until the permit is dropped.
    /// `None` only if the semaphore was closed, which this code never does.
    pub(crate) async fn acquire(&self, server: &str) -> Option<OwnedSemaphorePermit> {
        let semaphore = {
            let mut per_server = self
                .inner
                .per_server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(
                per_server
                    .entry(server.to_string())
                    .or_insert_with(|| Arc::new(Semaphore::new(self.inner.max))),
            )
        };
        if semaphore.available_permits() == 0 {
            tracing::debug!(
                server,
                max = self.inner.max,
                "waiting for a connection slot"
            );
        }
        semaphore.acquire_owned().await.ok()
    }
}

impl Default for ServerSlots {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONNECTIONS_PER_SERVER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Whether a slot on `server` comes within a moment.
    async fn free(slots: &ServerSlots, server: &str) -> Option<OwnedSemaphorePermit> {
        tokio::time::timeout(Duration::from_millis(100), slots.acquire(server))
            .await
            .ok()
            .flatten()
    }

    #[test]
    fn the_default_is_six_and_zero_means_one() {
        assert_eq!(ServerSlots::default().max(), 6);
        assert_eq!(DEFAULT_MAX_CONNECTIONS_PER_SERVER, 6);
        assert_eq!(ServerSlots::new(0).max(), 1);
        assert_eq!(ServerSlots::new(9).max(), 9);
    }

    #[tokio::test]
    async fn a_full_server_makes_the_next_request_wait_until_a_slot_is_given_back() {
        let slots = ServerSlots::new(2);
        let first = free(&slots, "a.example").await.expect("first slot");
        let _second = free(&slots, "a.example").await.expect("second slot");
        assert!(free(&slots, "a.example").await.is_none(), "a third slot");
        drop(first);
        assert!(free(&slots, "a.example").await.is_some(), "the freed slot");
    }

    #[tokio::test]
    async fn each_server_has_its_own_slots() {
        let slots = ServerSlots::new(1);
        let _a = free(&slots, "a.example").await.expect("a");
        assert!(
            free(&slots, "b.example").await.is_some(),
            "b is another server"
        );
        assert!(
            free(&slots, "a.example:8080").await.is_some(),
            "another port is another server"
        );
    }

    #[tokio::test]
    async fn clones_share_the_slots_and_new_tables_do_not() {
        let slots = ServerSlots::new(1);
        let _held = free(&slots, "a.example").await.expect("slot");
        assert!(free(&slots.clone(), "a.example").await.is_none(), "a clone");
        assert!(
            free(&ServerSlots::new(1), "a.example").await.is_some(),
            "a new table"
        );
    }
}
