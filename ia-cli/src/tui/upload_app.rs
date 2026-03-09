//! Upload dashboard state machine.
//!
//! Tracks per-item and per-file upload progress with byte-level granularity,
//! rate limit status, throughput sampling, and S3 task counts. Implements the
//! [`Dashboard`](super::framework::Dashboard) trait via [`UploadDashboard`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};

use ia_core::upload::{UploadProgress, UploadProgressStatus};

use super::framework::Dashboard;
use super::upload_ui;
use super::widgets::ThroughputTracker;

// ---------------------------------------------------------------------------
// Per-item status
// ---------------------------------------------------------------------------

/// Status of a single item in the upload batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadItemStatus {
    Pending,
    Verifying,
    Uploading,
    RateLimited,
    Complete,
    Failed(String),
}

// ---------------------------------------------------------------------------
// Per-item state
// ---------------------------------------------------------------------------

/// Aggregated state for one item being uploaded.
#[derive(Debug, Clone)]
pub struct UploadItemState {
    pub identifier: String,
    pub status: UploadItemStatus,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_uploaded: u64,
    pub started_at: Instant,
}

impl UploadItemState {
    /// Per-item throughput in bytes/sec.
    #[must_use]
    #[allow(dead_code)] // Will be used by batch dashboard (Task 9)
    pub fn throughput(&self) -> f64 {
        let secs = self.started_at.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.bytes_uploaded as f64 / secs
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// Per-file progress
// ---------------------------------------------------------------------------

/// Progress state for a single file currently being uploaded.
#[derive(Debug, Clone)]
pub struct UploadFileProgress {
    pub name: String,
    #[allow(dead_code)] // Used by batch dashboard (Task 9)
    pub identifier: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub status: UploadProgressStatus,
    #[allow(dead_code)] // Will be used for per-file ETA display
    pub started_at: Instant,
}

// ---------------------------------------------------------------------------
// Global TUI state
// ---------------------------------------------------------------------------

/// Shared state for the upload TUI dashboard.
///
/// Analogous to [`TuiState`](super::app::TuiState) for downloads but
/// tailored to upload semantics: verifying, rate limiting, S3 task counts.
#[derive(Debug, Clone)]
pub struct UploadTuiState {
    pub items: Vec<UploadItemState>,
    /// O(1) lookup from identifier to index in `items`.
    pub item_index: HashMap<String, usize>,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_uploaded: u64,
    pub bytes_total: u64,
    /// Active file uploads, keyed by `file_key(identifier, key)`.
    pub active_files: HashMap<String, UploadFileProgress>,
    pub completed_files: VecDeque<String>,
    pub failed_files: Vec<(String, String)>,
    pub throughput: ThroughputTracker,
    pub scroll_offset: usize,
    pub quit_requested: bool,
    pub done: bool,
    // S3 Tasks panel
    pub tasks_queued: u32,
    pub tasks_running: u32,
    pub tasks_error: u32,
}

impl UploadTuiState {
    /// Create a new state, pre-populating one [`UploadItemState`] per identifier.
    #[must_use]
    pub fn new(identifiers: &[String]) -> Self {
        let items: Vec<UploadItemState> = identifiers
            .iter()
            .map(|id| UploadItemState {
                identifier: id.clone(),
                status: UploadItemStatus::Pending,
                files_total: 0,
                files_completed: 0,
                files_skipped: 0,
                files_failed: 0,
                bytes_uploaded: 0,
                started_at: Instant::now(),
            })
            .collect();

        let item_index: HashMap<String, usize> = identifiers
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();

        Self {
            items,
            item_index,
            files_total: 0,
            files_completed: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_uploaded: 0,
            bytes_total: 0,
            active_files: HashMap::new(),
            completed_files: VecDeque::new(),
            failed_files: Vec::new(),
            throughput: ThroughputTracker::new(),
            scroll_offset: 0,
            quit_requested: false,
            done: false,
            tasks_queued: 0,
            tasks_running: 0,
            tasks_error: 0,
        }
    }

    /// Composite key for `active_files` that avoids collisions when multiple
    /// items contain files with the same S3 key.
    #[must_use]
    pub fn file_key(identifier: &str, key: &str) -> String {
        format!("{identifier}\0{key}")
    }

    /// Process a single progress event and update all derived state.
    pub fn update(&mut self, p: UploadProgress) {
        let fk = Self::file_key(&p.identifier, &p.key);

        // ── Per-item state ──────────────────────────────────────────
        if let Some(&idx) = self.item_index.get(&p.identifier) {
            let item = &mut self.items[idx];
            match p.status {
                UploadProgressStatus::Verifying => {
                    if item.status == UploadItemStatus::Pending {
                        item.status = UploadItemStatus::Verifying;
                        item.started_at = Instant::now();
                    }
                    item.files_total += 1;
                }
                UploadProgressStatus::Uploading => {
                    if matches!(
                        item.status,
                        UploadItemStatus::Pending
                            | UploadItemStatus::Verifying
                            | UploadItemStatus::RateLimited
                    ) {
                        item.status = UploadItemStatus::Uploading;
                    }
                    if let Some(fp) = self.active_files.get(&fk) {
                        let delta = p.bytes_sent.saturating_sub(fp.bytes_sent);
                        item.bytes_uploaded += delta;
                    }
                }
                UploadProgressStatus::WaitingRateLimit => {
                    item.status = UploadItemStatus::RateLimited;
                }
                UploadProgressStatus::Complete => {
                    item.files_completed += 1;
                    // Account for any remaining byte delta.
                    if let Some(fp) = self.active_files.get(&fk) {
                        let delta = p.bytes_sent.saturating_sub(fp.bytes_sent);
                        item.bytes_uploaded += delta;
                    } else {
                        item.bytes_uploaded += p.bytes_sent;
                    }
                    // If all files for this item are done, mark it complete.
                    if item.files_total > 0
                        && item.files_completed + item.files_skipped + item.files_failed
                            >= item.files_total
                    {
                        if item.files_failed > 0 {
                            item.status = UploadItemStatus::Failed(format!(
                                "{} file(s) failed",
                                item.files_failed
                            ));
                        } else {
                            item.status = UploadItemStatus::Complete;
                        }
                    }
                }
                UploadProgressStatus::Skipped => {
                    item.files_skipped += 1;
                    if item.files_total > 0
                        && item.files_completed + item.files_skipped + item.files_failed
                            >= item.files_total
                    {
                        item.status = UploadItemStatus::Complete;
                    }
                }
                UploadProgressStatus::Failed => {
                    item.files_failed += 1;
                    if item.files_total > 0
                        && item.files_completed + item.files_skipped + item.files_failed
                            >= item.files_total
                    {
                        item.status = UploadItemStatus::Failed(format!(
                            "{} file(s) failed",
                            item.files_failed
                        ));
                    }
                }
            }
        }

        // ── Global state ────────────────────────────────────────────
        match p.status {
            UploadProgressStatus::Verifying => {
                self.files_total += 1;
                self.bytes_total += p.total_bytes;
                self.active_files.insert(
                    fk,
                    UploadFileProgress {
                        name: p.key,
                        identifier: p.identifier,
                        bytes_sent: 0,
                        total_bytes: p.total_bytes,
                        status: UploadProgressStatus::Verifying,
                        started_at: Instant::now(),
                    },
                );
            }
            UploadProgressStatus::Uploading => {
                if let Some(fp) = self.active_files.get_mut(&fk) {
                    let delta = p.bytes_sent.saturating_sub(fp.bytes_sent);
                    self.bytes_uploaded += delta;
                    fp.bytes_sent = p.bytes_sent;
                    fp.status = UploadProgressStatus::Uploading;
                }
            }
            UploadProgressStatus::WaitingRateLimit => {
                if let Some(fp) = self.active_files.get_mut(&fk) {
                    fp.status = UploadProgressStatus::WaitingRateLimit;
                }
            }
            UploadProgressStatus::Complete => {
                if let Some(fp) = self.active_files.remove(&fk) {
                    let delta = p.bytes_sent.saturating_sub(fp.bytes_sent);
                    self.bytes_uploaded += delta;
                } else {
                    self.bytes_uploaded += p.bytes_sent;
                }
                self.files_completed += 1;
                self.completed_files.push_back(p.key);
                if self.completed_files.len() > 10 {
                    self.completed_files.pop_front();
                }
            }
            UploadProgressStatus::Skipped => {
                self.active_files.remove(&fk);
                self.files_skipped += 1;
            }
            UploadProgressStatus::Failed => {
                self.active_files.remove(&fk);
                self.files_failed += 1;
                self.failed_files
                    .push((p.key, "upload failed".to_string()));
            }
        }

        // Sample throughput
        self.throughput.set_bytes(self.bytes_uploaded);
        self.throughput.maybe_sample();
    }

    /// Overall progress as a fraction in `[0.0, 1.0]`.
    ///
    /// Uses byte-level granularity: `bytes_uploaded / bytes_total`.
    #[must_use]
    pub fn overall_progress(&self) -> f64 {
        if self.bytes_total == 0 {
            return 0.0;
        }
        (self.bytes_uploaded as f64 / self.bytes_total as f64).min(1.0)
    }

    /// Clamp scroll_offset so it doesn't scroll past the active file list.
    pub fn clamp_scroll(&mut self) {
        let max = self.active_files.len().saturating_sub(1);
        self.scroll_offset = self.scroll_offset.min(max);
    }
}

// ---------------------------------------------------------------------------
// Dashboard wrapper
// ---------------------------------------------------------------------------

/// Wrapper that implements [`Dashboard`] for the upload TUI.
pub struct UploadDashboard {
    pub state: Arc<Mutex<UploadTuiState>>,
}

impl Dashboard for UploadDashboard {
    fn draw(&self, frame: &mut ratatui::Frame) {
        let s = self.state.lock().unwrap();
        upload_ui::draw(frame, &s);
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut s = self.state.lock().unwrap();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                s.quit_requested = true;
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                s.quit_requested = true;
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                s.scroll_offset = s.scroll_offset.saturating_add(1);
                s.clamp_scroll();
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                s.scroll_offset = s.scroll_offset.saturating_sub(1);
                true
            }
            _ => false,
        }
    }

