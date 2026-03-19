//! Shared S3 task state used across all tabs.
//!
//! Extracted from `upload_app.rs` so that the Upload tab, Tasks tab, and
//! Errors tab can all access S3 task counts and the full task list.

use std::time::{Duration, Instant};

/// Poll interval for S3 task queries.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// A simplified task entry for display in the Tasks tab.
#[derive(Debug, Clone)]
pub struct S3TaskEntry {
    pub identifier: String,
    pub cmd: String,
    pub submitter: String,
    pub status: String,
    pub submittime: String,
}

/// Shared S3 task state used across all tabs.
#[derive(Debug)]
pub struct S3TaskState {
    /// Summary counts from the user's tasks.
    pub queued: u32,
    pub running: u32,
    pub errors: u32,
    /// Total task count across all users.
    pub global_count: u32,
    /// Breakdown of global task counts by status.
    pub global_queued: u32,
    pub global_running: u32,
    pub global_errors: u32,
    /// Whether any item is currently rate-limited.
    pub is_rate_limited: bool,
    /// Full task list for the Tasks tab.
    pub tasks: Vec<S3TaskEntry>,
    /// Whether to show global tasks (true) or user tasks (false).
    pub show_global: bool,
    /// When we last polled the API. `None` means never polled — needs immediate poll.
    last_polled: Option<Instant>,
}

impl S3TaskState {
    pub fn new() -> Self {
        Self {
            queued: 0,
            running: 0,
            errors: 0,
            global_count: 0,
            global_queued: 0,
            global_running: 0,
            global_errors: 0,
            is_rate_limited: false,
            tasks: Vec::new(),
            show_global: false,
            last_polled: None,
        }
    }

    pub fn update_summary(&mut self, queued: u32, running: u32, errors: u32) {
        self.queued = queued;
        self.running = running;
        self.errors = errors;
    }

    pub fn update_global_count(&mut self, count: u32) {
        self.global_count = count;
    }

    pub fn update_global_summary(&mut self, queued: u32, running: u32, errors: u32) {
        self.global_queued = queued;
        self.global_running = running;
        self.global_errors = errors;
    }

    pub fn update_tasks(&mut self, tasks: Vec<S3TaskEntry>) {
        self.tasks = tasks;
    }

    pub fn toggle_view(&mut self) {
        self.show_global = !self.show_global;
    }

    pub fn set_rate_limited(&mut self, limited: bool) {
        self.is_rate_limited = limited;
    }

    #[allow(dead_code)] // Called from dashboard polling loop in upload_app.rs
    pub fn needs_poll(&self) -> bool {
        self.last_polled
            .is_none_or(|t| t.elapsed() >= POLL_INTERVAL)
    }

    pub fn mark_polled(&mut self) {
        self.last_polled = Some(Instant::now());
    }

    pub fn seconds_since_poll(&self) -> u64 {
        self.last_polled.map_or(0, |t| t.elapsed().as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let state = S3TaskState::new();
        assert_eq!(state.queued, 0);
        assert_eq!(state.running, 0);
        assert_eq!(state.errors, 0);
        assert_eq!(state.global_count, 0);
        assert_eq!(state.global_queued, 0);
        assert_eq!(state.global_running, 0);
        assert_eq!(state.global_errors, 0);
        assert!(!state.is_rate_limited);
        assert!(state.tasks.is_empty());
        assert!(!state.show_global);
    }

    #[test]
    fn test_toggle_view() {
        let mut state = S3TaskState::new();
        assert!(!state.show_global);
        state.toggle_view();
        assert!(state.show_global);
        state.toggle_view();
        assert!(!state.show_global);
    }

    #[test]
    fn test_update_from_summary() {
        let mut state = S3TaskState::new();
        state.update_summary(23, 4, 1);
        assert_eq!(state.queued, 23);
        assert_eq!(state.running, 4);
        assert_eq!(state.errors, 1);
    }

    #[test]
    fn test_update_global_count() {
        let mut state = S3TaskState::new();
        state.update_global_count(847);
        assert_eq!(state.global_count, 847);
    }

    #[test]
    fn test_update_global_summary() {
        let mut state = S3TaskState::new();
        state.update_global_summary(10, 5, 2);
        assert_eq!(state.global_queued, 10);
        assert_eq!(state.global_running, 5);
        assert_eq!(state.global_errors, 2);
    }

    #[test]
    fn test_update_tasks() {
        let mut state = S3TaskState::new();
        let tasks = vec![S3TaskEntry {
            identifier: "test-item".to_string(),
            cmd: "s3-put".to_string(),
            submitter: "test@example.com".to_string(),
            status: "queued".to_string(),
            submittime: "2026-03-18 14:00:00".to_string(),
        }];
        state.update_tasks(tasks);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0].identifier, "test-item");
    }

    #[test]
    fn test_set_rate_limited() {
        let mut state = S3TaskState::new();
        state.set_rate_limited(true);
        assert!(state.is_rate_limited);
        state.set_rate_limited(false);
        assert!(!state.is_rate_limited);
    }

    #[test]
    fn test_needs_poll_respects_interval() {
        let mut state = S3TaskState::new();
        // Just created with backdated last_polled, should need poll
        assert!(state.needs_poll());
        // After marking polled, should not need poll
        state.mark_polled();
        assert!(!state.needs_poll());
    }

    #[test]
    fn test_needs_poll_after_construction() {
        let state = S3TaskState::new();
        // Freshly constructed with last_polled = None should need immediate poll
        assert!(
            state.needs_poll(),
            "newly constructed S3TaskState should need immediate poll"
        );
        assert_eq!(
            state.seconds_since_poll(),
            0,
            "never-polled state should report 0 seconds"
        );
    }

    #[test]
    fn test_seconds_since_poll() {
        let mut state = S3TaskState::new();
        state.mark_polled();
        assert!(state.seconds_since_poll() < 2);
    }
}
