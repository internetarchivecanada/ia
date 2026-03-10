use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};
use ia_core::upload::{UploadProgress, UploadProgressStatus};

/// Single-item download progress: one aggregate bar, no dynamic insertion.
///
/// Uses a single `ProgressBar` tracking total bytes across all files.
/// The header is printed once via `eprintln!`, errors are collected and
/// printed at the end to avoid any redraw/flicker issues.
pub struct DownloadDisplay {
    identifier: String,
    bar: ProgressBar,
    per_file_bytes: Mutex<HashMap<String, u64>>,
    files_done: Mutex<usize>,
    files_total: Mutex<usize>,
    errors: Mutex<Vec<String>>,
}

impl DownloadDisplay {
    pub fn new(identifier: &str) -> Self {
        eprintln!(
            "{} {}",
            style("▸").cyan(),
            style(identifier).bold(),
        );

        let bar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::with_template(
                "  {bar:40.cyan/dim} {bytes}/{total_bytes} {bytes_per_sec:.dim}  ({msg})",
            )
            .unwrap()
            .progress_chars("━╸─"),
        );
        bar.set_message("starting...");

        Self {
            identifier: identifier.to_string(),
            bar,
            per_file_bytes: Mutex::new(HashMap::new()),
            files_done: Mutex::new(0),
            files_total: Mutex::new(0),
            errors: Mutex::new(Vec::new()),
        }
    }

    pub fn update(&self, progress: DownloadProgress) {
        match &progress.status {
            DownloadStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                self.bar.set_length(*bytes_total);
                *self.files_total.lock().unwrap() = *files_count;
                self.bar.set_message(format!("0/{files_count} files"));
            }
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let mut map = self.per_file_bytes.lock().unwrap();
                map.insert(progress.file_name.clone(), progress.bytes_downloaded);
                let total: u64 = map.values().sum();
                self.bar.set_position(total);
            }
            DownloadStatus::Complete => {
                if let Some(size) = progress.total_bytes {
                    let mut map = self.per_file_bytes.lock().unwrap();
                    map.insert(progress.file_name.clone(), size);
                    let total: u64 = map.values().sum();
                    self.bar.set_position(total);
                }
                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{total} files"));
            }
            DownloadStatus::Skipped(_) => {
                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{total} files"));
            }
            DownloadStatus::Failed(err) => {
                self.errors.lock().unwrap().push(format!(
                    "  {} {} {}",
                    style("✗").red(),
                    style(&progress.file_name).dim(),
                    style(format!("— {err}")).red(),
                ));
                let mut done = self.files_done.lock().unwrap();
                *done += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{done}/{total} files"));
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn finish(&self, result: &ItemDownloadResult, destdir: &Path) {
        self.bar.finish_and_clear();

        // Print collected errors
        let errors = self.errors.lock().unwrap();
        for err in errors.iter() {
            eprintln!("{err}");
        }

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
            "{}  {} files ({}) in {:.1}s{}",
            style(&self.identifier).bold(),
            result.files_downloaded,
            format_bytes(result.bytes_total),
            elapsed,
            style(&speed).dim(),
        );
        eprintln!(
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
    batch_header: ProgressBar,
    bottom_sentinel: ProgressBar,
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

        let batch_header = multi.add(ProgressBar::new_spinner());
        batch_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        batch_header.set_message(format!(
            "Downloading {} items ({} workers)...",
            style(items_total).bold(),
            jobs,
        ));

        // Bottom sentinel — new item bars are inserted before this
        let bottom_sentinel = multi.add(ProgressBar::new_spinner());
        bottom_sentinel.set_style(ProgressStyle::with_template("{msg}").unwrap());
        bottom_sentinel.set_message("");
        bottom_sentinel.finish();

        Self {
            multi,
            batch_header,
            bottom_sentinel,
            active_item_bars: Mutex::new(HashMap::new()),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn on_item_start(&self, identifier: &str, _current: usize, _total: usize) {
        let item_header = self.multi.insert_before(
            &self.bottom_sentinel,
            ProgressBar::new_spinner(),
        );
        item_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        item_header.set_message(format!(
            "{} {}",
            style("▸").cyan(),
            style(identifier).bold(),
        ));

        let sentinel = self.multi.insert_before(
            &self.bottom_sentinel,
            ProgressBar::new_spinner(),
        );
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
            DownloadStatus::Enumerated { .. } | DownloadStatus::Verifying => {}
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
        // Finish all multi-progress bars so they render their final state
        self.batch_header.finish_and_clear();
        self.bottom_sentinel.finish_and_clear();

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

/// Progress display for file uploads.
///
/// Manages per-file progress bars via `MultiProgress`. Create once, then
/// pass `update` as the progress callback to `upload_item`.
pub struct UploadDisplay {
    multi: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    style: ProgressStyle,
}

impl UploadDisplay {
    pub fn new() -> Self {
        let style = ProgressStyle::with_template(
            "{spinner:.green} {prefix:.bold} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec})",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=> ");

        Self {
            multi: MultiProgress::new(),
            bars: Mutex::new(HashMap::new()),
            style,
        }
    }

    /// Handle an upload progress event — create/update/remove progress bars.
    pub fn update(&self, p: UploadProgress) {
        let mut bars = self.bars.lock().unwrap_or_else(|e| e.into_inner());
        match p.status {
            UploadProgressStatus::Enumerated { .. } => {
                // Nothing to do here yet — future tasks will use this to set
                // the aggregate bar length.
            }
            UploadProgressStatus::Uploading => {
                let pb = bars.entry(p.key.clone()).or_insert_with(|| {
                    let pb = self.multi.add(ProgressBar::new(p.total_bytes));
                    pb.set_style(self.style.clone());
                    pb.set_prefix(p.key.clone());
                    pb
                });
                pb.set_position(p.bytes_sent);
            }
            UploadProgressStatus::Verifying => {
                let pb = bars.entry(p.key.clone()).or_insert_with(|| {
                    let pb = self.multi.add(ProgressBar::new(p.total_bytes));
                    pb.set_style(self.style.clone());
                    pb.set_prefix(p.key.clone());
                    pb
                });
                pb.set_message("verifying...");
            }
            UploadProgressStatus::Complete => {
                if let Some(pb) = bars.remove(&p.key) {
                    pb.finish_and_clear();
                }
            }
            UploadProgressStatus::Skipped => {
                if let Some(pb) = bars.remove(&p.key) {
                    pb.finish_and_clear();
                }
            }
            UploadProgressStatus::Failed => {
                if let Some(pb) = bars.remove(&p.key) {
                    pb.abandon();
                }
            }
            UploadProgressStatus::WaitingRateLimit => {
                if let Some(pb) = bars.get(&p.key) {
                    pb.set_message("rate limited, waiting...");
                }
            }
        }
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

/// Format a byte count as a human-readable string using binary prefixes.
///
/// Examples: `"512 B"`, `"1.5 KiB"`, `"23.4 MiB"`, `"1.20 GiB"`, `"2.50 TiB"`.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < MIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < GIB {
        format!("{:.1} MiB", b / MIB)
    } else if b < TIB {
        format!("{:.2} GiB", b / GIB)
    } else {
        format!("{:.2} TiB", b / TIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_small() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn format_bytes_kib() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn format_bytes_mib() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(10 * 1024 * 1024 + 512 * 1024), "10.5 MiB");
    }

    #[test]
    fn format_bytes_gib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "2.50 GiB");
    }

    #[test]
    fn format_bytes_tib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.00 TiB");
    }
}