    fn is_done(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.done && s.active_files.is_empty()
    }

    fn quit_requested(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.quit_requested
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Entry point for `ia upload --dashboard`.
///
/// Sets up the terminal, spawns upload tasks with progress callbacks that
/// update shared TUI state, optionally polls the S3 tasks API, and runs
/// the dashboard event loop on a blocking thread. Prints a summary line
/// after the dashboard exits.
pub async fn run_upload_tui(
    client: &ia_core::IaClient,
    identifiers: Vec<String>,
    files_per_item: Vec<Vec<std::path::PathBuf>>,
    opts: ia_core::upload::UploadOpts,
    concurrency: usize,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // Set up terminal — the guard ensures cleanup even on panic.
    let (mut terminal, _guard) = super::framework::setup_terminal()?;

    let state = Arc::new(Mutex::new(UploadTuiState::new(&identifiers)));

    // Semaphore to limit how many items upload concurrently
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));

    // Spawn upload tasks (one per item)
    let mut handles = Vec::new();
    for (id, files) in identifiers.iter().zip(files_per_item.iter()) {
        let client = client.clone();
        let id = id.clone();
        let files = files.clone();
        let opts = opts.clone();
        let sem = Arc::clone(&semaphore);
        let progress_state = Arc::clone(&state);
        let cleanup_state = Arc::clone(&state);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let progress_fn: Arc<dyn Fn(UploadProgress) + Send + Sync> =
                Arc::new(move |p: UploadProgress| {
                    if let Ok(mut s) = progress_state.lock() {
                        s.update(p);
                    }
                });
            let result =
                ia_core::upload::upload_item(&client, &id, &files, &opts, Some(progress_fn)).await;

            // Clean up any lingering active files for this item
            if let Ok(mut s) = cleanup_state.lock() {
                let prefix = format!("{id}\0");
                s.active_files.retain(|k, _| !k.starts_with(&prefix));

                // Mark item status based on result
                if let Some(&idx) = s.item_index.get(&id) {
                    match &result {
                        Ok(_) => {
                            // upload_item's progress events handle per-file status,
                            // but ensure the item is marked complete/failed.
                            let item = &mut s.items[idx];
                            if !matches!(
                                item.status,
                                UploadItemStatus::Complete | UploadItemStatus::Failed(_)
                            ) {
                                if item.files_failed > 0 {
                                    item.status = UploadItemStatus::Failed(format!(
                                        "{} file(s) failed",
                                        item.files_failed
                                    ));
                                } else {
                                    item.status = UploadItemStatus::Complete;
                                }
                            }
                        }
                        Err(e) => {
                            s.items[idx].status =
                                UploadItemStatus::Failed(e.to_string());
                        }
                    }
                }

                // Check if all items are done
                if s.items.iter().all(|i| {
                    matches!(
                        i.status,
                        UploadItemStatus::Complete | UploadItemStatus::Failed(_)
                    )
                }) {
                    s.done = true;
                }
            }

            result
        }));
    }

    // Spawn S3 tasks polling loop (every 60s).
    // Failures are silently ignored — auth might not be configured.
    let tasks_state = Arc::clone(&state);
    let tasks_client = client.clone();
    let tasks_ids = identifiers.clone();
    let tasks_handle = tokio::spawn(async move {
        loop {
            let mut total_queued = 0u32;
            let mut total_running = 0u32;
            let mut total_error = 0u32;

            for id in &tasks_ids {
                if let Ok(value) = ia_core::tasks::get_tasks(
                    &tasks_client,
                    &ia_core::tasks::TasksQuery {
                        identifier: Some(id.clone()),
                        ..Default::default()
                    },
                )
                .await
                {
                    total_queued += value.summary.queued;
                    total_running += value.summary.running;
                    total_error += value.summary.error;
                }
            }

            if let Ok(mut s) = tasks_state.lock() {
                s.tasks_queued = total_queued;
                s.tasks_running = total_running;
                s.tasks_error = total_error;
            }

            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // Run the dashboard event loop on a blocking thread so the async
    // upload tasks keep running on the tokio runtime.
    let dashboard_state = Arc::clone(&state);
    let tick_rate = Duration::from_millis(100);
    tokio::task::spawn_blocking(move || {
        let mut dashboard = UploadDashboard {
            state: dashboard_state,
        };
        super::framework::run_dashboard_sync(&mut terminal, &mut dashboard, tick_rate)
    })
    .await??;

    // Dashboard exited — clean up
    tasks_handle.abort();
    drop(_guard);

    // Collect results and print summary
    let mut total_uploaded = 0usize;
    let mut total_skipped = 0usize;
    let mut total_failed = 0usize;
    let mut total_bytes = 0u64;

    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => {
                for r in &results {
                    match &r.status {
                        ia_core::upload::UploadStatus::Uploaded => {
                            total_uploaded += 1;
                            total_bytes += r.bytes;
                        }
                        ia_core::upload::UploadStatus::Skipped => {
                            total_skipped += 1;
                        }
                        ia_core::upload::UploadStatus::Failed(_) => {
                            total_failed += 1;
                        }
                        ia_core::upload::UploadStatus::DryRun => {
                            total_skipped += 1;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("Error: {e}");
                total_failed += 1;
            }
            Err(e) => {
                eprintln!("Task panicked: {e}");
                total_failed += 1;
            }
        }
    }

    let elapsed = state.lock().unwrap().throughput.elapsed();
    eprintln!(
        "Uploaded {} files, {} skipped, {} failed ({}) in {:.1}s",
        total_uploaded,
        total_skipped,
        total_failed,
        crate::output::format_bytes(total_bytes),
        elapsed.as_secs_f64(),
    );

    if total_failed > 0 {
        anyhow::bail!("{total_failed} file(s) failed to upload");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Batch (import) entry point
// ---------------------------------------------------------------------------

/// Entry point for `ia upload import --dashboard`.
///
/// Groups spreadsheet records by identifier (reusing `batch::group_records`),
/// validates them, then runs the upload dashboard with per-item concurrency
/// controlled by a semaphore matching the `--jobs` parameter.
pub async fn run_upload_batch_tui(
    client: &ia_core::IaClient,
    records: Vec<ia_core::spreadsheet::SpreadsheetRecord>,
    opts: ia_core::upload::UploadOpts,
    jobs: usize,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Group and validate (reuse batch.rs logic)
    let groups = ia_core::upload::batch::group_records(records)?;
    ia_core::upload::batch::validate_groups(&groups)?;

    // 2. Extract identifiers for TUI state initialization
    let identifiers: Vec<String> = groups.iter().map(|g| g.identifier.clone()).collect();

    // 3. Set up terminal — the guard ensures cleanup even on panic.
    let (mut terminal, _guard) = super::framework::setup_terminal()?;
    let state = Arc::new(Mutex::new(UploadTuiState::new(&identifiers)));

    // 4. Spawn upload tasks (one per item group, with concurrency semaphore)
    let semaphore = Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut handles = Vec::new();

    for group in groups {
        let client = client.clone();
        let mut item_opts = opts.clone();
        // Merge spreadsheet metadata — same logic as batch.rs lines 58-66:
        // spreadsheet metadata overrides CLI metadata for same keys.
        for (key, value) in group.metadata {
            if let Some(existing) = item_opts.metadata.iter_mut().find(|(k, _)| k == &key) {
                existing.1 = value;
            } else {
                item_opts.metadata.push((key, value));
            }
        }

        let sem = Arc::clone(&semaphore);
        let progress_state = Arc::clone(&state);
        let cleanup_state = Arc::clone(&state);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let id = group.identifier.clone();
            let files = group.files;

            let progress_fn: Arc<dyn Fn(UploadProgress) + Send + Sync> =
                Arc::new(move |p: UploadProgress| {
                    if let Ok(mut s) = progress_state.lock() {
                        s.update(p);
                    }
                });
            let result =
                ia_core::upload::upload_item(&client, &id, &files, &item_opts, Some(progress_fn))
                    .await;

            // Clean up lingering active files and mark item status
            if let Ok(mut s) = cleanup_state.lock() {
                let prefix = format!("{id}\0");
                s.active_files.retain(|k, _| !k.starts_with(&prefix));

                if let Some(&idx) = s.item_index.get(&id) {
                    match &result {
                        Ok(_) => {
                            let item = &mut s.items[idx];
                            if !matches!(
                                item.status,
                                UploadItemStatus::Complete | UploadItemStatus::Failed(_)
                            ) {
                                if item.files_failed > 0 {
                                    item.status = UploadItemStatus::Failed(format!(
                                        "{} file(s) failed",
                                        item.files_failed
                                    ));
                                } else {
                                    item.status = UploadItemStatus::Complete;
                                }
                            }
                        }
                        Err(e) => {
                            s.items[idx].status = UploadItemStatus::Failed(e.to_string());
                        }
                    }
                }

                // Check if all items are done
                if s.items.iter().all(|i| {
                    matches!(
                        i.status,
                        UploadItemStatus::Complete | UploadItemStatus::Failed(_)
                    )
                }) {
                    s.done = true;
                }
            }

            result
        }));
    }

    // 5. Spawn S3 tasks polling loop (every 60s).
    let tasks_state = Arc::clone(&state);
    let tasks_client = client.clone();
    let tasks_ids = identifiers.clone();
    let tasks_handle = tokio::spawn(async move {
        loop {
            let mut total_queued = 0u32;
            let mut total_running = 0u32;
            let mut total_error = 0u32;

            for id in &tasks_ids {
                if let Ok(value) = ia_core::tasks::get_tasks(
                    &tasks_client,
                    &ia_core::tasks::TasksQuery {
                        identifier: Some(id.clone()),
                        ..Default::default()
                    },
                )
                .await
                {
                    total_queued += value.summary.queued;
                    total_running += value.summary.running;
                    total_error += value.summary.error;
                }
            }

            if let Ok(mut s) = tasks_state.lock() {
                s.tasks_queued = total_queued;
                s.tasks_running = total_running;
                s.tasks_error = total_error;
            }

            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // 6. Run the dashboard event loop on a blocking thread.
    let dashboard_state = Arc::clone(&state);
    let tick_rate = Duration::from_millis(100);
    tokio::task::spawn_blocking(move || {
        let mut dashboard = UploadDashboard {
            state: dashboard_state,
        };
        super::framework::run_dashboard_sync(&mut terminal, &mut dashboard, tick_rate)
    })
    .await??;

    // 7. Dashboard exited — clean up
    tasks_handle.abort();
    drop(_guard);

    // Collect results and print summary
    let mut total_uploaded = 0usize;
    let mut total_skipped = 0usize;
    let mut total_failed = 0usize;
    let mut total_bytes = 0u64;

    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => {
                for r in &results {
                    match &r.status {
                        ia_core::upload::UploadStatus::Uploaded => {
                            total_uploaded += 1;
                            total_bytes += r.bytes;
                        }
                        ia_core::upload::UploadStatus::Skipped => {
                            total_skipped += 1;
                        }
                        ia_core::upload::UploadStatus::Failed(_) => {
                            total_failed += 1;
                        }
                        ia_core::upload::UploadStatus::DryRun => {
                            total_skipped += 1;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("Error: {e}");
                total_failed += 1;
            }
            Err(e) => {
                eprintln!("Task panicked: {e}");
                total_failed += 1;
            }
        }
    }

    let elapsed = state.lock().unwrap().throughput.elapsed();
    eprintln!(
        "Uploaded {} files, {} skipped, {} failed ({}) in {:.1}s",
        total_uploaded,
        total_skipped,
        total_failed,
        crate::output::format_bytes(total_bytes),
        elapsed.as_secs_f64(),
    );

    if total_failed > 0 {
        anyhow::bail!("{total_failed} file(s) failed to upload");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build an `UploadProgress` event.
    fn progress(
        id: &str,
        key: &str,
        bytes_sent: u64,
        total_bytes: u64,
        status: UploadProgressStatus,
    ) -> UploadProgress {
        UploadProgress {
            identifier: id.to_string(),
            key: key.to_string(),
            bytes_sent,
            total_bytes,
            status,
        }
    }

    // 1. Single file: Verifying → Uploading (with bytes) → Complete
    #[test]
    fn test_single_file_progress_tracking() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        // Verifying — registers the file
        state.update(progress(
            "item-a",
            "file.txt",
            0,
            1000,
            UploadProgressStatus::Verifying,
        ));
        assert_eq!(state.files_total, 1);
        assert_eq!(state.bytes_total, 1000);
        assert_eq!(state.active_files.len(), 1);
        assert_eq!(state.items[0].status, UploadItemStatus::Verifying);
        assert_eq!(state.items[0].files_total, 1);

        // Uploading — partial bytes
        state.update(progress(
            "item-a",
            "file.txt",
            400,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.bytes_uploaded, 400);
        assert_eq!(state.items[0].status, UploadItemStatus::Uploading);
        assert_eq!(state.items[0].bytes_uploaded, 400);

        // Uploading — more bytes
        state.update(progress(
            "item-a",
            "file.txt",
            800,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.bytes_uploaded, 800);
        assert_eq!(state.items[0].bytes_uploaded, 800);

        // Complete
        state.update(progress(
            "item-a",
            "file.txt",
            1000,
            1000,
            UploadProgressStatus::Complete,
        ));
        assert_eq!(state.bytes_uploaded, 1000);
        assert_eq!(state.files_completed, 1);
        assert!(state.active_files.is_empty());
        assert_eq!(state.completed_files, VecDeque::from(vec!["file.txt".to_string()]));
        assert_eq!(state.items[0].status, UploadItemStatus::Complete);
        assert_eq!(state.items[0].bytes_uploaded, 1000);
    }

    // 2. Two items, verify independent tracking
    #[test]
    fn test_batch_progress_multiple_items() {
        let mut state =
            UploadTuiState::new(&["item-a".to_string(), "item-b".to_string()]);

        // Verify and upload item-a
        state.update(progress(
            "item-a",
            "a.txt",
            0,
            500,
            UploadProgressStatus::Verifying,
        ));
        state.update(progress(
            "item-b",
            "b.txt",
            0,
            300,
            UploadProgressStatus::Verifying,
        ));

        assert_eq!(state.files_total, 2);
        assert_eq!(state.bytes_total, 800);
        assert_eq!(state.items[0].files_total, 1);
        assert_eq!(state.items[1].files_total, 1);

        // Progress on item-a
        state.update(progress(
            "item-a",
            "a.txt",
            250,
            500,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.items[0].bytes_uploaded, 250);
        assert_eq!(state.items[1].bytes_uploaded, 0);
        assert_eq!(state.bytes_uploaded, 250);

        // Progress on item-b
        state.update(progress(
            "item-b",
            "b.txt",
            150,
            300,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.items[0].bytes_uploaded, 250);
        assert_eq!(state.items[1].bytes_uploaded, 150);
        assert_eq!(state.bytes_uploaded, 400);

        // Complete item-a
        state.update(progress(
            "item-a",
            "a.txt",
            500,
            500,
            UploadProgressStatus::Complete,
        ));
        assert_eq!(state.items[0].status, UploadItemStatus::Complete);
        assert_eq!(state.items[1].status, UploadItemStatus::Uploading);
        assert_eq!(state.bytes_uploaded, 650);

        // Complete item-b
        state.update(progress(
            "item-b",
            "b.txt",
            300,
            300,
            UploadProgressStatus::Complete,
        ));
        assert_eq!(state.items[1].status, UploadItemStatus::Complete);
        assert_eq!(state.bytes_uploaded, 800);
        assert_eq!(state.files_completed, 2);
    }

    // 3. WaitingRateLimit updates item status
    #[test]
    fn test_rate_limit_tracking() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        state.update(progress(
            "item-a",
            "file.txt",
            0,
            1000,
            UploadProgressStatus::Verifying,
        ));
        state.update(progress(
            "item-a",
            "file.txt",
            200,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.items[0].status, UploadItemStatus::Uploading);

        // Rate limited
        state.update(progress(
            "item-a",
            "file.txt",
            200,
            1000,
            UploadProgressStatus::WaitingRateLimit,
        ));
        assert_eq!(state.items[0].status, UploadItemStatus::RateLimited);

        // File status in active_files should also reflect rate limit
        let fk = UploadTuiState::file_key("item-a", "file.txt");
        assert_eq!(
            state.active_files[&fk].status,
            UploadProgressStatus::WaitingRateLimit
        );

        // Resuming upload after rate limit clears
        state.update(progress(
            "item-a",
            "file.txt",
            500,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.items[0].status, UploadItemStatus::Uploading);
        assert_eq!(state.bytes_uploaded, 500);
    }

    // 4. Files properly counted, removed from active
    #[test]
    fn test_skipped_files() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        state.update(progress(
            "item-a",
            "a.txt",
            0,
            500,
            UploadProgressStatus::Verifying,
        ));
        state.update(progress(
            "item-a",
            "b.txt",
            0,
            300,
            UploadProgressStatus::Verifying,
        ));
        assert_eq!(state.active_files.len(), 2);
        assert_eq!(state.items[0].files_total, 2);

        // Skip a.txt
        state.update(progress(
            "item-a",
            "a.txt",
            0,
            500,
            UploadProgressStatus::Skipped,
        ));
        assert_eq!(state.files_skipped, 1);
        assert_eq!(state.active_files.len(), 1);
        assert_eq!(state.items[0].files_skipped, 1);
        // Bytes should not increase from a skip
        assert_eq!(state.bytes_uploaded, 0);

        // Complete b.txt
        state.update(progress(
            "item-a",
            "b.txt",
            300,
            300,
            UploadProgressStatus::Complete,
        ));
        assert_eq!(state.files_completed, 1);
        assert_eq!(state.items[0].status, UploadItemStatus::Complete);
        assert!(state.active_files.is_empty());
    }

    // 5. Failed files in failed_files vec
    #[test]
    fn test_failed_files_tracked_as_errors() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        state.update(progress(
            "item-a",
            "good.txt",
            0,
            200,
            UploadProgressStatus::Verifying,
        ));
        state.update(progress(
            "item-a",
            "bad.txt",
            0,
            100,
            UploadProgressStatus::Verifying,
        ));

        // Fail bad.txt
        state.update(progress(
            "item-a",
            "bad.txt",
            0,
            100,
            UploadProgressStatus::Failed,
        ));
        assert_eq!(state.files_failed, 1);
        assert_eq!(state.failed_files.len(), 1);
        assert_eq!(state.failed_files[0].0, "bad.txt");
        assert_eq!(state.items[0].files_failed, 1);
        assert!(state.active_files.contains_key(
            &UploadTuiState::file_key("item-a", "good.txt")
        ));
        assert!(!state.active_files.contains_key(
            &UploadTuiState::file_key("item-a", "bad.txt")
        ));

        // Complete good.txt — item should be Failed because of bad.txt
        state.update(progress(
            "item-a",
            "good.txt",
            200,
            200,
            UploadProgressStatus::Complete,
        ));
        assert!(matches!(
            state.items[0].status,
            UploadItemStatus::Failed(_)
        ));
    }

    // 6. Progress ratio calculation
    #[test]
    fn test_overall_progress() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        // No bytes total yet
        assert_eq!(state.overall_progress(), 0.0);

        state.update(progress(
            "item-a",
            "f.txt",
            0,
            1000,
            UploadProgressStatus::Verifying,
        ));
        assert_eq!(state.overall_progress(), 0.0);

        state.update(progress(
            "item-a",
            "f.txt",
            500,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert!((state.overall_progress() - 0.5).abs() < 0.001);

        state.update(progress(
            "item-a",
            "f.txt",
            1000,
            1000,
            UploadProgressStatus::Complete,
        ));
        assert!((state.overall_progress() - 1.0).abs() < 0.001);
    }

    // 7. Multiple Uploading events, verify cumulative bytes are correct (not double-counted)
    #[test]
    fn test_byte_delta_tracking() {
        let mut state = UploadTuiState::new(&["item-a".to_string()]);

        state.update(progress(
            "item-a",
            "file.txt",
            0,
            1000,
            UploadProgressStatus::Verifying,
        ));
        assert_eq!(state.bytes_uploaded, 0);

        // First chunk: 0 → 100
        state.update(progress(
            "item-a",
            "file.txt",
            100,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.bytes_uploaded, 100);
        assert_eq!(state.items[0].bytes_uploaded, 100);

        // Second chunk: 100 → 350
        state.update(progress(
            "item-a",
            "file.txt",
            350,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.bytes_uploaded, 350);
        assert_eq!(state.items[0].bytes_uploaded, 350);

        // Third chunk: 350 → 700
        state.update(progress(
            "item-a",
            "file.txt",
            700,
            1000,
            UploadProgressStatus::Uploading,
        ));
        assert_eq!(state.bytes_uploaded, 700);
        assert_eq!(state.items[0].bytes_uploaded, 700);

        // Complete: 700 → 1000
        state.update(progress(
            "item-a",
            "file.txt",
            1000,
            1000,
            UploadProgressStatus::Complete,
        ));
        assert_eq!(state.bytes_uploaded, 1000);
        assert_eq!(state.items[0].bytes_uploaded, 1000);
        // No double-counting — total should match total_bytes exactly
        assert_eq!(state.bytes_uploaded, state.bytes_total);
    }

    // --- Dashboard trait tests ---

    #[test]
    fn test_dashboard_quit_on_q() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let mut dash = UploadDashboard {
            state: Arc::clone(&state),
        };

        assert!(!dash.quit_requested());
        dash.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(dash.quit_requested());
    }

    #[test]
    fn test_dashboard_quit_on_esc() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let mut dash = UploadDashboard {
            state: Arc::clone(&state),
        };

        dash.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(dash.quit_requested());
    }

    #[test]
    fn test_dashboard_quit_on_ctrl_c() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let mut dash = UploadDashboard {
            state: Arc::clone(&state),
        };

        dash.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(dash.quit_requested());
    }

    #[test]
    fn test_dashboard_scroll() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let mut dash = UploadDashboard {
            state: Arc::clone(&state),
        };

        // Add active files so scroll has room to move
        {
            let mut s = state.lock().unwrap();
            for i in 0..5 {
                s.update(progress(
                    "x",
                    &format!("file-{i}.txt"),
                    0,
                    100,
                    UploadProgressStatus::Verifying,
                ));
            }
        }

        dash.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(state.lock().unwrap().scroll_offset, 1);
        dash.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(state.lock().unwrap().scroll_offset, 2);
        dash.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(state.lock().unwrap().scroll_offset, 1);
        dash.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.lock().unwrap().scroll_offset, 0);
        // Scroll up at 0 stays at 0
        dash.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.lock().unwrap().scroll_offset, 0);
    }

    #[test]
    fn test_dashboard_is_done() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let dash = UploadDashboard {
            state: Arc::clone(&state),
        };

        // Not done initially
        assert!(!dash.is_done());

        // Mark done but leave an active file — should still not be done
        {
            let mut s = state.lock().unwrap();
            s.done = true;
            s.active_files.insert(
                "x\0f.txt".to_string(),
                UploadFileProgress {
                    name: "f.txt".to_string(),
                    identifier: "x".to_string(),
                    bytes_sent: 0,
                    total_bytes: 100,
                    status: UploadProgressStatus::Uploading,
                    started_at: Instant::now(),
                },
            );
        }
        assert!(!dash.is_done());

        // Clear active files — now it should be done
        state.lock().unwrap().active_files.clear();
        assert!(dash.is_done());
    }

    #[test]
    fn test_unhandled_key_returns_false() {
        let state = Arc::new(Mutex::new(UploadTuiState::new(&["x".to_string()])));
        let mut dash = UploadDashboard {
            state: Arc::clone(&state),
        };
        assert!(!dash.handle_key(KeyCode::Char('z'), KeyModifiers::NONE));
    }

    // --- Bounded completed_files and scroll clamping ---

    #[test]
    fn completed_files_bounded() {
        let mut state = UploadTuiState::new(&["item-1".into()]);
        for i in 0..20 {
            state.update(progress(
                "item-1",
                &format!("file-{i}.txt"),
                0,
                100,
                UploadProgressStatus::Verifying,
            ));
            state.update(progress(
                "item-1",
                &format!("file-{i}.txt"),
                100,
                100,
                UploadProgressStatus::Complete,
            ));
        }
        assert!(state.completed_files.len() <= 10);
        assert_eq!(state.completed_files.back().unwrap(), "file-19.txt");
    }

    #[test]
    fn scroll_offset_clamped() {
        let mut state = UploadTuiState::new(&["item-1".into()]);
        state.update(progress(
            "item-1",
            "a.txt",
            0,
            100,
            UploadProgressStatus::Verifying,
        ));
        state.update(progress(
            "item-1",
            "b.txt",
            0,
            100,
            UploadProgressStatus::Verifying,
        ));
        state.scroll_offset = 100;
        state.clamp_scroll();
        assert!(state.scroll_offset <= 1);
    }
}
