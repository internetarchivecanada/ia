use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use tokio::sync::Semaphore;

use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus};
use ia_core::IaClient;

use super::framework::{self, Dashboard};
use super::ui;
use super::widgets::ThroughputTracker;

/// Per-item download state for batch tracking.
#[derive(Debug, Clone)]
pub struct ItemState {
    pub identifier: String,
    pub status: ItemStatus,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub started_at: Instant,
}

/// Status of a single item in batch mode.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemStatus {
    Pending,
    Downloading,
    Complete,
    Failed(String),
}

impl ItemState {
    pub fn throughput(&self) -> f64 {
        let secs = self.started_at.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.bytes_downloaded as f64 / secs
        } else {
            0.0
        }
    }
}

/// Shared state for TUI rendering.
#[derive(Debug, Clone)]
pub struct TuiState {
    pub identifier: String,
    pub items: Vec<ItemState>,
    /// O(1) lookup from identifier → index in `items`.
    item_index: HashMap<String, usize>,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    /// Active file downloads, keyed by "{identifier}/{file_name}" to avoid
    /// collisions when multiple items have files with the same name.
    pub active_files: HashMap<String, FileProgress>,
    pub completed_files: Vec<String>,
    pub failed_files: Vec<(String, String)>,
    pub throughput: ThroughputTracker,
    pub disk_statuses: Vec<(String, u64)>,
    #[allow(dead_code)]
    pub paused: bool,
    pub scroll_offset: usize,
    pub quit_requested: bool,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct FileProgress {
    pub name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub started_at: Instant,
}

impl TuiState {
    pub fn new(identifiers: &[String]) -> Self {
        let identifier = if identifiers.len() == 1 {
            identifiers[0].clone()
        } else {
            format!("{} items", identifiers.len())
        };

        let items: Vec<ItemState> = identifiers
            .iter()
            .map(|id| ItemState {
                identifier: id.clone(),
                status: ItemStatus::Pending,
                files_total: 0,
                files_completed: 0,
                files_skipped: 0,
                files_failed: 0,
                bytes_downloaded: 0,
                started_at: Instant::now(),
            })
            .collect();

        let item_index: HashMap<String, usize> = identifiers
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();

        Self {
            identifier,
            items,
            item_index,
            files_total: 0,
            files_completed: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_downloaded: 0,
            bytes_total: 0,
            active_files: HashMap::new(),
            completed_files: Vec::new(),
            failed_files: Vec::new(),
            throughput: ThroughputTracker::new(),
            disk_statuses: Vec::new(),
            paused: false,
            scroll_offset: 0,
            quit_requested: false,
            done: false,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.throughput.elapsed()
    }

    pub fn throughput(&self) -> f64 {
        self.throughput.throughput()
    }

    pub fn overall_progress(&self) -> f64 {
        let items_total = self.items.len();
        if items_total == 0 {
            return 0.0;
        }
        let total_progress: f64 = self
            .items
            .iter()
            .map(|item| match &item.status {
                ItemStatus::Complete | ItemStatus::Failed(_) => 1.0,
                ItemStatus::Downloading => {
                    if item.files_total > 0 {
                        (item.files_completed + item.files_skipped + item.files_failed) as f64
                            / item.files_total as f64
                    } else {
                        0.0
                    }
                }
                ItemStatus::Pending => 0.0,
            })
            .sum();
        total_progress / items_total as f64
    }

    /// Composite key for active_files to avoid collisions when multiple items
    /// have files with the same name (e.g. `_meta.xml`).
    fn file_key(identifier: &str, file_name: &str) -> String {
        format!("{identifier}\0{file_name}")
    }

    fn update(&mut self, progress: DownloadProgress) {
        let file_key = Self::file_key(&progress.identifier, &progress.file_name);

        // Update per-item state via O(1) HashMap lookup
        if let Some(&idx) = self.item_index.get(&progress.identifier) {
            let item = &mut self.items[idx];
            match &progress.status {
                DownloadStatus::Enumerated { files_count, .. } => {
                    if item.status == ItemStatus::Pending {
                        item.status = ItemStatus::Downloading;
                        item.started_at = Instant::now();
                    }
                    item.files_total = *files_count;
                }
                DownloadStatus::Starting => {}
                DownloadStatus::Downloading => {
                    if let Some(fp) = self.active_files.get(&file_key) {
                        let delta =
                            progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                        item.bytes_downloaded += delta;
                    }
                }
                DownloadStatus::Complete => {
                    item.files_completed += 1;
                    if let Some(fp) = self.active_files.get(&file_key) {
                        let delta =
                            progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                        item.bytes_downloaded += delta;
                    } else {
                        item.bytes_downloaded += progress.bytes_downloaded;
                    }
                }
                DownloadStatus::Skipped(_) => {
                    item.files_skipped += 1;
                }
                DownloadStatus::Failed(_) => {
                    item.files_failed += 1;
                }
                DownloadStatus::Verifying => {}
            }
        }

        // Update global state
        match &progress.status {
            DownloadStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                self.files_total += files_count;
                self.bytes_total += bytes_total;
            }
            DownloadStatus::Starting => {
                self.active_files.insert(
                    file_key,
                    FileProgress {
                        name: progress.file_name.clone(),
                        bytes_downloaded: progress.bytes_downloaded,
                        total_bytes: progress.total_bytes,
                        started_at: Instant::now(),
                    },
                );
            }
            DownloadStatus::Downloading => {
                if let Some(fp) = self.active_files.get_mut(&file_key) {
                    let delta = progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                    self.bytes_downloaded += delta;
                    fp.bytes_downloaded = progress.bytes_downloaded;
                }
            }
            DownloadStatus::Complete => {
                if let Some(fp) = self.active_files.remove(&file_key) {
                    let delta = progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                    self.bytes_downloaded += delta;
                } else {
                    // File wasn't in active_files (e.g. Starting event missed) —
                    // count the full bytes so the total stays accurate.
                    self.bytes_downloaded += progress.bytes_downloaded;
                }
                self.files_completed += 1;
                self.completed_files.push(progress.file_name);
            }
            DownloadStatus::Skipped(_) => {
                self.active_files.remove(&file_key);
                self.files_skipped += 1;
            }
            DownloadStatus::Failed(msg) => {
                self.active_files.remove(&file_key);
                self.files_failed += 1;
                self.failed_files.push((progress.file_name, msg.clone()));
            }
            DownloadStatus::Verifying => {}
        }

        // Sample throughput every second
        self.throughput.set_bytes(self.bytes_downloaded);
        self.throughput.maybe_sample();
    }

