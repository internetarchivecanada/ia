use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use tokio::sync::Semaphore;

use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus};
use ia_core::IaClient;

use super::ui;

/// Shared state for TUI rendering.
#[derive(Debug, Clone)]
pub struct TuiState {
    pub identifier: String,
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
    pub fn new(identifier: &str) -> Self {
        Self {
            identifier: identifier.to_string(),
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
                self.failed_files
                    .push((progress.file_name, msg.clone()));
            }
            DownloadStatus::Verifying => {}
        }
    }
}

/// Run the TUI download interface.
pub async fn run_tui(
    client: &IaClient,
    identifier: &str,
    opts: &DownloadOpts,
    semaphore: Arc<Semaphore>,
) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(TuiState::new(identifier)));

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn download task
    let download_state = state.clone();
    let client = client.clone();
    let identifier = identifier.to_string();
    let opts = opts.clone();

    let download_handle = tokio::spawn(async move {
        let progress_state = download_state.clone();
        let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
            Some(Arc::new(move |p: DownloadProgress| {
                if let Ok(mut s) = progress_state.lock() {
                    s.update(p);
                }
            }));

        let result = ia_core::download::download_item(&client, &identifier, &opts, semaphore, progress).await;

        if let Ok(mut s) = download_state.lock() {
            s.done = true;
        }

        result
    });

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

    // Wait for download to finish and report result
    let result = download_handle.await??;
    eprintln!(
        "Downloaded {}/{} files ({} bytes) in {:.1}s",
        result.files_downloaded,
        result.files_total,
        result.bytes_total,
        result.elapsed.as_secs_f64(),
    );

    if result.files_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
