//! Upload dashboard state machine.
//!
//! Tracks per-item and per-file upload progress with byte-level granularity,
//! rate limit status, and throughput sampling.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ia_core::upload::{UploadProgress, UploadProgressStatus};

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
    pub done: bool,
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
            done: false,
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
                UploadProgressStatus::Enumerated { .. } => {
                    // File list is now known — nothing to update in per-item
                    // state here since Verifying events fill in the counts.
                }
                UploadProgressStatus::Verifying => {
                    if item.status == UploadItemStatus::Pending {
                        item.status = UploadItemStatus::Verifying;
                        item.started_at = Instant::now();
                    }
                    // Guard: only count a file once (prevents double-count on retry).
                    if !self.active_files.contains_key(&fk) {
                        item.files_total += 1;
                    }
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
                UploadProgressStatus::Retrying => {
                    // Treat the same as rate limited in the TUI
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
                UploadProgressStatus::Skipped | UploadProgressStatus::Resumed => {
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
            UploadProgressStatus::Enumerated { .. } => {
                // File list and total size are known — the TUI derives these
                // values incrementally via Verifying events, so nothing to do.
            }
            UploadProgressStatus::Verifying => {
                // Guard: only count a file once (prevents double-count on retry).
                if self.active_files.contains_key(&fk) {
                    return;
                }
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
            UploadProgressStatus::Retrying => {
                if let Some(fp) = self.active_files.get_mut(&fk) {
                    fp.status = UploadProgressStatus::Retrying;
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
            UploadProgressStatus::Skipped | UploadProgressStatus::Resumed => {
                self.active_files.remove(&fk);
                self.files_skipped += 1;
            }
            UploadProgressStatus::Failed => {
                self.active_files.remove(&fk);
                self.files_failed += 1;
                self.failed_files.push((p.key, "upload failed".to_string()));
            }
        }

        // Sample throughput
        self.throughput.set_bytes(self.bytes_uploaded);
        self.throughput.maybe_sample();
    }
}

// ---------------------------------------------------------------------------
// Shared dashboard lifecycle
// ---------------------------------------------------------------------------

/// Shared dashboard lifecycle: poll S3 tasks, run the event loop, collect
/// results, and print a summary. Used by both [`run_upload_tui`] and
/// [`run_upload_batch_tui`].
async fn run_dashboard_and_summarize(
    client: &ia_core::IaClient,
    state: Arc<Mutex<UploadTuiState>>,
    mut terminal: super::framework::Term,
    _guard: super::framework::TerminalGuard,
    handles: Vec<
        tokio::task::JoinHandle<
            std::result::Result<Vec<ia_core::upload::UploadResult>, ia_core::error::IaError>,
        >,
    >,
    joblog_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use std::time::Duration;

    use super::dashboard::MultiTabDashboard;
    use super::joblog_state::JoblogState;
    use super::s3_state::{S3TaskEntry, S3TaskState};

    // Create shared state for the multi-tab dashboard.
    let s3_state = Arc::new(Mutex::new(S3TaskState::new()));
    let joblog_state = Arc::new(Mutex::new(match joblog_path {
        Some(p) => JoblogState::open(p).unwrap_or_else(|_| JoblogState::empty()),
        None => JoblogState::empty(),
    }));

    // 1. Spawn S3 tasks polling loop (every 15s, single aggregate query).
    //    One query WITHOUT submitter filter gives true global counts.
    //    We filter locally by submitter email for user-specific counts.
    let poll_s3 = Arc::clone(&s3_state);
    let tasks_client = client.clone();
    let submitter = client.config().cookies.get("logged-in-user").cloned();
    let tasks_handle = tokio::spawn(async move {
        loop {
            // Global tasks query (no submitter filter) with catalog entries.
            match ia_core::tasks::get_tasks(
                &tasks_client,
                &ia_core::tasks::TasksQuery {
                    args: Some("*s3-put*".to_string()),
                    submitter: None,
                    catalog: Some(true),
                    history: Some(false),
                    summary: Some(true),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(value) => {
                    if let Ok(mut s3) = poll_s3.lock() {
                        // Global count from the unfiltered summary.
                        s3.update_global_count(
                            value.summary.queued
                                + value.summary.running
                                + value.summary.error
                                + value.summary.paused,
                        );

                        // Convert catalog entries to S3TaskEntry for display.
                        // TaskEntry.color maps to display status: green=running,
                        // blue=queued, red=error, brown=paused.
                        let all_entries: Vec<S3TaskEntry> = value
                            .catalog
                            .iter()
                            .map(|e| S3TaskEntry {
                                identifier: e.identifier.clone(),
                                cmd: e.cmd.clone(),
                                submitter: e.submitter.clone(),
                                status: match e.color.as_str() {
                                    "green" => "running".to_string(),
                                    "blue" => "queued".to_string(),
                                    "red" => "error".to_string(),
                                    "brown" => "paused".to_string(),
                                    other => other.to_string(),
                                },
                                submittime: e.submittime.clone(),
                            })
                            .collect();

                        // Filter for user-specific summary counts.
                        let (mut user_queued, mut user_running, mut user_errors) =
                            (0u32, 0u32, 0u32);
                        for entry in &all_entries {
                            let is_user = submitter
                                .as_ref()
                                .is_some_and(|email| entry.submitter == *email);
                            if is_user {
                                match entry.status.as_str() {
                                    "queued" => user_queued += 1,
                                    "running" => user_running += 1,
                                    "error" => user_errors += 1,
                                    _ => {}
                                }
                            }
                        }
                        s3.update_summary(user_queued, user_running, user_errors);
                        s3.update_tasks(all_entries);
                        s3.mark_polled();
                    }
                }
                Err(e) => {
                    tracing::debug!("S3 tasks poll failed: {e}");
                }
            }

            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });

    // 2. Run the dashboard event loop on a blocking thread so the async
    //    upload tasks keep running on the tokio runtime.
    let tick_rate = Duration::from_millis(100);
    let dashboard_upload = Arc::clone(&state);
    let dashboard_s3 = Arc::clone(&s3_state);
    let dashboard_joblog = Arc::clone(&joblog_state);
    tokio::task::spawn_blocking(move || {
        let mut dashboard =
            MultiTabDashboard::new(dashboard_upload, dashboard_s3, dashboard_joblog);
        super::framework::run_dashboard_sync(&mut terminal, &mut dashboard, tick_rate)
    })
    .await??;

    // 3. Clean up
    tasks_handle.abort();
    drop(_guard);

    // Let the user know if uploads are still in-flight.
    let in_flight = handles.iter().filter(|h| !h.is_finished()).count();
    if in_flight > 0 {
        eprintln!("Waiting for {in_flight} in-flight upload(s) to finish (Ctrl-C to abort)...");
    }

    // 4. Collect results and print summary
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
                        ia_core::upload::UploadStatus::Skipped
                        | ia_core::upload::UploadStatus::Resumed => {
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

    let elapsed = state
        .lock()
        .map_or(Duration::ZERO, |s| s.throughput.elapsed());
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

/// Post-upload cleanup: remove lingering active files, mark item status, and
/// check if all items are done. Shared by both `run_upload_tui` and
/// `run_upload_batch_tui` task closures.
fn finalize_item(
    state: &Mutex<UploadTuiState>,
    id: &str,
    result: &std::result::Result<Vec<ia_core::upload::UploadResult>, ia_core::error::IaError>,
) {
    if let Ok(mut s) = state.lock() {
        let prefix = format!("{id}\0");
        s.active_files.retain(|k, _| !k.starts_with(&prefix));

        if let Some(&idx) = s.item_index.get(id) {
            match result {
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

        if s.items.iter().all(|i| {
            matches!(
                i.status,
                UploadItemStatus::Complete | UploadItemStatus::Failed(_)
            )
        }) {
            s.done = true;
        }
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
#[allow(clippy::too_many_arguments)]
pub async fn run_upload_tui(
    client: &ia_core::IaClient,
    identifiers: Vec<String>,
    files_per_item: Vec<Vec<std::path::PathBuf>>,
    opts: ia_core::upload::UploadOpts,
    concurrency: usize,
    skip_set: Option<Arc<std::collections::HashSet<(String, String)>>>,
    joblog_path: Option<&std::path::Path>,
    file_concurrency: usize,
) -> anyhow::Result<()> {
    // Set up terminal — the guard ensures cleanup even on panic.
    let (terminal, _guard) = super::framework::setup_terminal()?;

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
        let skip = skip_set.clone();

        handles.push(tokio::spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return Err(ia_core::error::IaError::Config(
                    "upload semaphore closed".into(),
                ));
            };
            let progress_fn: Arc<dyn Fn(UploadProgress) + Send + Sync> =
                Arc::new(move |p: UploadProgress| {
                    if let Ok(mut s) = progress_state.lock() {
                        s.update(p);
                    }
                });
            let result = ia_core::upload::upload_item(
                &client,
                &id,
                &files,
                &opts,
                Some(progress_fn),
                skip.as_deref(),
                None,
                file_concurrency,
            )
            .await;

            finalize_item(&cleanup_state, &id, &result);
            result
        }));
    }

    run_dashboard_and_summarize(client, state, terminal, _guard, handles, joblog_path).await
}

// ---------------------------------------------------------------------------
// Batch (import) entry point
// ---------------------------------------------------------------------------

/// Entry point for `ia upload --spreadsheet <FILE> --dashboard`.
///
/// Groups spreadsheet records by identifier (reusing `batch::group_records`),
/// validates them, then runs the upload dashboard with per-item concurrency
/// controlled by a semaphore matching the `--jobs` parameter.
pub async fn run_upload_batch_tui(
    client: &ia_core::IaClient,
    records: Vec<ia_core::spreadsheet::SpreadsheetRecord>,
    opts: ia_core::upload::UploadOpts,
    jobs: usize,
    skip_set: Option<Arc<std::collections::HashSet<(String, String)>>>,
    joblog_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    // 1. Group and validate (reuse batch.rs logic)
    let groups = ia_core::upload::batch::group_records(records)?;
    ia_core::upload::batch::validate_groups(&groups)?;

    // 2. Extract identifiers for TUI state initialization
    let identifiers: Vec<String> = groups.iter().map(|g| g.identifier.clone()).collect();

    // 3. Set up terminal — the guard ensures cleanup even on panic.
    let (terminal, _guard) = super::framework::setup_terminal()?;
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
        let skip = skip_set.clone();

        handles.push(tokio::spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return Err(ia_core::error::IaError::Config(
                    "upload semaphore closed".into(),
                ));
            };
            let id = group.identifier.clone();
            let files = group.files;

            let progress_fn: Arc<dyn Fn(UploadProgress) + Send + Sync> =
                Arc::new(move |p: UploadProgress| {
                    if let Ok(mut s) = progress_state.lock() {
                        s.update(p);
                    }
                });
            let result = ia_core::upload::upload_item(
                &client,
                &id,
                &files,
                &item_opts,
                Some(progress_fn),
                skip.as_deref(),
                None,
                1, // sequential within batch items; concurrency is across items
            )
            .await;

            finalize_item(&cleanup_state, &id, &result);
            result
        }));
    }

    // 5-7. Run dashboard lifecycle (tasks polling, event loop, summary).
    run_dashboard_and_summarize(client, state, terminal, _guard, handles, joblog_path).await
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
        assert_eq!(
            state.completed_files,
            VecDeque::from(vec!["file.txt".to_string()])
        );
        assert_eq!(state.items[0].status, UploadItemStatus::Complete);
        assert_eq!(state.items[0].bytes_uploaded, 1000);
    }

    // 2. Two items, verify independent tracking
    #[test]
    fn test_batch_progress_multiple_items() {
        let mut state = UploadTuiState::new(&["item-a".to_string(), "item-b".to_string()]);

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
        assert!(state
            .active_files
            .contains_key(&UploadTuiState::file_key("item-a", "good.txt")));
        assert!(!state
            .active_files
            .contains_key(&UploadTuiState::file_key("item-a", "bad.txt")));

        // Complete good.txt — item should be Failed because of bad.txt
        state.update(progress(
            "item-a",
            "good.txt",
            200,
            200,
            UploadProgressStatus::Complete,
        ));
        assert!(matches!(state.items[0].status, UploadItemStatus::Failed(_)));
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

    // --- Bounded completed_files ---

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
}
