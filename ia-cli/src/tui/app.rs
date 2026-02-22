use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use tokio::sync::Semaphore;

use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus};
use ia_core::IaClient;

use super::ui;

/// Guard that restores the terminal on drop (even during panic).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
    }
}

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
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub active_files: HashMap<String, FileProgress>,
    pub completed_files: Vec<String>,
    pub failed_files: Vec<(String, String)>,
    pub started_at: Instant,
    pub throughput_history: Vec<f64>,
    pub last_throughput_sample: Instant,
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

        Self {
            identifier,
            items,
            files_total: 0,
            files_completed: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_downloaded: 0,
            bytes_total: 0,
            active_files: HashMap::new(),
            completed_files: Vec::new(),
            failed_files: Vec::new(),
            started_at: Instant::now(),
            throughput_history: Vec::new(),
            last_throughput_sample: Instant::now(),
            disk_statuses: Vec::new(),
            paused: false,
            scroll_offset: 0,
            quit_requested: false,
            done: false,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn throughput(&self) -> f64 {
        let secs = self.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.bytes_downloaded as f64 / secs
        } else {
            0.0
        }
    }

    pub fn eta_seconds(&self) -> Option<f64> {
        let speed = self.throughput();
        if speed > 0.0 && self.bytes_total > self.bytes_downloaded {
            Some((self.bytes_total - self.bytes_downloaded) as f64 / speed)
        } else {
            None
        }
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

    fn update(&mut self, progress: DownloadProgress) {
        // Update per-item state
        let item_id = progress.identifier.clone();
        if let Some(item) = self.items.iter_mut().find(|i| i.identifier == item_id) {
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
                    // bytes delta handled below at global level
                }
                DownloadStatus::Complete => {
                    item.files_completed += 1;
                    let delta = if let Some(fp) = self.active_files.get(&progress.file_name) {
                        progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded)
                    } else {
                        0
                    };
                    item.bytes_downloaded += delta;
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
                // Set totals upfront so the progress bar has a stable denominator
                self.files_total += files_count;
                self.bytes_total += bytes_total;
            }
            DownloadStatus::Starting => {
                self.active_files.insert(
                    progress.file_name.clone(),
                    FileProgress {
                        name: progress.file_name.clone(),
                        bytes_downloaded: 0,
                        total_bytes: progress.total_bytes,
                        started_at: Instant::now(),
                    },
                );
                // bytes_total and files_total already set by Enumerated
            }
            DownloadStatus::Downloading => {
                if let Some(fp) = self.active_files.get_mut(&progress.file_name) {
                    let delta = progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                    self.bytes_downloaded += delta;
                    // Also update item bytes
                    if let Some(item) =
                        self.items.iter_mut().find(|i| i.identifier == item_id)
                    {
                        item.bytes_downloaded += delta;
                    }
                    fp.bytes_downloaded = progress.bytes_downloaded;
                }
            }
            DownloadStatus::Complete => {
                if let Some(fp) = self.active_files.remove(&progress.file_name) {
                    let delta = progress.bytes_downloaded.saturating_sub(fp.bytes_downloaded);
                    self.bytes_downloaded += delta;
                }
                self.files_completed += 1;
                self.completed_files.push(progress.file_name);
            }
            DownloadStatus::Skipped(_) => {
                self.active_files.remove(&progress.file_name);
                self.files_skipped += 1;
            }
            DownloadStatus::Failed(msg) => {
                self.active_files.remove(&progress.file_name);
                self.files_failed += 1;
                self.failed_files.push((progress.file_name, msg.clone()));
            }
            DownloadStatus::Verifying => {}
        }

        // Sample throughput every second
        if self.last_throughput_sample.elapsed() >= Duration::from_secs(1) {
            self.throughput_history.push(self.throughput());
            if self.throughput_history.len() > 60 {
                self.throughput_history.remove(0);
            }
            self.last_throughput_sample = Instant::now();
        }
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
}

/// Run the TUI download interface for one or more items.
pub async fn run_tui(
    client: &IaClient,
    identifiers: &[String],
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
    items_concurrency: usize,
) -> anyhow::Result<()> {
    // Check if stdout is a TTY — raw mode requires an interactive terminal
    if !atty::is(atty::Stream::Stdout) {
        anyhow::bail!(
            "Dashboard mode requires an interactive terminal.\n\
             Hint: remove --dashboard when piping output or running without a TTY."
        );
    }

    let state = Arc::new(Mutex::new(TuiState::new(identifiers)));

    // Set up terminal — the guard ensures cleanup even on panic
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

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

    // Main UI loop
    let tick_rate = Duration::from_millis(100);
    let mut input_disabled = false;

    loop {
        // Draw
        {
            let s = state.lock().unwrap();
            terminal.draw(|f| ui::draw(f, &s))?;

            if s.quit_requested || (s.done && s.active_files.is_empty()) {
                break;
            }
        }

        // Handle input — if event system fails, continue without input
        if !input_disabled {
            match event::poll(tick_rate) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => {
                        let mut s = state.lock().unwrap();
                        match key.code {
                            KeyCode::Char('q') => {
                                s.quit_requested = true;
                            }
                            KeyCode::Char('c')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                s.quit_requested = true;
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                s.scroll_offset = s.scroll_offset.saturating_add(1);
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                s.scroll_offset = s.scroll_offset.saturating_sub(1);
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        input_disabled = true;
                    }
                },
                Ok(false) => {}
                Err(_) => {
                    // Input reader failed — continue rendering without input.
                    // Downloads will run to completion; the TUI exits automatically.
                    input_disabled = true;
                    tokio::time::sleep(tick_rate).await;
                }
            }
        } else {
            tokio::time::sleep(tick_rate).await;
        }
    }

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