    /// Number of items that have finished successfully.
    pub fn items_completed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Complete))
            .count()
    }

    /// Number of items that have failed.
    pub fn items_failed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Failed(_)))
            .count()
    }

    /// Number of items currently downloading.
    #[cfg(test)]
    pub fn items_in_progress(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Downloading))
            .count()
    }
}

/// Wrapper that implements [`Dashboard`] for the download TUI, delegating
/// rendering to `ui::draw` and key handling to the existing j/k/q logic.
struct DownloadDashboard {
    state: Arc<Mutex<TuiState>>,
}

impl Dashboard for DownloadDashboard {
    fn draw(&self, frame: &mut ratatui::Frame) {
        let s = self.state.lock().unwrap();
        ui::draw(frame, &s);
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut s = self.state.lock().unwrap();
        match code {
            KeyCode::Char('q') => {
                s.quit_requested = true;
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                s.quit_requested = true;
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                s.scroll_offset = s.scroll_offset.saturating_add(1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ia_core::download::{DownloadProgress, DownloadStatus};

    fn progress(id: &str, file: &str, status: DownloadStatus) -> DownloadProgress {
        DownloadProgress {
            identifier: id.to_string(),
            file_name: file.to_string(),
            bytes_downloaded: 0,
            total_bytes: None,
            status,
        }
    }

    fn progress_bytes(
        id: &str,
        file: &str,
        bytes: u64,
        total: Option<u64>,
        status: DownloadStatus,
    ) -> DownloadProgress {
        DownloadProgress {
            identifier: id.to_string(),
            file_name: file.to_string(),
            bytes_downloaded: bytes,
            total_bytes: total,
            status,
        }
    }

    // --- Single-item tests ---

    #[test]
    fn single_item_progress_starts_at_zero() {
        let state = TuiState::new(&["item-a".to_string()]);
        assert_eq!(state.overall_progress(), 0.0);
    }

    #[test]
    fn single_item_progress_zero_after_enumeration() {
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 10,
                bytes_total: 1000,
            },
        ));
        // Item is Downloading but 0 files done
        assert_eq!(state.overall_progress(), 0.0);
    }

    #[test]
    fn single_item_progress_increases_with_completions() {
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 4,
                bytes_total: 400,
            },
        ));

        // Complete 1 file
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            100,
            Some(100),
            DownloadStatus::Starting,
        ));
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            100,
            Some(100),
            DownloadStatus::Complete,
        ));
        assert!(
            (state.overall_progress() - 0.25).abs() < 0.001,
            "expected ~0.25, got {}",
            state.overall_progress()
        );

        // Skip 1 file
        state.update(progress(
            "item-a",
            "f2.txt",
            DownloadStatus::Skipped("match".to_string()),
        ));
        assert!(
            (state.overall_progress() - 0.5).abs() < 0.001,
            "expected ~0.5, got {}",
            state.overall_progress()
        );
    }

    #[test]
    fn single_item_all_skipped_reaches_100() {
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 3,
                bytes_total: 300,
            },
        ));
        for i in 0..3 {
            state.update(progress(
                "item-a",
                &format!("f{i}.txt"),
                DownloadStatus::Skipped("match".to_string()),
            ));
        }
        assert!(
            (state.overall_progress() - 1.0).abs() < 0.001,
            "expected 1.0, got {}",
            state.overall_progress()
        );
    }

    // --- Batch tests ---

    #[test]
    fn batch_pending_items_keep_progress_low() {
        // This is the core bug: with 100 items, if only item-0 is enumerated
        // and all its files skip, progress should be ~1%, not 100%.
        let ids: Vec<String> = (0..100).map(|i| format!("item-{i}")).collect();
        let mut state = TuiState::new(&ids);

        // Enumerate and complete all files for item-0
        state.update(progress(
            "item-0",
            "",
            DownloadStatus::Enumerated {
                files_count: 5,
                bytes_total: 500,
            },
        ));
        for i in 0..5 {
            state.update(progress(
                "item-0",
                &format!("f{i}.txt"),
                DownloadStatus::Skipped("match".to_string()),
            ));
        }

        // item-0 is fully done (1.0), 99 items are pending (0.0 each)
        // Overall = 1.0 / 100 = 0.01
        let p = state.overall_progress();
        assert!(
            (p - 0.01).abs() < 0.001,
            "expected ~0.01, got {p} — pending items must count in denominator"
        );
    }

    #[test]
    fn batch_progress_monotonically_increases() {
        let ids: Vec<String> = (0..10).map(|i| format!("item-{i}")).collect();
        let mut state = TuiState::new(&ids);

        let mut prev = 0.0;
        for i in 0..10 {
            let id = format!("item-{i}");

            state.update(progress(
                &id,
                "",
                DownloadStatus::Enumerated {
                    files_count: 2,
                    bytes_total: 200,
                },
            ));

            let p = state.overall_progress();
            assert!(
                p >= prev,
                "progress decreased from {prev} to {p} at enumeration of item {i}"
            );
            prev = p;

            // Complete first file
            state.update(progress_bytes(
                &id,
                &format!("a{i}.txt"),
                100,
                Some(100),
                DownloadStatus::Starting,
            ));
            state.update(progress_bytes(
                &id,
                &format!("a{i}.txt"),
                100,
                Some(100),
                DownloadStatus::Complete,
            ));

            let p = state.overall_progress();
            assert!(
                p >= prev,
                "progress decreased from {prev} to {p} after first file of item {i}"
            );
            prev = p;

            // Skip second file
            state.update(progress(
                &id,
                &format!("b{i}.txt"),
                DownloadStatus::Skipped("match".to_string()),
            ));

            let p = state.overall_progress();
            assert!(
                p >= prev,
                "progress decreased from {prev} to {p} after second file of item {i}"
            );
            prev = p;
        }

        assert!(
            (prev - 1.0).abs() < 0.001,
            "expected 1.0 at end, got {prev}"
        );
    }

    #[test]
    fn batch_interleaved_enumeration_and_skips() {
        // Simulate batch where item 1 enumerates and skips before item 2 enumerates
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut state = TuiState::new(&ids);

        // item "a" enumerates 2 files, both skip
        state.update(progress(
            "a",
            "",
            DownloadStatus::Enumerated {
                files_count: 2,
                bytes_total: 200,
            },
        ));
        state.update(progress(
            "a",
            "a1.txt",
            DownloadStatus::Skipped("match".to_string()),
        ));
        state.update(progress(
            "a",
            "a2.txt",
            DownloadStatus::Skipped("match".to_string()),
        ));

        // At this point: item "a" is 2/2 = 1.0, "b" and "c" are 0.0
        // Progress = 1.0 / 3 = 0.333
        let p = state.overall_progress();
        assert!(
            (p - 1.0 / 3.0).abs() < 0.01,
            "expected ~0.333, got {p}"
        );

        // Now item "b" enumerates
        state.update(progress(
            "b",
            "",
            DownloadStatus::Enumerated {
                files_count: 4,
                bytes_total: 400,
            },
        ));
        state.update(progress_bytes(
            "b",
            "b1.txt",
            50,
            Some(100),
            DownloadStatus::Starting,
        ));
        state.update(progress_bytes(
            "b",
            "b1.txt",
            100,
            Some(100),
            DownloadStatus::Complete,
        ));

        // item "a" = 1.0, item "b" = 1/4 = 0.25, item "c" = 0.0
        // Progress = (1.0 + 0.25 + 0.0) / 3 = 0.4167
        let p = state.overall_progress();
        assert!(
            (p - (1.0 + 0.25) / 3.0).abs() < 0.01,
            "expected ~0.417, got {p}"
        );
    }

    // --- Item counter tests ---

    #[test]
    fn items_completed_counts_correctly() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut state = TuiState::new(&ids);

        assert_eq!(state.items_completed(), 0);
        assert_eq!(state.items_failed(), 0);
        assert_eq!(state.items_in_progress(), 0);

        // Enumerate item "a" (transitions Pending → Downloading)
        state.update(progress(
            "a",
            "",
            DownloadStatus::Enumerated {
                files_count: 1,
                bytes_total: 100,
            },
        ));
        assert_eq!(state.items_in_progress(), 1);
        assert_eq!(state.items_completed(), 0);

        // Manually mark "a" complete (as run_tui does)
        state
            .items
            .iter_mut()
            .find(|i| i.identifier == "a")
            .unwrap()
            .status = ItemStatus::Complete;
        assert_eq!(state.items_completed(), 1);
        assert_eq!(state.items_in_progress(), 0);

        // Mark "b" as failed
        state
            .items
            .iter_mut()
            .find(|i| i.identifier == "b")
            .unwrap()
            .status = ItemStatus::Failed("error".to_string());
        assert_eq!(state.items_completed(), 1);
        assert_eq!(state.items_failed(), 1);
    }

    // --- Bytes tracking tests ---

    #[test]
    fn bytes_downloaded_tracks_actual_http_bytes() {
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 2,
                bytes_total: 200,
            },
        ));

        // Download a file: 100 bytes
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            0,
            Some(100),
            DownloadStatus::Starting,
        ));
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            50,
            Some(100),
            DownloadStatus::Downloading,
        ));
        assert_eq!(state.bytes_downloaded, 50);

        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            100,
            Some(100),
            DownloadStatus::Complete,
        ));
        assert_eq!(state.bytes_downloaded, 100);

        // Skip a file: should NOT add to bytes_downloaded
        state.update(progress(
            "item-a",
            "f2.txt",
            DownloadStatus::Skipped("match".to_string()),
        ));
        assert_eq!(
            state.bytes_downloaded, 100,
            "skipped files must not inflate bytes_downloaded"
        );
    }

    #[test]
    fn bytes_tracked_even_without_starting_event() {
        // If a Complete event arrives without a prior Starting event (edge
        // case), bytes should still be counted via the fallback path.
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 1,
                bytes_total: 500,
            },
        ));

        // Complete without Starting — the else branch should count bytes
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            500,
            Some(500),
            DownloadStatus::Complete,
        ));
        assert_eq!(
            state.bytes_downloaded, 500,
            "bytes must be tracked even without Starting event"
        );
        // Per-item bytes should also be tracked
        assert_eq!(state.items[0].bytes_downloaded, 500);
    }

    #[test]
    fn resumed_download_starting_bytes_preserved() {
        // When resuming, the Starting event carries the already-downloaded
        // byte count.  The TUI should store that in active_files so the
        // delta on Complete is correct.
        let mut state = TuiState::new(&["item-a".to_string()]);
        state.update(progress(
            "item-a",
            "",
            DownloadStatus::Enumerated {
                files_count: 1,
                bytes_total: 1000,
            },
        ));

        // Starting with 400 bytes already on disk (resume)
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            400,
            Some(1000),
            DownloadStatus::Starting,
        ));

        // Complete with total 1000 bytes
        state.update(progress_bytes(
            "item-a",
            "f1.txt",
            1000,
            Some(1000),
            DownloadStatus::Complete,
        ));

        // Only the new bytes (600) should be counted, not the full 1000
        assert_eq!(
            state.bytes_downloaded, 600,
            "resumed downloads should only count new bytes"
        );
    }
}

