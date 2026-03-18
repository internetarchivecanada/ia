use console::{style, Color};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};
use ia_core::upload::{UploadProgress, UploadProgressStatus, UploadResult, UploadStatus};

// ─── Shared progress style ──────────────────────────────────────────────────

/// Progress bar characters: filled, head, empty.
const PROGRESS_CHARS: &str = "━╸─";

/// Progress bar width for per-item bars.
/// Total line: 2 (indent) + BAR + bytes + speed + msg ≈ 78 cols.
const BAR_WIDTH: usize = 28;

// Icons used across all progress displays.
const ICON_HEADER: &str = "▸";
const ICON_SUCCESS: &str = "✓";
const ICON_ERROR: &str = "✗";
const ICON_SKIPPED: &str = "–";
const ICON_DRY_RUN: &str = "⊘";

/// Create a progress bar with the shared style.
fn make_progress_bar(total_bytes: u64) -> ProgressBar {
    let bar = ProgressBar::new(total_bytes);
    bar.set_style(
        ProgressStyle::with_template(&format!(
            "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes}}/{{total_bytes}} {{bytes_per_sec:.dim}}  ({{msg}})"
        ))
        .unwrap()
        .progress_chars(PROGRESS_CHARS),
    );
    bar.set_message("starting...");
    bar
}

/// Print `▸ identifier` header line to stderr.
fn print_item_header(identifier: &str) {
    eprintln!("{} {}", style(ICON_HEADER).cyan(), style(identifier).bold(),);
}

/// Format transfer speed as `· X.X MiB/s`, or empty string if elapsed is zero.
fn format_speed(bytes: u64, elapsed_secs: f64) -> String {
    if elapsed_secs > 0.0 {
        format!(
            " · {}/s",
            format_bytes((bytes as f64 / elapsed_secs) as u64)
        )
    } else {
        String::new()
    }
}

/// Format a count: colored with `color` if non-zero, plain "0" otherwise.
fn colored_count(count: usize, color: Color) -> String {
    if count > 0 {
        style(count.to_string()).fg(color).to_string()
    } else {
        "0".to_string()
    }
}

/// Stats for a completed single-item operation.
struct ItemFinish<'a> {
    identifier: &'a str,
    verb: &'a str,
    files_done: usize,
    files_skipped: usize,
    files_failed: usize,
    bytes_total: u64,
    elapsed_secs: f64,
    dry_run: bool,
}

/// Print the standard item completion block to stderr.
fn print_item_finish(f: &ItemFinish<'_>) {
    let speed = format_speed(f.bytes_total, f.elapsed_secs);
    let icon = if f.dry_run {
        style(ICON_DRY_RUN).dim()
    } else if f.files_failed > 0 {
        style(ICON_ERROR).red()
    } else if f.files_done == 0 && f.files_skipped > 0 {
        style(ICON_SKIPPED).dim()
    } else {
        style(ICON_SUCCESS).green()
    };
    eprintln!(
        "{}  {} files ({}) in {:.1}s{}",
        style(f.identifier).bold(),
        f.files_done,
        format_bytes(f.bytes_total),
        f.elapsed_secs,
        style(&speed).dim(),
    );
    eprintln!(
        "  {} {} {} · {} skipped · {} errors",
        icon,
        f.files_done,
        f.verb,
        colored_count(f.files_skipped, Color::Yellow),
        colored_count(f.files_failed, Color::Red),
    );
}

/// Aggregate stats for a batch operation's summary block.
pub struct BatchSummary {
    pub items_total: usize,
    pub items_succeeded: usize,
    pub items_failed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_total: u64,
    pub elapsed_secs: f64,
}

