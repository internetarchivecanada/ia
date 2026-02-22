use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus};
use ia_core::IaClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// State of a single download job.
#[derive(Debug, Clone)]
pub struct DownloadState {
    pub identifier: String,
    pub status: DownloadJobStatus,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadJobStatus {
    Queued,
    Downloading,
    Complete,
    Failed,
}

/// Manages download jobs with concurrency control and progress tracking.
pub struct DownloadManager {
    client: Arc<IaClient>,
    runtime: tokio::runtime::Handle,
    semaphore: Arc<Semaphore>,
    downloads: Arc<Mutex<HashMap<String, DownloadState>>>,
    dest_dir: PathBuf,
}

impl DownloadManager {
    pub fn new(
        client: Arc<IaClient>,
        runtime: tokio::runtime::Handle,
        concurrency: usize,
        dest_dir: PathBuf,
    ) -> Self {
        Self {
            client,
            runtime,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            downloads: Arc::new(Mutex::new(HashMap::new())),
            dest_dir,
        }
    }

    /// Queue a download for the given identifier.
    pub fn queue_download(&self, identifier: &str) {
        {
            let mut downloads = self.downloads.lock().unwrap();
            downloads.insert(
                identifier.to_string(),
                DownloadState {
                    identifier: identifier.to_string(),
                    status: DownloadJobStatus::Queued,
                    files_total: 0,
                    files_completed: 0,
                    files_skipped: 0,
                    files_failed: 0,
                    bytes_downloaded: 0,
                    bytes_total: 0,
                    error: None,
                },
            );
        }

        let client = Arc::clone(&self.client);
        let semaphore = Arc::clone(&self.semaphore);
        let downloads = Arc::clone(&self.downloads);
        let identifier = identifier.to_string();
        let dest_dir = self.dest_dir.clone();

        self.runtime.spawn(async move {
            // Update status to Downloading
            {
                let mut dl = downloads.lock().unwrap();
                if let Some(state) = dl.get_mut(&identifier) {
                    state.status = DownloadJobStatus::Downloading;
                }
            }

            let progress_downloads = Arc::clone(&downloads);
            let progress_id = identifier.clone();
            let progress_cb: Arc<dyn Fn(DownloadProgress) + Send + Sync> =
                Arc::new(move |p: DownloadProgress| {
                    let mut dl = progress_downloads.lock().unwrap();
                    if let Some(state) = dl.get_mut(&progress_id) {
                        match &p.status {
                            DownloadStatus::Enumerated {
                                files_count,
                                bytes_total,
                            } => {
                                state.files_total = *files_count;
                                state.bytes_total = *bytes_total;
                            }
                            DownloadStatus::Downloading => {
                                state.bytes_downloaded += p.bytes_downloaded.saturating_sub(
                                    // approximate: we track cumulative, callback gives per-file
                                    0,
                                );
                            }
                            DownloadStatus::Complete => {
                                state.files_completed += 1;
                                state.bytes_downloaded =
                                    state.bytes_downloaded.max(p.bytes_downloaded);
                            }
                            DownloadStatus::Skipped(_) => {
                                state.files_skipped += 1;
                            }
                            DownloadStatus::Failed(err) => {
                                state.files_failed += 1;
                                state.error = Some(err.clone());
                            }
                            _ => {}
                        }
                    }
                });

            let opts = DownloadOpts {
                destdir: dest_dir.clone(),
                ..Default::default()
            };

            let result = ia_core::download::download_item(
                &client,
                &identifier,
                &opts,
                semaphore,
                Some(progress_cb),
            )
            .await;

            // Update final status
            let mut dl = downloads.lock().unwrap();
            if let Some(state) = dl.get_mut(&identifier) {
                match result {
                    Ok(item_result) => {
                        state.files_completed = item_result.files_downloaded;
                        state.files_skipped = item_result.files_skipped;
                        state.files_failed = item_result.files_failed;
                        if item_result.files_failed > 0 {
                            state.status = DownloadJobStatus::Failed;
                        } else {
                            state.status = DownloadJobStatus::Complete;
                        }
                    }
                    Err(e) => {
                        state.status = DownloadJobStatus::Failed;
                        state.error = Some(e.to_string());
                    }
                }
            }
        });
    }

    /// Get a snapshot of all download states.
    pub fn get_downloads(&self) -> Vec<DownloadState> {
        let dl = self.downloads.lock().unwrap();
        dl.values().cloned().collect()
    }

    /// Get the state for a specific download.
    #[allow(dead_code)]
    pub fn get_download(&self, identifier: &str) -> Option<DownloadState> {
        let dl = self.downloads.lock().unwrap();
        dl.get(identifier).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_state_defaults() {
        let state = DownloadState {
            identifier: "test".into(),
            status: DownloadJobStatus::Queued,
            files_total: 0,
            files_completed: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_downloaded: 0,
            bytes_total: 0,
            error: None,
        };
        assert_eq!(state.status, DownloadJobStatus::Queued);
        assert_eq!(state.identifier, "test");
    }

    #[test]
    fn test_download_job_status_eq() {
        assert_eq!(DownloadJobStatus::Queued, DownloadJobStatus::Queued);
        assert_eq!(
            DownloadJobStatus::Downloading,
            DownloadJobStatus::Downloading
        );
        assert_eq!(DownloadJobStatus::Complete, DownloadJobStatus::Complete);
        assert_eq!(DownloadJobStatus::Failed, DownloadJobStatus::Failed);
        assert_ne!(DownloadJobStatus::Queued, DownloadJobStatus::Complete);
    }
}