/// Run the TUI download interface for one or more items.
pub async fn run_tui(
    client: &IaClient,
    identifiers: &[String],
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    items_concurrency: usize,
) -> anyhow::Result<()> {
    // Set up terminal — the guard ensures cleanup even on panic.
    // `setup_terminal` also checks for an interactive TTY.
    let (mut terminal, _guard) = framework::setup_terminal()?;

    let state = Arc::new(Mutex::new(TuiState::new(identifiers)));

    // Semaphore to limit how many items download concurrently
    let item_sem = Arc::new(Semaphore::new(items_concurrency));

    // Spawn download tasks for each item
    let mut handles = Vec::new();
    for identifier in identifiers {
        let client = client.clone();
        let id = identifier.clone();
        let opts = opts.clone();
        let sem = Arc::clone(&semaphore);
        let isem = Arc::clone(&item_sem);
        let download_state = state.clone();

        let handle = tokio::spawn(async move {
            // Wait for an item slot before starting this item's download
            let _item_permit = isem.acquire().await.unwrap();

            let progress_state = download_state.clone();
            let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
                Some(Arc::new(move |p: DownloadProgress| {
                    if let Ok(mut s) = progress_state.lock() {
                        s.update(p);
                    }
                }));

            let result =
                ia_core::download::download_item(&client, &id, &opts, sem, progress).await;

            if let Ok(mut s) = download_state.lock() {
                // Clean up any leaked active_files for this item (e.g. files
                // that failed all retries without sending a Complete event).
                s.active_files.retain(|k, _| !k.starts_with(&format!("{id}\0")));

                // Mark item as complete or failed
                if let Some(item) = s.items.iter_mut().find(|i| i.identifier == id) {
                    match &result {
                        Ok(_) => item.status = ItemStatus::Complete,
                        Err(e) => item.status = ItemStatus::Failed(e.to_string()),
                    }
                }
                // Check if all items done
                if s.items.iter().all(|i| {
                    matches!(i.status, ItemStatus::Complete | ItemStatus::Failed(_))
                }) {
                    s.done = true;
                }
            }

            result
        });
        handles.push(handle);
    }

    // Run the dashboard event loop on a blocking thread so the async
    // download tasks keep running on the tokio runtime.
    let dashboard_state = Arc::clone(&state);
    let tick_rate = Duration::from_millis(100);
    tokio::task::spawn_blocking(move || {
        let mut dashboard = DownloadDashboard {
            state: dashboard_state,
        };
        framework::run_dashboard_sync(&mut terminal, &mut dashboard, tick_rate)
    })
    .await??;

    // Guard handles terminal cleanup (disable_raw_mode + LeaveAlternateScreen) on drop.
    drop(_guard);

    // Wait for all downloads to finish and collect results
    let mut total_downloaded = 0usize;
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut total_failed = 0usize;

    for handle in handles {
        match handle.await? {
            Ok(result) => {
                total_downloaded += result.files_downloaded;
                total_files += result.files_total;
                total_bytes += result.bytes_total;
                total_failed += result.files_failed;
            }
            Err(e) => {
                eprintln!("Error: {e}");
                total_failed += 1;
            }
        }
    }

    let elapsed = state.lock().unwrap().elapsed();
    eprintln!(
        "Downloaded {}/{} files ({} bytes) in {:.1}s",
        total_downloaded,
        total_files,
        total_bytes,
        elapsed.as_secs_f64(),
    );

    if total_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