/// Print the batch summary footer to stderr.
pub fn print_batch_summary(
    summary: &BatchSummary,
    verb: &str,
    disk_statuses: Option<&[ia_core::disk_pool::DiskStatus]>,
) {
    let speed = format_speed(summary.bytes_total, summary.elapsed_secs);

    eprintln!(
        "{}",
        style("────────────────────────────────────────────────────").dim()
    );
    let errors_total = summary.files_failed + summary.items_failed;
    eprintln!(
        "{}/{} items ({} done) · {} files skipped · {} file errors",
        summary.items_succeeded,
        summary.items_total,
        style(summary.items_succeeded).green(),
        colored_count(summary.files_skipped, Color::Yellow),
        colored_count(errors_total, Color::Red),
    );
    eprintln!(
        "{} {}{}",
        format_bytes(summary.bytes_total),
        verb,
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

    eprintln!("{:.1}s elapsed", summary.elapsed_secs);
}

// ─── DownloadDisplay ────────────────────────────────────────────────────────

/// Single-item download progress: one aggregate bar, no dynamic insertion.
///
/// Uses a single `ProgressBar` tracking total bytes across all files.
/// The header is printed once via `eprintln!`, errors are collected and
/// printed at the end to avoid any redraw/flicker issues.
pub struct DownloadDisplay {
    identifier: String,
    bar: ProgressBar,
    per_file_bytes: Mutex<HashMap<String, u64>>,
    files_processed: Mutex<usize>,
    files_total: Mutex<usize>,
    errors: Mutex<Vec<String>>,
}

impl DownloadDisplay {
    pub fn new(identifier: &str) -> Self {
        print_item_header(identifier);
        let bar = make_progress_bar(0);

        Self {
            identifier: identifier.to_string(),
            bar,
            per_file_bytes: Mutex::new(HashMap::new()),
            files_processed: Mutex::new(0),
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
                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{processed}/{total} files"));
            }
            DownloadStatus::Skipped(_) => {
                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{processed}/{total} files"));
            }
            DownloadStatus::Failed(err) => {
                self.errors.lock().unwrap().push(format!(
                    "  {} {} {}",
                    style(ICON_ERROR).red(),
                    style(&progress.file_name).dim(),
                    style(format!("— {err}")).red(),
                ));
                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let total = *self.files_total.lock().unwrap();
                self.bar.set_message(format!("{processed}/{total} files"));
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

        print_item_finish(&ItemFinish {
            identifier: &self.identifier,
            verb: "downloaded",
            files_done: result.files_downloaded,
            files_skipped: result.files_skipped,
            files_failed: result.files_failed,
            bytes_total: result.bytes_total,
            elapsed_secs: result.elapsed.as_secs_f64(),
            dry_run: false,
        });

        if let Some(free) = disk_space_free(destdir) {
            eprintln!(
                "  {}: {} free",
                style(destdir.display()).dim(),
                format_bytes(free)
            );
        }
    }
}

// ─── BatchDisplay ───────────────────────────────────────────────────────────

pub struct BatchDisplay {
    multi: MultiProgress,
    batch_header: ProgressBar,
    bottom_sentinel: ProgressBar,
    active_item_bars: Mutex<HashMap<String, ItemBars>>,
    #[allow(dead_code)]
    started_at: Instant,
}

struct ItemBars {
    header: ProgressBar,
    bar: ProgressBar,
    per_file_bytes: HashMap<String, u64>,
    files_processed: usize,
    files_total: usize,
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
            started_at: Instant::now(),
        }
    }

    pub fn on_item_start(&self, identifier: &str, _current: usize, _total: usize) {
        // Insert bare bars first, configure after — configuring before
        // insertion corrupts MultiProgress cursor tracking (indicatif #677).
        let item_header = self
            .multi
            .insert_before(&self.bottom_sentinel, ProgressBar::new_spinner());
        item_header.set_style(ProgressStyle::with_template("{msg}").unwrap());
        item_header.set_message(format!(
            "{} {}",
            style(ICON_HEADER).cyan(),
            style(identifier).bold(),
        ));

        let bar = self
            .multi
            .insert_before(&self.bottom_sentinel, ProgressBar::new(0));
        bar.set_style(
            ProgressStyle::with_template(&format!(
                "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes}}/{{total_bytes}} {{bytes_per_sec:.dim}}  ({{msg}})"
            ))
            .unwrap()
            .progress_chars(PROGRESS_CHARS),
        );
        bar.set_message("starting...");

        let mut items = self.active_item_bars.lock().unwrap();
        items.insert(
            identifier.to_string(),
            ItemBars {
                header: item_header,
                bar,
                per_file_bytes: HashMap::new(),
                files_processed: 0,
                files_total: 0,
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
            DownloadStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                item.bar.set_length(*bytes_total);
                item.files_total = *files_count;
                item.bar.set_message(format!("0/{files_count} files"));
            }
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                item.per_file_bytes
                    .insert(progress.file_name.clone(), progress.bytes_downloaded);
                let total: u64 = item.per_file_bytes.values().sum();
                item.bar.set_position(total);
            }
            DownloadStatus::Complete => {
                if let Some(size) = progress.total_bytes {
                    item.per_file_bytes.insert(progress.file_name.clone(), size);
                    let total: u64 = item.per_file_bytes.values().sum();
                    item.bar.set_position(total);
                }
                item.files_processed += 1;
                item.bar.set_message(format!(
                    "{}/{} files",
                    item.files_processed, item.files_total
                ));
            }
            DownloadStatus::Skipped(_) => {
                item.files_processed += 1;
                item.bar.set_message(format!(
                    "{}/{} files",
                    item.files_processed, item.files_total
                ));
            }
            DownloadStatus::Failed(_) => {
                item.files_processed += 1;
                item.bar.set_message(format!(
                    "{}/{} files",
                    item.files_processed, item.files_total
                ));
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn on_item_complete(&self, result: &ia_core::download::ItemDownloadResult) {
        let identifier = &result.identifier;
        let mut items = self.active_item_bars.lock().unwrap();
        if let Some(item) = items.remove(identifier) {
            item.bar.finish_and_clear();

            let elapsed = result.elapsed.as_secs_f64();
            let speed = format_speed(result.bytes_total, elapsed);

            let skipped_info = if result.files_skipped > 0 {
                format!(
                    "\n  {} {} skipped",
                    style(ICON_SKIPPED).dim(),
                    style(result.files_skipped).yellow()
                )
            } else {
                String::new()
            };

            item.header.set_message(format!(
                "{} {}       {} files ({}) {:.0}s{}{}",
                style(ICON_SUCCESS).green(),
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
        self.batch_header.finish_and_clear();
        self.bottom_sentinel.finish_and_clear();

        let summary = BatchSummary {
            items_total: result.items_total,
            items_succeeded: result.items_succeeded,
            items_failed: result.items_failed,
            files_skipped: result.files_skipped,
            files_failed: result.files_failed,
            bytes_total: result.bytes_total,
            elapsed_secs: result.elapsed.as_secs_f64(),
        };
        print_batch_summary(&summary, "downloaded", disk_statuses);
    }
}

// ─── UploadDisplay ──────────────────────────────────────────────────────────

/// Single-item upload progress: one aggregate bar, no per-file bars.
///
/// Matches `DownloadDisplay` pattern: header on creation, aggregate byte
/// bar during transfer, summary block on finish.
pub struct UploadDisplay {
    identifier: String,
    bar: ProgressBar,
    per_file_bytes: Mutex<HashMap<String, u64>>,
    files_processed: Mutex<usize>,
    files_skipped: Mutex<usize>,
    files_total: Mutex<usize>,
    bytes_total: Mutex<u64>,
    errors: Mutex<Vec<String>>,
    dry_run: Mutex<bool>,
    started_at: Instant,
}

impl UploadDisplay {
    pub fn new(identifier: &str, dry_run: bool) -> Self {
        print_item_header(identifier);
        let bar = make_progress_bar(0);

        Self {
            identifier: identifier.to_string(),
            bar,
            per_file_bytes: Mutex::new(HashMap::new()),
            files_processed: Mutex::new(0),
            files_skipped: Mutex::new(0),
            files_total: Mutex::new(0),
            bytes_total: Mutex::new(0),
            errors: Mutex::new(Vec::new()),
            dry_run: Mutex::new(dry_run),
            started_at: Instant::now(),
        }
    }

    /// Handle an upload progress event.
    pub fn update(&self, p: UploadProgress) {
        match p.status {
            UploadProgressStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                self.bar.set_length(bytes_total);
                *self.files_total.lock().unwrap() = files_count;
                *self.bytes_total.lock().unwrap() = bytes_total;
                self.bar.set_message(format!("0/{files_count} files"));
            }
            UploadProgressStatus::Uploading => {
                let mut map = self.per_file_bytes.lock().unwrap();
                map.insert(p.key.clone(), p.bytes_sent);
                let total: u64 = map.values().sum();
                self.bar.set_position(total);
            }
            UploadProgressStatus::Verifying => {
                // Verifying doesn't change byte count
            }
            UploadProgressStatus::Complete => {
                let mut map = self.per_file_bytes.lock().unwrap();
                map.insert(p.key.clone(), p.total_bytes);
                let total: u64 = map.values().sum();
                self.bar.set_position(total);
                drop(map);

                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar
                    .set_message(format!("{processed}/{files_total} files"));
            }
            UploadProgressStatus::Skipped | UploadProgressStatus::Resumed => {
                *self.files_skipped.lock().unwrap() += 1;
                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar
                    .set_message(format!("{processed}/{files_total} files"));
            }
            UploadProgressStatus::Failed => {
                self.errors.lock().unwrap().push(format!(
                    "  {} {} {}",
                    style(ICON_ERROR).red(),
                    style(&p.key).dim(),
                    style("— upload failed").red(),
                ));
                let mut processed = self.files_processed.lock().unwrap();
                *processed += 1;
                let files_total = *self.files_total.lock().unwrap();
                self.bar
                    .set_message(format!("{processed}/{files_total} files"));
            }
            UploadProgressStatus::WaitingRateLimit => {
                self.bar.set_message("rate limited, waiting...");
            }
            UploadProgressStatus::Retrying => {
                self.bar.set_message("retrying...");
            }
        }
    }

    /// Finish the display: clear the bar and print summary.
    pub fn finish(&self) {
        self.bar.finish_and_clear();

        let errors = self.errors.lock().unwrap();
        for err in errors.iter() {
            eprintln!("{err}");
        }
        let files_failed = errors.len();
        drop(errors);

        let elapsed = self.started_at.elapsed().as_secs_f64();
        let files_processed = *self.files_processed.lock().unwrap();
        let files_skipped = *self.files_skipped.lock().unwrap();
        let files_uploaded = files_processed
            .saturating_sub(files_failed)
            .saturating_sub(files_skipped);
        let bytes_total = *self.bytes_total.lock().unwrap();
        let dry_run = *self.dry_run.lock().unwrap();

        print_item_finish(&ItemFinish {
            identifier: &self.identifier,
            verb: "uploaded",
            files_done: files_uploaded,
            files_skipped,
            files_failed,
            bytes_total,
            elapsed_secs: elapsed,
            dry_run,
        });
    }
}

// ─── UploadBatchDisplay ─────────────────────────────────────────────────────

/// Multi-item upload progress display.
///
/// Driven entirely by `UploadProgress` events: creates aggregate bars on
/// `Enumerated`, tracks bytes on `Uploading`, auto-detects item completion
/// when the file counter reaches the expected count.
pub struct UploadBatchDisplay {
    multi: MultiProgress,
    batch_header: ProgressBar,
    bottom_sentinel: ProgressBar,
    active_items: Mutex<HashMap<String, UploadItemBars>>,
    items_total: usize,
    items_completed: AtomicUsize,
    bytes_uploaded: AtomicU64,
    started_at: Instant,
    last_header_refresh: Mutex<Instant>,
}

struct UploadItemBars {
    header: ProgressBar,
    bar: ProgressBar,
    per_file_bytes: HashMap<String, u64>,
    files_processed: usize,
    files_skipped: usize,
    files_total: usize,
    bytes_total: u64,
    errors: Vec<String>,
    started_at: Instant,
}

impl UploadBatchDisplay {
    pub fn new(items_total: usize, _jobs: usize) -> Self {
        let multi = MultiProgress::new();

        // Primary status line — bright, the thing your eye should land on
        let batch_header = multi.add(ProgressBar::new_spinner());
        batch_header.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        batch_header.set_message(format!(
            "Uploading  0/{} items  0 B uploaded",
            style(items_total).bold(),
        ));
        batch_header.enable_steady_tick(Duration::from_millis(80));

        // Blank line separating summary from per-item detail
        let spacer = multi.add(ProgressBar::new_spinner());
        spacer.set_style(ProgressStyle::with_template(" ").unwrap());
        spacer.finish();

        let bottom_sentinel = multi.add(ProgressBar::new_spinner());
        bottom_sentinel.set_style(ProgressStyle::with_template("{msg}").unwrap());
        bottom_sentinel.set_message("");
        bottom_sentinel.finish();

        Self {
            multi,
            batch_header,
            bottom_sentinel,
            active_items: Mutex::new(HashMap::new()),
            items_total,
            items_completed: AtomicUsize::new(0),
            bytes_uploaded: AtomicU64::new(0),
            started_at: Instant::now(),
            last_header_refresh: Mutex::new(Instant::now()),
        }
    }

    /// Handle an upload progress event.
    pub fn update(&self, p: UploadProgress) {
        let identifier = &p.identifier;

        match p.status {
            UploadProgressStatus::Enumerated {
                files_count,
                bytes_total,
            } => {
                // Guard: only create bars once per item (ignore duplicate Enumerated events)
                let mut items = self.active_items.lock().unwrap();
                if items.contains_key(identifier) {
                    return;
                }

                // Insert bare bars into MultiProgress THEN configure — configuring
                // before insertion corrupts cursor tracking (indicatif #677).
                let header = self
                    .multi
                    .insert_before(&self.bottom_sentinel, ProgressBar::new_spinner());
                header.set_style(ProgressStyle::with_template("{msg}").unwrap());
                header.set_message(format!(
                    "{} {}",
                    style(ICON_HEADER).dim().cyan(),
                    style(identifier).dim().bold(),
                ));

                let bar = self
                    .multi
                    .insert_before(&self.bottom_sentinel, ProgressBar::new(bytes_total));
                bar.set_style(
                    ProgressStyle::with_template(&format!(
                        "  {{bar:{BAR_WIDTH}.cyan/dim}} {{bytes:.dim}}/{{total_bytes:.dim}} {{bytes_per_sec:.dim}}  ({{msg:.dim}})"
                    ))
                    .unwrap()
                    .progress_chars(PROGRESS_CHARS),
                );
                bar.set_message(format!("0/{files_count} files"));

                items.insert(
                    identifier.to_string(),
                    UploadItemBars {
                        header,
                        bar,
                        per_file_bytes: HashMap::new(),
                        files_processed: 0,
                        files_skipped: 0,
                        files_total: files_count,
                        bytes_total,
                        errors: Vec::new(),
                        started_at: Instant::now(),
                    },
                );
            }
            UploadProgressStatus::Uploading => {
                let delta = {
                    let mut items = self.active_items.lock().unwrap();
                    if let Some(item) = items.get_mut(identifier) {
                        let prev: u64 = item.per_file_bytes.values().sum();
                        item.per_file_bytes.insert(p.key.clone(), p.bytes_sent);
                        let now: u64 = item.per_file_bytes.values().sum();
                        item.bar.set_position(now);
                        now.saturating_sub(prev)
                    } else {
                        0
                    }
                };
                if delta > 0 {
                    self.bytes_uploaded.fetch_add(delta, Ordering::Relaxed);
                    self.refresh_header();
                }
            }
            UploadProgressStatus::Complete => {
                let (should_finish, byte_delta) = {
                    let mut items = self.active_items.lock().unwrap();
                    if let Some(item) = items.get_mut(identifier) {
                        let prev: u64 = item.per_file_bytes.values().sum();
                        item.per_file_bytes.insert(p.key.clone(), p.total_bytes);
                        let now: u64 = item.per_file_bytes.values().sum();
                        item.bar.set_position(now);
                        item.files_processed += 1;
                        item.bar.set_message(format!(
                            "{}/{} files",
                            item.files_processed, item.files_total
                        ));
                        (
                            item.files_processed >= item.files_total,
                            now.saturating_sub(prev),
                        )
                    } else {
                        (false, 0)
                    }
                };
                if byte_delta > 0 {
                    self.bytes_uploaded.fetch_add(byte_delta, Ordering::Relaxed);
                    self.refresh_header();
                }
                if should_finish {
                    self.maybe_finish_item(identifier);
                }
            }
            UploadProgressStatus::Skipped | UploadProgressStatus::Resumed => {
                let should_finish = {
                    let mut items = self.active_items.lock().unwrap();
                    if let Some(item) = items.get_mut(identifier) {
                        item.files_skipped += 1;
                        item.files_processed += 1;
                        item.bar.set_message(format!(
                            "{}/{} files",
                            item.files_processed, item.files_total
                        ));
                        item.files_processed >= item.files_total
                    } else {
                        false
                    }
                };
                if should_finish {
                    self.maybe_finish_item(identifier);
                }
            }
            UploadProgressStatus::Failed => {
                let should_finish = {
                    let mut items = self.active_items.lock().unwrap();
                    if let Some(item) = items.get_mut(identifier) {
                        item.errors.push(format!(
                            "  {} {} {}",
                            style(ICON_ERROR).red(),
                            style(&p.key).dim(),
                            style("— upload failed").red(),
                        ));
                        item.files_processed += 1;
                        item.bar.set_message(format!(
                            "{}/{} files",
                            item.files_processed, item.files_total
                        ));
                        item.files_processed >= item.files_total
                    } else {
                        false
                    }
                };
                if should_finish {
                    self.maybe_finish_item(identifier);
                }
            }
            UploadProgressStatus::WaitingRateLimit => {
                let mut items = self.active_items.lock().unwrap();
                if let Some(item) = items.get_mut(identifier) {
                    item.bar.set_message("rate limited, waiting...");
                }
            }
            UploadProgressStatus::Retrying => {
                let mut items = self.active_items.lock().unwrap();
                if let Some(item) = items.get_mut(identifier) {
                    item.bar.set_message("retrying...");
                }
            }
            UploadProgressStatus::Verifying => {
                // No visual update needed
            }
        }
    }

    /// Update the batch header spinner with current item/byte counters.
    fn refresh_header(&self) {
        // Throttle: skip if <80ms since last refresh (matches spinner tick rate).
        let now = Instant::now();
        {
            let mut last = self.last_header_refresh.lock().unwrap();
            if now.duration_since(*last) < Duration::from_millis(80) {
                return;
            }
            *last = now;
        }

        let completed = self.items_completed.load(Ordering::Relaxed);
        let bytes = self.bytes_uploaded.load(Ordering::Relaxed);
        let elapsed = self.started_at.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            format!("  {}/s", format_bytes((bytes as f64 / elapsed) as u64))
        } else {
            String::new()
        };
        self.batch_header.set_message(format!(
            "Uploading  {}/{} items  {} uploaded{}",
            completed,
            style(self.items_total).bold(),
            format_bytes(bytes),
            style(&speed).dim(),
        ));
    }

    /// Finalize an item when all its files are done.
    fn maybe_finish_item(&self, identifier: &str) {
        // Extract the item from the lock, then release the lock before
        // calling multi.println() (which must not be called while holding
        // the items mutex).
        let item = {
            let mut items = self.active_items.lock().unwrap();
            items.remove(identifier)
        };

        let Some(item) = item else { return };

        self.items_completed.fetch_add(1, Ordering::Relaxed);
        self.refresh_header();

        item.bar.finish_and_clear();

        for err in &item.errors {
            let _ = self.multi.println(err);
        }

        let has_errors = !item.errors.is_empty();
        let elapsed = item.started_at.elapsed().as_secs_f64();
        let speed = format_speed(item.bytes_total, elapsed);
        let files_uploaded = item
            .files_processed
            .saturating_sub(item.errors.len())
            .saturating_sub(item.files_skipped);
        let error_info = if has_errors {
            format!(
                "  {} {} errors",
                style(ICON_ERROR).red(),
                style(item.errors.len()).red()
            )
        } else {
            String::new()
        };
        let skipped_info = if item.files_skipped > 0 {
            format!(
                "  {} {} skipped",
                style(ICON_SKIPPED).dim(),
                style(item.files_skipped).yellow()
            )
        } else {
            String::new()
        };

        let icon = if has_errors {
            style(ICON_ERROR).red()
        } else if files_uploaded == 0 && item.files_skipped > 0 {
            style(ICON_SKIPPED).dim()
        } else {
            style(ICON_SUCCESS).green()
        };

        item.header.set_message(format!(
            "{} {}  {} files ({}) {:.0}s{}{}{}",
            icon,
            style(identifier).bold(),
            files_uploaded,
            format_bytes(item.bytes_total),
            elapsed,
            style(&speed).dim(),
            error_info,
            skipped_info,
        ));

        if has_errors {
            item.header.finish();
        } else {
            item.header.finish_and_clear();
        }
    }

    /// Finish the batch display and print a summary.
    pub fn finish(&self, results: &[UploadResult], elapsed: Duration) {
        self.batch_header.finish_and_clear();
        self.bottom_sentinel.finish_and_clear();

        let mut items_succeeded = 0usize;
        let mut items_failed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_failed = 0usize;
        let mut bytes_total = 0u64;

        // Group results by identifier to compute per-item stats
        let mut item_had_failure: HashMap<String, bool> = HashMap::new();
        for r in results {
            match &r.status {
                UploadStatus::Uploaded => {
                    bytes_total += r.bytes;
                }
                UploadStatus::Skipped | UploadStatus::Resumed => {
                    files_skipped += 1;
                }
                UploadStatus::Failed(_) => {
                    files_failed += 1;
                    item_had_failure.insert(r.identifier.clone(), true);
                }
                UploadStatus::DryRun => {
                    bytes_total += r.bytes;
                }
            }
        }

        // Count items that succeeded vs failed
        let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for r in results {
            seen_ids.insert(&r.identifier);
        }

        for id in &seen_ids {
            if item_had_failure.contains_key(*id) {
                items_failed += 1;
            } else {
                items_succeeded += 1;
            }
        }

        let summary = BatchSummary {
            items_total: self.items_total,
            items_succeeded,
            items_failed,
            files_skipped,
            files_failed,
            bytes_total,
            elapsed_secs: elapsed.as_secs_f64(),
        };
        print_batch_summary(&summary, "uploaded", None);
    }
}

// ─── Utilities ──────────────────────────────────────────────────────────────

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
        assert_eq!(
            format_bytes(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "2.50 GiB"
        );
    }

    #[test]
    fn format_bytes_tib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.00 TiB");
    }
}
