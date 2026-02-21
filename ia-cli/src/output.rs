use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};

pub struct DownloadDisplay {
    identifier: String,
    multi: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    header: ProgressBar,
    separator: ProgressBar,
}

impl DownloadDisplay {
    pub fn new(identifier: &str, multi: &MultiProgress) -> Self {
        let header = multi.add(ProgressBar::new_spinner());
        header.set_message(format!(
            "{}  Resolving...",
            style(identifier).bold()
        ));
        header.enable_steady_tick(std::time::Duration::from_millis(100));

        let separator = multi.add(ProgressBar::new_spinner());
        separator.set_style(ProgressStyle::with_template("{msg}").unwrap());
        separator.set_message(
            style("────────────────────────────────────────────────────")
                .dim()
                .to_string(),
        );

        Self {
            identifier: identifier.to_string(),
            multi: multi.clone(),
            bars: Mutex::new(HashMap::new()),
            header,
            separator,
        }
    }

    pub fn update(&self, progress: DownloadProgress) {
        let mut bars = self.bars.lock().unwrap();

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = bars.entry(progress.file_name.clone()).or_insert_with(|| {
                    let pb = self.multi.insert_before(
                        &self.separator,
                        ProgressBar::new(progress.total_bytes.unwrap_or(0)),
                    );
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

    pub fn finish(&self, result: &ItemDownloadResult, destdir: &Path) {
        self.header.finish_and_clear();

        let elapsed = result.elapsed.as_secs_f64();
        let speed = if elapsed > 0.0 {
            format!(
                " · {}/s",
                format_bytes((result.bytes_total as f64 / elapsed) as u64)
            )
        } else {
            String::new()
        };

        let summary = format!(
            "{}  {} files ({}) in {:.1}s{}",
            style(&self.identifier).bold(),
            result.files_downloaded,
            format_bytes(result.bytes_total),
            elapsed,
            style(&speed).dim(),
        );

        let stats = format!(
            "  {} {} downloaded · {} skipped · {} errors",
            style("✓").green(),
            result.files_downloaded,
            if result.files_skipped > 0 {
                style(result.files_skipped.to_string())
                    .yellow()
                    .to_string()
            } else {
                "0".to_string()
            },
            if result.files_failed > 0 {
                style(result.files_failed.to_string()).red().to_string()
            } else {
                "0".to_string()
            },
        );

        eprintln!("{summary}");
        eprintln!("{stats}");

        // Disk space
        if let Some(free) = disk_space_free(destdir) {
            eprintln!(
                "  {}: {} free",
                style(destdir.display()).dim(),
                format_bytes(free)
            );
        }
    }
}

pub struct BatchDisplay {
    multi: MultiProgress,
    active_item_bars: Mutex<HashMap<String, ItemBars>>,
    #[allow(dead_code)]
    started_at: std::time::Instant,
}

struct ItemBars {
    header: ProgressBar,
    file_bars: HashMap<String, ProgressBar>,
    sentinel: ProgressBar,
}

impl BatchDisplay {
    pub fn new(items_total: usize, jobs: usize) -> Self {
        let multi = MultiProgress::new();

        let header = multi.add(ProgressBar::new_spinner());
        header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        header.set_message(format!(
            "Downloading {} items ({} workers)...",
            style(items_total).bold(),
            jobs,
        ));

        let sep = multi.add(ProgressBar::new_spinner());
        sep.set_style(ProgressStyle::with_template("{msg}").unwrap());
        sep.set_message(
            style("────────────────────────────────────────────────────")
                .dim()
                .to_string(),
        );

        Self {
            multi,
            active_item_bars: Mutex::new(HashMap::new()),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn on_item_start(&self, identifier: &str, _current: usize, _total: usize) {
        let item_header = self.multi.add(ProgressBar::new_spinner());
        item_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        item_header.set_message(format!(
            "{} {}",
            style("▸").cyan(),
            style(identifier).bold(),
        ));

        let sentinel = self.multi.add(ProgressBar::new_spinner());
        sentinel.set_style(ProgressStyle::with_template("{msg}").unwrap());
        sentinel.set_message("");
        sentinel.finish();

        let mut items = self.active_item_bars.lock().unwrap();
        items.insert(
            identifier.to_string(),
            ItemBars {
                header: item_header,
                file_bars: HashMap::new(),
                sentinel,
            },
        );
    }

    pub fn on_progress(&self, progress: DownloadProgress) {
        let identifier = &progress.identifier;
        let mut items = self.active_item_bars.lock().unwrap();
        let Some(item) = items.get_mut(identifier) else {
            return;
        };

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = item
                    .file_bars
                    .entry(progress.file_name.clone())
                    .or_insert_with(|| {
                        let pb = self.multi.insert_before(
                            &item.sentinel,
                            ProgressBar::new(progress.total_bytes.unwrap_or(0)),
                        );
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
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_and_clear();
                }
            }
            DownloadStatus::Skipped(_) => {
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_and_clear();
                }
            }
            DownloadStatus::Failed(err) => {
                if let Some(bar) = item.file_bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {} {}",
                        style("✗").red(),
                        progress.file_name,
                        style(err).red(),
                    ));
                }
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn on_item_complete(&self, result: &ia_core::download::ItemDownloadResult) {
        let identifier = &result.identifier;
        let mut items = self.active_item_bars.lock().unwrap();
        if let Some(item) = items.remove(identifier) {
            for (_, bar) in item.file_bars {
                bar.finish_and_clear();
            }
            item.sentinel.finish_and_clear();

            let elapsed = result.elapsed.as_secs_f64();
            let speed = if elapsed > 0.0 {
                format!(
                    "  {}/s",
                    format_bytes((result.bytes_total as f64 / elapsed) as u64)
                )
            } else {
                String::new()
            };

            let skipped_info = if result.files_skipped > 0 {
                format!(
                    "\n  {} {} skipped",
                    style("─").dim(),
                    style(result.files_skipped).yellow()
                )
            } else {
                String::new()
            };

            item.header.set_message(format!(
                "{} {}       {} files ({}) {:.0}s{}{}",
                style("✓").green(),
                style(identifier).bold(),
                result.files_downloaded,
                format_bytes(result.bytes_total),
                elapsed,
                style(&speed).dim(),
                skipped_info,
            ));
            item.header.finish();
        }
    }

    pub fn finish(
        &self,
        result: &ia_core::download::BatchDownloadResult,
        disk_statuses: Option<&[ia_core::disk_pool::DiskStatus]>,
    ) {
        let elapsed = result.elapsed.as_secs_f64();
        let speed = if elapsed > 0.0 {
            format!(
                " · {}/s",
                format_bytes((result.bytes_total as f64 / elapsed) as u64)
            )
        } else {
            String::new()
        };

        eprintln!(
            "{}",
            style("────────────────────────────────────────────────────").dim()
        );
        eprintln!(
            "{}/{} items ({} done · {} skipped · {} errors)",
            result.items_succeeded,
            result.items_total,
            style(result.items_succeeded).green(),
            if result.files_skipped > 0 {
                style(result.files_skipped.to_string())
                    .yellow()
                    .to_string()
            } else {
                "0".to_string()
            },
            if result.files_failed > 0 || result.items_failed > 0 {
                style((result.files_failed + result.items_failed).to_string())
                    .red()
                    .to_string()
            } else {
                "0".to_string()
            },
        );
        eprintln!(
            "{} downloaded{}",
            format_bytes(result.bytes_total),
            style(&speed).dim(),
        );

        if let Some(statuses) = disk_statuses {
            let disk_line: Vec<String> = statuses
                .iter()
                .map(|ds| {
                    format!(
                        "{}: {} free",
                        style(ds.path.display()).dim(),
                        format_bytes(ds.free_bytes)
                    )
                })
                .collect();
            eprintln!("{}", disk_line.join(" │ "));
        }

        eprintln!("{:.1}s elapsed", elapsed);
    }
}

/// Get free disk space for a path (bytes).
pub fn disk_space_free(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let c_path = std::ffi::CString::new(path.to_str()?).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            #[allow(clippy::unnecessary_cast)]
            Some(stat.f_bavail as u64 * stat.f_frsize as u64)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

pub fn format_bytes(bytes: u64) -> String {
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
