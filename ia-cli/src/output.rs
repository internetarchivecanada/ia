use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Mutex;

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};

pub struct DownloadDisplay {
    identifier: String,
    multi: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    header: ProgressBar,
}

impl DownloadDisplay {
    pub fn new(identifier: &str) -> Self {
        let multi = MultiProgress::new();
        let header = multi.add(ProgressBar::new_spinner());
        header.set_message(format!(
            "{}  Resolving...",
            style(identifier).bold()
        ));
        header.enable_steady_tick(std::time::Duration::from_millis(100));

        Self {
            identifier: identifier.to_string(),
            multi,
            bars: Mutex::new(HashMap::new()),
            header,
        }
    }

    pub fn update(&self, progress: DownloadProgress) {
        let mut bars = self.bars.lock().unwrap();

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = bars.entry(progress.file_name.clone()).or_insert_with(|| {
                    let pb = self.multi.add(ProgressBar::new(
                        progress.total_bytes.unwrap_or(0),
                    ));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  {prefix:.dim} {bar:20.cyan/dim} {bytes}/{total_bytes} {bytes_per_sec:.dim}")
                            .unwrap()
                            .progress_chars("━╸─"),
                    );
                    pb.set_prefix(progress.file_name.clone());
                    pb
                });
                bar.set_position(progress.bytes_downloaded);
            }
            DownloadStatus::Complete => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style("done").green()
                    ));
                }
            }
            DownloadStatus::Skipped(reason) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("skipped ({reason})")).yellow()
                    ));
                }
            }
            DownloadStatus::Failed(err) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("FAILED: {err}")).red()
                    ));
                }
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn finish(&self, result: &ItemDownloadResult) {
        self.header.finish_and_clear();

        let summary = format!(
            "\n{}  Downloaded {} files ({}) in {:.1}s",
            style(&self.identifier).bold(),
            result.files_downloaded,
            format_bytes(result.bytes_total),
            result.elapsed.as_secs_f64(),
        );

        let stats = format!(
            "  {} {} succeeded, {} failed, {} skipped",
            style("✓").green(),
            result.files_downloaded,
            if result.files_failed > 0 {
                style(result.files_failed.to_string()).red().to_string()
            } else {
                "0".to_string()
            },
            result.files_skipped,
        );

        eprintln!("{summary}");
        eprintln!("{stats}");
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
