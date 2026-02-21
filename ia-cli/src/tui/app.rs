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
        if self.bytes_total > 0 {
            self.bytes_downloaded as f64 / self.bytes_total as f64
        } else if self.files_total > 0 {
            (self.files_completed + self.files_skipped + self.files_failed) as f64
                / self.files_total as f64
        } else {
            0.0
        }
    }

    fn update(&mut self, progress: DownloadProgress) {
        // Update per-item state
        let item_id = progress.identifier.clone();
        if let Some(item) = self.items.iter_mut().find(|i| i.identifier == item_id) {
            match &progress.status {
                DownloadStatus::Starting => {
                    if item.status == ItemStatus::Pending {
                        item.status = ItemStatus::Downloading;
                        item.started_at = Instant::now();
                    }
                    item.files_total += 1;
                }
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
                if let Some(total) = progress.total_bytes {
                    self.bytes_total += total;
                }
                self.files_total += 1;
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
}

/// Run the TUI download interface for one or more items.
pub async fn run_tui(
    client: &IaClient,
    identifiers: &[String],
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(TuiState::new(identifiers)));

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn download tasks for each item
    let mut handles = Vec::new();
    for identifier in identifiers {
        let client = client.clone();
        let id = identifier.clone();
        let opts = opts.clone();
        let sem = Arc::clone(&semaphore);
        let download_state = state.clone();

        let handle = tokio::spawn(async move {
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

    loop {
        // Draw
        {
            let s = state.lock().unwrap();
            terminal.draw(|f| ui::draw(f, &s))?;

            if s.quit_requested || (s.done && s.active_files.is_empty()) {
                break;
            }
        }

        // Handle input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                let mut s = state.lock().unwrap();
                match key.code {
                    KeyCode::Char('q') => {
                        s.quit_requested = true;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

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
