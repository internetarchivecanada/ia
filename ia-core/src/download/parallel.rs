//! Parallel Range downloads within one file (#16).
//!
//! A file of [`MIN_SIZE`] or more is cut into consecutive [`PART_SIZE`]
//! parts on a fixed grid. Up to [`DownloadOpts::streams`] streams, each on
//! its own HTTP/1.1 connection, take the next unfetched part from a shared
//! queue and fetch it with one closed `Range` request, writing it at its
//! offset in the `.part` file. Measured on 2026-10-30, four such
//! connections ran 2.41 times as fast as one, and eight 3.50 times; four
//! ranges multiplexed on one HTTP/2 connection gained nothing
//! (`docs/plans/parallel-transfer-measurement-2026-10-30.md`).
//!
//! Progress is kept in a journal beside the `.part` file
//! (`<name>.part.json`, see [`Journal`]), so an interrupted download resumes
//! per part. A journal beside a `.part` means the `.part` may have holes and
//! its length means nothing; no journal means the `.part` is a contiguous
//! prefix of the file, which is what the single-stream path assumes. The
//! journal is therefore written before the first byte lands in the `.part`
//! and deleted only once every part is on disk; the single-stream path
//! settles one it finds ([`settle_for_single_stream`]).
//!
//! The design and its reasons are in
//! `docs/plans/2026-11-03-parallel-range-downloads.md`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use futures::stream::FuturesUnordered;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::{DownloadOpts, DownloadProgress, DownloadStatus, FileDownloadResult};
use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::stall;
use crate::types::FileMetadata;

/// The number of streams per file unless the caller sets another.
pub const DEFAULT_STREAMS: usize = 4;

/// The most streams one file uses; a larger [`DownloadOpts::streams`] is
/// treated as this. The measurement tested up to eight connections.
pub const MAX_STREAMS: usize = 8;

/// The length of every part but the last. rclone's default chunk size for
/// the same feature; at the per-stream rate measured with four streams a
/// part takes about 20 s, so the first-byte wait of its request (a
/// `/download/` redirect, measured at 0.16 to 6.2 s) costs a few percent.
const PART_SIZE: u64 = 64 * 1024 * 1024;

/// The smallest file fetched in parallel: four parts, one for each default
/// stream. rclone's default cutoff for the same feature.
const MIN_SIZE: u64 = 256 * 1024 * 1024;

/// A stream records its progress in the journal each time it has written
/// this much since its last record, so a crash loses at most this much per
/// stream.
const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;

/// The journal format this code writes and reads.
const JOURNAL_VERSION: u32 = 1;

/// A journal larger than this is not read: a file of 1 TiB in 64 MiB parts
/// needs about 200 KiB.
const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;

/// How often, in bytes summed over all streams, a `Downloading` progress
/// event goes out. The single-stream path uses the same step.
const PROGRESS_STEP: u64 = 256 * 1024;

/// The part grid and checkpoint interval in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    part_size: u64,
    min_size: u64,
    checkpoint: u64,
}

/// The constants, except under `cfg(test)` while a [`LayoutOverride`] is
/// alive on this thread.
fn layout() -> Layout {
    #[cfg(test)]
    if let Some(layout) = test_layout::OVERRIDE.with(|cell| cell.get()) {
        return layout;
    }
    Layout {
        part_size: PART_SIZE,
        min_size: MIN_SIZE,
        checkpoint: CHECKPOINT_BYTES,
    }
}

#[cfg(test)]
use test_layout::LayoutOverride;

#[cfg(test)]
mod test_layout {
    use std::cell::Cell;

    use super::Layout;

    thread_local! {
        pub(super) static OVERRIDE: Cell<Option<Layout>> = const { Cell::new(None) };
    }

    /// Shrinks the part size, the threshold and the checkpoint interval for
    /// the rest of the test that holds it, so a test serves kilobytes
    /// instead of hundreds of mebibytes. Works for the single-threaded
    /// `#[tokio::test]` runtime, where every stream runs on the test's own
    /// thread.
    pub(crate) struct LayoutOverride;

    impl LayoutOverride {
        pub(crate) fn new(part_size: u64, min_size: u64, checkpoint: u64) -> Self {
            OVERRIDE.with(|cell| {
                cell.set(Some(Layout {
                    part_size,
                    min_size,
                    checkpoint,
                }))
            });
            Self
        }
    }

    impl Drop for LayoutOverride {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(None));
        }
    }
}

/// The part journal: which bytes of each part are on disk.
///
/// Stored as one JSON object at `<name>.part.json` beside `<name>.part`:
///
/// ```text
/// {"version":1,"size":2193522688,"md5":"44343bc1373c6dc826ebb58839155c39",
///  "part_size":67108864,"done":[67108864,67108864,1048576,0]}
/// ```
///
/// `size` and `md5` are the item metadata's values when the journal was
/// written (`md5` is `null` when the metadata has none); they tie the bytes
/// to one version of the file. Part *k* covers bytes `k × part_size` up to
/// the next boundary, the last part taking the remainder; `done[k]` bytes of
/// it, counted from its first byte, are on disk. Nothing past `done[k]` in a
/// part is trusted. The file is replaced, never edited ([`write_journal`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Journal {
    version: u32,
    size: u64,
    md5: Option<String>,
    part_size: u64,
    done: Vec<u64>,
}

impl Journal {
    /// A journal for a file of `size` bytes whose first `prefix` bytes are
    /// on disk. `part_size` must not be zero.
    fn new(size: u64, md5: Option<&str>, part_size: u64, prefix: u64) -> Self {
        let mut journal = Self {
            version: JOURNAL_VERSION,
            size,
            md5: md5.map(str::to_string),
            part_size,
            done: vec![0; size.div_ceil(part_size) as usize],
        };
        journal.done = (0..journal.done.len())
            .map(|k| {
                let (start, end) = journal.bounds(k);
                prefix.clamp(start, end) - start
            })
            .collect();
        journal
    }

    /// The first byte of part `k` and the byte after its last.
    fn bounds(&self, k: usize) -> (u64, u64) {
        let start = k as u64 * self.part_size;
        (start, start.saturating_add(self.part_size).min(self.size))
    }

    /// The length of part `k`.
    fn len_of(&self, k: usize) -> u64 {
        let (start, end) = self.bounds(k);
        end - start
    }

    /// The parts with bytes still to fetch, in file order.
    fn pending(&self) -> Vec<usize> {
        (0..self.done.len())
            .filter(|&k| self.done[k] < self.len_of(k))
            .collect()
    }

    /// Bytes on disk, summed over all parts.
    fn bytes_done(&self) -> u64 {
        self.done.iter().sum()
    }

    /// The length of the run of bytes on disk from byte 0: every complete
    /// part from the start, plus the done bytes of the first incomplete one.
    fn contiguous_prefix(&self) -> u64 {
        match self.pending().first() {
            Some(&k) => self.bounds(k).0 + self.done[k],
            None => self.size,
        }
    }

    /// The byte after the furthest byte the journal claims.
    fn furthest(&self) -> u64 {
        self.done
            .iter()
            .enumerate()
            .filter(|&(_, &done)| done > 0)
            .map(|(k, &done)| self.bounds(k).0 + done)
            .max()
            .unwrap_or(0)
    }

    /// Why this journal cannot be used for `file`, if it cannot: a format
    /// it does not know, another version of the file, or a shape that does
    /// not fit its own grid.
    fn check(&self, file: &FileMetadata) -> std::result::Result<(), String> {
        if self.version != JOURNAL_VERSION {
            return Err(format!(
                "it is format version {}, not {JOURNAL_VERSION}",
                self.version
            ));
        }
        if file.size != Some(self.size) {
            return Err(format!(
                "it was written for a file of {} bytes and the item metadata now says {}",
                self.size,
                file.size
                    .map_or_else(|| "nothing".to_string(), |s| format!("{s} bytes"))
            ));
        }
        if self.md5 != file.md5 {
            return Err(format!(
                "it was written for md5 {} and the item metadata now says {}",
                self.md5.as_deref().unwrap_or("(none)"),
                file.md5.as_deref().unwrap_or("(none)")
            ));
        }
        // Another part size would come from another version of this code;
        // a tiny one, from a damaged journal, would mean millions of
        // requests.
        if self.part_size != layout().part_size {
            return Err(format!(
                "its part size is {}, not {}",
                self.part_size,
                layout().part_size
            ));
        }
        let parts = self.size.div_ceil(self.part_size);
        if self.done.len() as u64 != parts {
            return Err(format!(
                "it lists {} parts where its grid has {parts}",
                self.done.len()
            ));
        }
        match (0..self.done.len()).find(|&k| self.done[k] > self.len_of(k)) {
            Some(k) => Err(format!(
                "it claims {} bytes of part {k}, which has {}",
                self.done[k],
                self.len_of(k)
            )),
            None => Ok(()),
        }
    }
}

/// `<name>.part.json` beside `part_path`.
pub(super) fn journal_path(part_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.json", part_path.display()))
}

/// The temporary [`write_journal`] renames over the journal at `path`.
fn temporary_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

/// Remove `path`; a file that is not there is not an error.
async fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What [`load_journal`] found.
#[derive(Debug)]
enum Loaded {
    /// No journal: the `.part`, if any, is a contiguous prefix.
    Absent,
    /// A journal that fits the file and the `.part` beside it.
    Valid(Journal),
    /// A journal that cannot be trusted, and why. The `.part` beside it may
    /// have holes, so both go.
    Invalid(String),
}

/// Read and check the journal at `path` for `file`, whose partial download
/// is at `part_path`.
///
/// Without a `.part` there is nothing to resume or settle, and a file at
/// `path` is left alone as [`Loaded::Absent`]: it may be an item file that
/// happens to have the journal's name. Otherwise, besides
/// [`Journal::check`], the journal must be a regular file that can be read,
/// and the `.part` must be a regular file that holds every byte the journal
/// claims and is no longer than the file.
async fn load_journal(path: &Path, file: &FileMetadata, part_path: &Path) -> Result<Loaded> {
    let meta = match fs::symlink_metadata(path).await {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::Absent),
        Err(e) => return Ok(Loaded::Invalid(format!("it cannot be read: {e}"))),
    };
    let held = match fs::symlink_metadata(part_path).await {
        Ok(part) if part.is_file() => part.len(),
        Ok(_) => {
            return Ok(Loaded::Invalid(
                "the .part beside it is not a regular file".to_string(),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::Absent),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return Ok(Loaded::Invalid("it is a symlink".to_string()));
    }
    if !meta.is_file() {
        return Ok(Loaded::Invalid("it is not a regular file".to_string()));
    }
    if meta.len() > MAX_JOURNAL_BYTES {
        return Ok(Loaded::Invalid(format!("it is {} bytes long", meta.len())));
    }
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) => return Ok(Loaded::Invalid(format!("it cannot be read: {e}"))),
    };
    let journal: Journal = match serde_json::from_slice(&bytes) {
        Ok(journal) => journal,
        Err(e) => return Ok(Loaded::Invalid(format!("it does not parse: {e}"))),
    };
    if let Err(why) = journal.check(file) {
        return Ok(Loaded::Invalid(why));
    }
    if held < journal.furthest() {
        return Ok(Loaded::Invalid(format!(
            "it claims bytes up to {} and the .part holds {held}",
            journal.furthest()
        )));
    }
    if held > journal.size {
        return Ok(Loaded::Invalid(format!(
            "the .part holds {held} bytes, more than the file's {}",
            journal.size
        )));
    }
    Ok(Loaded::Valid(journal))
}

/// Replace the journal at `path` with `journal`: write a temporary beside
/// it, sync it, and rename it over the journal, so a crash leaves the old
/// journal or the new one whole. The temporary is created with
/// `create_new` after any old one is removed, so the write never goes
/// through a link; the rename replaces a link at `path` without following
/// it.
async fn write_journal(path: &Path, journal: &Journal) -> std::io::Result<()> {
    let temporary = temporary_path(path);
    remove_if_present(&temporary).await?;
    let json = serde_json::to_vec(journal).map_err(std::io::Error::other)?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await?;
    out.write_all(&json).await?;
    out.flush().await?;
    out.sync_all().await?;
    drop(out);
    fs::rename(&temporary, path).await
}

/// Remove the journal at `path` and any temporary beside it.
async fn remove_journal(path: &Path) -> std::io::Result<()> {
    remove_if_present(path).await?;
    remove_if_present(&temporary_path(path)).await
}

/// Remove the `.part` at `part_path` and the journal beside it.
async fn discard(part_path: &Path) -> std::io::Result<()> {
    remove_if_present(part_path).await?;
    remove_journal(&journal_path(part_path)).await
}

/// Whether `file` is fetched over parallel ranges: two or more streams, a
/// size in the metadata at or above the threshold, and not the item's own
/// `_files.xml`, whose size is unknowable.
pub(super) fn applies(opts: &DownloadOpts, identifier: &str, file: &FileMetadata) -> bool {
    opts.streams >= 2
        && file.size.is_some_and(|size| size >= layout().min_size)
        && !super::is_size_unknowable(identifier, &file.name)
}

/// Settle a journal the single-stream path finds beside `part_path`, before
/// it reads the `.part` length.
///
/// A valid journal yields its contiguous prefix: the journal is first
/// rewritten to claim only that prefix, then the `.part` is truncated to it,
/// then the journal is removed. A crash at any step leaves a journal that
/// claims no more than the `.part` holds, or no journal and a `.part` that
/// is a prefix. An invalid journal is removed together with the `.part`.
pub(super) async fn settle_for_single_stream(file: &FileMetadata, part_path: &Path) -> Result<()> {
    let path = journal_path(part_path);
    match load_journal(&path, file, part_path).await? {
        Loaded::Absent => Ok(()),
        Loaded::Invalid(why) => {
            warn!(
                file = %file.name,
                reason = %why,
                "part journal cannot be used; removing it and the .part"
            );
            Ok(discard(part_path).await?)
        }
        Loaded::Valid(journal) => {
            let prefix = journal.contiguous_prefix();
            let trimmed = Journal::new(
                journal.size,
                journal.md5.as_deref(),
                journal.part_size,
                prefix,
            );
            write_journal(&path, &trimmed).await?;
            // load_journal saw a regular .part (or none); never truncate
            // through a link planted since.
            if fs::symlink_metadata(part_path)
                .await
                .is_ok_and(|meta| meta.is_file())
            {
                let part = fs::OpenOptions::new().write(true).open(part_path).await?;
                part.set_len(prefix).await?;
                part.sync_all().await?;
            }
            remove_journal(&path).await?;
            info!(
                file = %file.name,
                prefix,
                on_disk = journal.bytes_done(),
                "settled the part journal: resuming one stream from its contiguous prefix"
            );
            Ok(())
        }
    }
}

/// How [`download`] ended without an error.
#[derive(Debug)]
pub(super) enum Outcome {
    /// The file is complete.
    Done(FileDownloadResult),
    /// The first request was not answered with a 206 for exactly the range
    /// asked for; nothing on disk was changed for it, and the caller fetches
    /// the file on one stream.
    Unsupported,
}

/// Fetch `file` over parallel ranges into `part_path`, then verify it and
/// rename it to `file_path`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn download(
    client: &IaClient,
    identifier: &str,
    file: &FileMetadata,
    file_path: &Path,
    part_path: &Path,
    url: &str,
    opts: &DownloadOpts,
    progress: Option<&(dyn Fn(DownloadProgress) + Send + Sync)>,
    start: std::time::Instant,
) -> Result<Outcome> {
    let Some(size) = file.size else {
        return Ok(Outcome::Unsupported);
    };
    let layout = layout();
    let journal_file = journal_path(part_path);

    // A link at the .part goes first, as on the single-stream path (#25):
    // it must lend nothing to the resume decision.
    if fs::symlink_metadata(part_path)
        .await
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        warn!(file = %file.name, "removing symlink .part file before resume");
        fs::remove_file(part_path).await?;
    }

    let (journal, on_disk) = match load_journal(&journal_file, file, part_path).await? {
        Loaded::Valid(journal) => {
            debug!(
                file = %file.name,
                on_disk = journal.bytes_done(),
                "resuming from the part journal"
            );
            (journal, true)
        }
        Loaded::Invalid(why) => {
            warn!(
                file = %file.name,
                reason = %why,
                "part journal cannot be used; starting the file over"
            );
            discard(part_path).await?;
            (
                Journal::new(size, file.md5.as_deref(), layout.part_size, 0),
                false,
            )
        }
        Loaded::Absent => {
            let prefix = prefix_on_disk(file, part_path, size).await?;
            (
                Journal::new(size, file.md5.as_deref(), layout.part_size, prefix),
                false,
            )
        }
    };
    let pending = journal.pending();

    if let Some(p) = progress {
        p(DownloadProgress {
            identifier: identifier.to_string(),
            file_name: file.name.clone(),
            bytes_downloaded: journal.bytes_done(),
            total_bytes: Some(size),
            status: DownloadStatus::Starting,
        });
    }

    let mut last_modified = None;
    if let Some(&first) = pending.first() {
        // The first request goes alone and is the file's: an error here is
        // returned to the per-file retry loop, as on one stream, and an
        // answer that is not the range asked for means one stream.
        let span = journal.span(first);
        let response = fetch_range(client, url, file, span, opts.count_views).await?;
        if !answers_span(&response, identifier, file, span)? {
            return Ok(Outcome::Unsupported);
        }
        last_modified = super::last_modified_of(&response);
        // Before the first byte lands in the .part: from here on, the
        // .part may have holes.
        if !on_disk {
            write_journal(&journal_file, &journal).await?;
        }
        // The streams only open the .part; it is created here, once.
        create_part(file, part_path).await?;
        let streams = opts.streams.min(MAX_STREAMS).min(pending.len());
        info!(
            file = %file.name,
            streams,
            parts = pending.len(),
            part_size = journal.part_size,
            "downloading over parallel ranges"
        );
        let shared = Shared {
            client,
            identifier,
            file,
            part_path,
            url,
            opts,
            progress,
            checkpoint: layout.checkpoint,
            journal_file: &journal_file,
            received: AtomicU64::new(journal.bytes_done()),
            reported: StdMutex::new(journal.bytes_done()),
            queue: StdMutex::new(pending[1..].iter().copied().collect()),
            journal: tokio::sync::Mutex::new(journal),
        };
        run_streams(&shared, streams, first, response).await?;
    }

    // Every part is on disk and synced, so the .part is the whole file:
    // without the journal it is a contiguous prefix of full length, which
    // both paths finish in place.
    let held = fs::symlink_metadata(part_path)
        .await
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len());
    if held != Some(size) {
        warn!(
            file = %file.name,
            held = ?held,
            size,
            "every part is recorded but the .part is not the file's length; deleting it"
        );
        discard(part_path).await?;
        return Err(IaError::ResumeFailed {
            file: file.name.clone(),
            reason: format!(
                "every part was recorded as written but the .part file is {} bytes, not \
                 {size}, so it was removed",
                held.map_or_else(|| "missing or not".to_string(), |h| h.to_string())
            ),
        });
    }
    remove_journal(&journal_file).await?;

    let hasher = if super::verifies(opts, identifier, file) {
        Some(super::seed_hasher_from_part(identifier, file, part_path, size, progress).await?)
    } else {
        None
    };
    super::finish_part(
        identifier,
        file,
        file_path,
        part_path,
        size,
        hasher,
        last_modified,
        opts,
        progress,
        start,
    )
    .await
    .map(Outcome::Done)
}

impl Journal {
    /// The first and last byte part `k` still needs, for its `Range`.
    fn span(&self, k: usize) -> (u64, u64) {
        let (start, end) = self.bounds(k);
        (start + self.done[k], end - 1)
    }
}

/// How many bytes of the file a `.part` without a journal holds: its length,
/// as a contiguous prefix. Opened first and the handle's metadata read, as
/// on the single-stream path, so a directory or other non-regular file
/// counts as nothing (and fails when a stream opens it). A `.part` longer
/// than the file is not a prefix of it and is removed.
async fn prefix_on_disk(file: &FileMetadata, part_path: &Path, size: u64) -> Result<u64> {
    let held = match fs::File::open(part_path).await {
        Ok(f) => {
            let meta = f.metadata().await?;
            if meta.is_file() {
                meta.len()
            } else {
                warn!(file = %file.name, "skipping resume: .part path is not a regular file");
                0
            }
        }
        Err(_) => 0,
    };
    if held > size {
        warn!(
            file = %file.name,
            held,
            size,
            ".part is longer than the file; deleting it"
        );
        fs::remove_file(part_path).await?;
        return Ok(0);
    }
    Ok(held)
}

/// Send one closed `Range` request for `span` over the HTTP/1.1 transport.
///
/// A 416 means the server's copy ends before the span: a `Content-Range`
/// total that differs from the metadata size is
/// [`IaError::ServerSizeMismatch`], anything else the plain
/// [`IaError::Http`].
async fn fetch_range(
    client: &IaClient,
    url: &str,
    file: &FileMetadata,
    (first, last): (u64, u64),
    count_views: bool,
) -> Result<reqwest::Response> {
    let response = super::fetch_response_via(
        client.range_http(),
        client,
        url,
        Some(super::ByteRange::Span { first, last }),
        count_views,
    )
    .await?;
    if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let server_size = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(super::parse_content_range_total);
        if let (Some(server_size), Some(metadata_size)) = (server_size, file.size) {
            if server_size != metadata_size {
                return Err(IaError::ServerSizeMismatch {
                    file: file.name.clone(),
                    metadata_size,
                    server_size,
                });
            }
        }
        return Err(super::http_error_from(response).await);
    }
    Ok(response)
}

/// Whether `response` is a 206 for exactly `span`. A `Content-Range` total
/// that differs from the metadata size is [`IaError::ServerSizeMismatch`],
/// as on the single-stream path.
fn answers_span(
    response: &reqwest::Response,
    identifier: &str,
    file: &FileMetadata,
    span: (u64, u64),
) -> Result<bool> {
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Ok(false);
    }
    super::check_content_range(response, identifier, file)?;
    let answered = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(super::parse_content_range_span);
    Ok(answered == Some(span))
}

/// What the streams of one download share.
struct Shared<'a> {
    client: &'a IaClient,
    identifier: &'a str,
    file: &'a FileMetadata,
    part_path: &'a Path,
    url: &'a str,
    opts: &'a DownloadOpts,
    progress: Option<&'a (dyn Fn(DownloadProgress) + Send + Sync)>,
    checkpoint: u64,
    journal_file: &'a Path,
    /// Bytes on disk over all parts, including what no record covers yet.
    received: AtomicU64,
    /// The byte count of the last `Downloading` event.
    reported: StdMutex<u64>,
    /// The parts no stream has taken yet.
    queue: StdMutex<VecDeque<usize>>,
    /// The journal as last written. Held while it is written, so records
    /// from several streams go to disk one at a time and in order.
    journal: tokio::sync::Mutex<Journal>,
}

impl Shared<'_> {
    fn next_part(&self) -> Option<usize> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    /// Count `n` more bytes on disk and report progress every
    /// [`PROGRESS_STEP`]. The count is read under the lock, so the events
    /// only grow.
    fn received(&self, n: u64) {
        self.received.fetch_add(n, Ordering::Relaxed);
        let Some(p) = self.progress else {
            return;
        };
        let mut reported = self
            .reported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total = self.received.load(Ordering::Relaxed);
        if total >= *reported + PROGRESS_STEP {
            *reported = total;
            p(DownloadProgress {
                identifier: self.identifier.to_string(),
                file_name: self.file.name.clone(),
                bytes_downloaded: total,
                total_bytes: self.file.size,
                status: DownloadStatus::Downloading,
            });
        }
    }

    /// Flush `out`, sync the `.part`'s data, and record that `done` bytes
    /// of part `k` are on disk. The sync comes first, so the journal never
    /// claims a byte the disk may not have.
    async fn record(
        &self,
        k: usize,
        out: &mut tokio::io::BufWriter<fs::File>,
        done: u64,
    ) -> Result<()> {
        out.flush().await?;
        out.get_ref().sync_data().await?;
        let mut journal = self.journal.lock().await;
        if journal.done[k] < done {
            journal.done[k] = done;
            write_journal(self.journal_file, &journal).await?;
        }
        Ok(())
    }

    /// Open the `.part` for writing at `offset`, never through a link (see
    /// [`refuse_link`]). The `.part` is created before the streams start
    /// and is never created here: a `.part` that went away mid-download
    /// fails the attempt instead of coming back as a file with holes beside
    /// a journal that claims bytes it does not hold.
    async fn open_at(&self, offset: u64) -> Result<tokio::io::BufWriter<fs::File>> {
        let mut out = open_part(self.file, self.part_path, false).await?;
        out.seek(std::io::SeekFrom::Start(offset)).await?;
        Ok(tokio::io::BufWriter::with_capacity(256 * 1024, out))
    }
}

/// Fail the attempt if the `.part` at `part_path` is a link, removing the
/// link, so no byte is ever written through one planted since the download
/// started. Several streams can see the same link; the first removes it,
/// and the others report the same failure rather than the missing file.
async fn refuse_link(file: &FileMetadata, part_path: &Path) -> Result<()> {
    if fs::symlink_metadata(part_path)
        .await
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        warn!(file = %file.name, "removing symlink .part file");
        remove_if_present(part_path).await?;
        return Err(IaError::ResumeFailed {
            file: file.name.clone(),
            reason: ".part path is a symlink".to_string(),
        });
    }
    Ok(())
}

/// Open the `.part` at `part_path` for writing, creating it when `create`
/// is set and never truncating it, and never through a link.
///
/// [`refuse_link`] runs first, but a link can be put in place between that
/// check and the open. On Unix the open file is therefore compared with
/// what is at the path afterwards, before anything is written: unless the
/// path is still a regular file with the same device and inode, the handle
/// is dropped unused and the attempt fails.
async fn open_part(file: &FileMetadata, part_path: &Path, create: bool) -> Result<fs::File> {
    refuse_link(file, part_path).await?;
    let out = fs::OpenOptions::new()
        .write(true)
        .create(create)
        .truncate(false)
        .open(part_path)
        .await?;
    #[cfg(unix)]
    ensure_opened_at_path(file, &out, part_path).await?;
    Ok(out)
}

/// Fail unless `opened` is the regular file now at `part_path` (same device
/// and inode): a link there means the open may have followed it, and
/// another file there means the `.part` was replaced. A link is removed, as
/// [`refuse_link`] does. The caller drops `opened` unused on failure.
#[cfg(unix)]
async fn ensure_opened_at_path(
    file: &FileMetadata,
    opened: &fs::File,
    part_path: &Path,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = opened.metadata().await?;
    let at_path = fs::symlink_metadata(part_path).await?;
    if !at_path.file_type().is_symlink()
        && (opened.dev(), opened.ino()) == (at_path.dev(), at_path.ino())
    {
        return Ok(());
    }
    refuse_link(file, part_path).await?;
    Err(IaError::ResumeFailed {
        file: file.name.clone(),
        reason: ".part path was replaced while it was being opened".to_string(),
    })
}

/// Create the `.part` at `part_path` if it is not there, keeping what it
/// holds if it is, and never through a link.
async fn create_part(file: &FileMetadata, part_path: &Path) -> Result<()> {
    open_part(file, part_path, true).await.map(drop)
}

/// Run `streams` streams over the parts in `shared`'s queue; the first
/// stream starts with `response`, the answer to the request for part
/// `first`. When one stream fails, the others are told to stop: each records
/// its progress and ends. The first failure is the result.
async fn run_streams(
    shared: &Shared<'_>,
    streams: usize,
    first: usize,
    response: reqwest::Response,
) -> Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut first = Some((first, response));
    let mut running: FuturesUnordered<_> = (0..streams)
        .map(|_| stream(shared, first.take(), stop_rx.clone()))
        .collect();
    let mut failure = None;
    while let Some(ended) = running.next().await {
        if let Err(e) = ended {
            if failure.is_none() {
                let _ = stop_tx.send(true);
                failure = Some(e);
            } else {
                debug!(file = %shared.file.name, error = %e, "another stream failed while stopping");
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Whether a stream goes on to its next part.
enum Flow {
    Next,
    Stopped,
}

/// One stream's budgets, for the whole download attempt of its file.
struct Budget {
    /// `--retries`: failed requests, and stalls, each.
    retries: usize,
    failed_requests: usize,
    stream_errors: usize,
    stalls: usize,
    backoff: reqwest_retry::policies::ExponentialBackoff,
}

impl Budget {
    fn new(retries: usize) -> Self {
        Self {
            retries,
            failed_requests: 0,
            stream_errors: 0,
            stalls: 0,
            backoff: crate::retry::backoff_policy(
                crate::retry::STANDARD_MIN_DELAY,
                crate::retry::STANDARD_MAX_DELAY,
                retries as u32,
            ),
        }
    }

    /// A request of this stream for part `k` failed with `err`. A permanent
    /// error fails the file as it is; a retryable one spends one of the
    /// stream's retries and waits the standard backoff, or the response's
    /// `Retry-After`, before the stream sends the request again. With the
    /// retries spent, the file fails with [`IaError::RangeFailed`]. Returns
    /// `false` when the stream was told to stop while waiting.
    async fn after_failed_request(
        &mut self,
        shared: &Shared<'_>,
        k: usize,
        err: IaError,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<bool> {
        if !err.is_retryable() {
            return Err(err);
        }
        let (start, end) = {
            let journal = shared.journal.lock().await;
            journal.bounds(k)
        };
        if self.failed_requests >= self.retries {
            return Err(IaError::RangeFailed {
                file: shared.file.name.clone(),
                start,
                end: end - 1,
                attempts: self.failed_requests + 1,
                source: Box::new(err),
            });
        }
        let retry_after = err.retry_after().map(Duration::from_secs);
        let delay = crate::retry::wait_before_retry(
            retry_after,
            &self.backoff,
            self.failed_requests as u32,
        );
        self.failed_requests += 1;
        warn!(
            file = %shared.file.name,
            start,
            end = end - 1,
            attempt = self.failed_requests,
            max = self.retries,
            retry_after = retry_after.is_some(),
            error = %crate::error::format_error_chain(&err),
            "range request failed; retrying after {delay:?}"
        );
        Ok(or_stopped(stop, tokio::time::sleep(delay)).await.is_some())
    }
}

/// Wait until the stream is told to stop (or the download that runs it is
/// gone). The guard `wait_for` returns is dropped here: it is not `Send`,
/// and a `select!` that kept it would make the download future `!Send`.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// `work`, unless the stream is told to stop first.
async fn or_stopped<F: std::future::Future>(
    stop: &mut watch::Receiver<bool>,
    work: F,
) -> Option<F::Output> {
    tokio::select! {
        output = work => Some(output),
        _ = stopped(stop) => None,
    }
}

/// One stream: fetch part `first` from its response, if given, then take
/// parts from the queue until it is empty or the stream is told to stop.
async fn stream(
    shared: &Shared<'_>,
    mut first: Option<(usize, reqwest::Response)>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut budget = Budget::new(shared.opts.retries);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let (k, response) = match first.take() {
            Some((k, response)) => (k, Some(response)),
            None => match shared.next_part() {
                Some(k) => (k, None),
                None => return Ok(()),
            },
        };
        match fetch_part(shared, k, response, &mut budget, &mut stop).await? {
            Flow::Next => {}
            Flow::Stopped => return Ok(()),
        }
    }
}

/// Send the request for `span` of part `k`, again after each retryable
/// failure while the stream's retries last. An answer that is not a 206 for
/// exactly `span` is a failed request. `None` when told to stop.
async fn request_span(
    shared: &Shared<'_>,
    k: usize,
    span: (u64, u64),
    budget: &mut Budget,
    stop: &mut watch::Receiver<bool>,
) -> Result<Option<reqwest::Response>> {
    loop {
        // Only the file's first request may count a view (see `download`).
        let Some(sent) = or_stopped(
            stop,
            fetch_range(shared.client, shared.url, shared.file, span, false),
        )
        .await
        else {
            return Ok(None);
        };
        let err = match sent {
            Ok(response) => match answers_span(&response, shared.identifier, shared.file, span) {
                Ok(true) => return Ok(Some(response)),
                Ok(false) => IaError::ResumeFailed {
                    file: shared.file.name.clone(),
                    reason: format!(
                        "the server answered {} with Content-Range {} to a request for bytes \
                         {}-{}",
                        response.status().as_u16(),
                        response
                            .headers()
                            .get("content-range")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("(none)"),
                        span.0,
                        span.1
                    ),
                },
                Err(e) => e,
            },
            Err(e) => e,
        };
        if !budget.after_failed_request(shared, k, err, stop).await? {
            return Ok(None);
        }
    }
}

/// Why a stream stopped reading a response body.
enum Ended {
    /// The part is complete.
    Done,
    /// The stream was told to stop.
    Stopped,
    /// A body-stream error, a body that ended early, or one that ran past
    /// the part.
    Error(IaError),
    /// The stall detector judged the stream too slow.
    Stalled { observed: u64, window_secs: u64 },
}

/// Fetch the rest of part `k`, starting from `response` when given. Body
/// errors, stalls and failed requests are handled by the stream's rules
/// (see the plan); progress is recorded every checkpoint and whenever a
/// response ends.
async fn fetch_part(
    shared: &Shared<'_>,
    k: usize,
    mut response: Option<reqwest::Response>,
    budget: &mut Budget,
    stop: &mut watch::Receiver<bool>,
) -> Result<Flow> {
    let opts = shared.opts;
    let (start, len, mut done) = {
        let journal = shared.journal.lock().await;
        (journal.bounds(k).0, journal.len_of(k), journal.done[k])
    };
    let (stall_window, stall_grace) = stall::policy();
    while done < len {
        let span = (start + done, start + len - 1);
        let body_response = match response.take() {
            Some(response) => response,
            None => match request_span(shared, k, span, budget, stop).await? {
                Some(response) => response,
                None => return Ok(Flow::Stopped),
            },
        };
        let mut out = shared.open_at(start + done).await?;
        let mut body = body_response.bytes_stream();
        // Every response is judged on its own clock, as on one stream.
        let mut detector = (opts.min_speed > 0).then(|| {
            stall::StallDetector::new(
                opts.min_speed,
                stall_window,
                stall_grace,
                tokio::time::Instant::now(),
            )
        });
        let mut ticker = tokio::time::interval(stall::CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut unrecorded = 0u64;

        let ended = loop {
            tokio::select! {
                _ = stopped(stop) => break Ended::Stopped,
                next = body.next() => match next {
                    Some(Ok(chunk)) => {
                        let n = chunk.len() as u64;
                        // Never write into the next part.
                        if n > len - done {
                            break Ended::Error(IaError::ResumeFailed {
                                file: shared.file.name.clone(),
                                reason: format!(
                                    "the server sent more than bytes {}-{}",
                                    span.0, span.1
                                ),
                            });
                        }
                        out.write_all(&chunk).await?;
                        done += n;
                        unrecorded += n;
                        if let Some(d) = detector.as_mut() {
                            d.record(tokio::time::Instant::now(), n);
                        }
                        shared.received(n);
                        if unrecorded >= shared.checkpoint {
                            shared.record(k, &mut out, done).await?;
                            unrecorded = 0;
                        }
                    }
                    Some(Err(e)) => break Ended::Error(IaError::Network(e.into())),
                    None if done == len => break Ended::Done,
                    None => break Ended::Error(IaError::ResumeFailed {
                        file: shared.file.name.clone(),
                        reason: format!(
                            "the body for bytes {}-{} ended at byte {}",
                            span.0,
                            span.1,
                            start + done
                        ),
                    }),
                },
                _ = ticker.tick(), if detector.is_some() => {
                    if let Some(d) = detector.as_mut() {
                        if let Some(observed) = d.check(tokio::time::Instant::now()) {
                            break Ended::Stalled { observed, window_secs: d.window_secs() };
                        }
                    }
                }
            }
        };
        // Close the connection, then put what arrived on record: it is the
        // offset of the next request, or of a later run.
        drop(body);
        shared.record(k, &mut out, done).await?;
        drop(out);
        // The part may have arrived in full just as a stall check or a body
        // error won the race with the end of the body: it is done, and
        // nothing is spent on it.
        if done == len {
            break;
        }

        let err = match ended {
            Ended::Done => break,
            Ended::Stopped => return Ok(Flow::Stopped),
            Ended::Stalled {
                observed,
                window_secs,
            } => {
                if budget.stalls >= opts.retries {
                    let stalls = budget.stalls + 1;
                    warn!(
                        file = %shared.file.name,
                        start,
                        end = start + len - 1,
                        stalls,
                        observed_bytes_per_sec = observed,
                        min_bytes_per_sec = opts.min_speed,
                        window_secs,
                        "stream below --min-speed and its stall budget is spent; keeping .part"
                    );
                    return Err(IaError::DownloadStalled {
                        file: shared.file.name.clone(),
                        observed_bytes_per_sec: observed,
                        min_bytes_per_sec: opts.min_speed,
                        window_secs,
                        stalls,
                    });
                }
                budget.stalls += 1;
                // No backoff, as on one stream: the server is answering,
                // too slowly.
                warn!(
                    file = %shared.file.name,
                    start,
                    end = start + len - 1,
                    attempt = budget.stalls,
                    max = opts.retries,
                    done,
                    observed_bytes_per_sec = observed,
                    min_bytes_per_sec = opts.min_speed,
                    window_secs,
                    "stream below --min-speed, re-requesting the rest of its part",
                );
                continue;
            }
            Ended::Error(err) => err,
        };
        if budget.stream_errors < super::MAX_STREAM_RETRIES {
            budget.stream_errors += 1;
            let backoff = super::stream_retry_backoff(budget.stream_errors);
            warn!(
                file = %shared.file.name,
                start,
                end = start + len - 1,
                attempt = budget.stream_errors,
                max = super::MAX_STREAM_RETRIES,
                done,
                backoff_ms = backoff.as_millis() as u64,
                error = %crate::error::format_error_chain(&err),
                "body-stream error, re-requesting the rest of the part",
            );
            if or_stopped(stop, tokio::time::sleep(backoff))
                .await
                .is_none()
            {
                return Ok(Flow::Stopped);
            }
        } else {
            // A fourth error on the stream is one failed request; its three
            // re-requests are available again.
            budget.stream_errors = 0;
            if !budget.after_failed_request(shared, k, err, stop).await? {
                return Ok(Flow::Stopped);
            }
        }
    }
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{download_file, download_item_with_metadata};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    // -- fixtures --

    /// Parts of 1 KiB, a 4 KiB threshold, a checkpoint every 512 bytes.
    const PART: u64 = 1024;
    const MIN: u64 = 4 * PART;
    const CHECKPOINT: u64 = 512;
    /// Five parts, the last one short.
    const LEN: usize = 4 * PART as usize + 500;

    fn small_layout() -> LayoutOverride {
        LayoutOverride::new(PART, MIN, CHECKPOINT)
    }

    fn body_of(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn md5_hex(bytes: &[u8]) -> String {
        use md5::Digest;
        crate::upload::checksum::to_hex(&md5::Md5::digest(bytes))
    }

    fn file_meta(name: &str, body: &[u8]) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some("original".to_string()),
            format: None,
            md5: Some(md5_hex(body)),
            size: Some(body.len() as u64),
            mtime: Some(1700000000),
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }
    }

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        config.general.host = server_uri
            .strip_prefix("http://")
            .unwrap_or(server_uri)
            .to_string();
        config.general.secure = false;
        config
    }

    fn opts(streams: usize) -> DownloadOpts {
        DownloadOpts {
            streams,
            retries: 2,
            ..Default::default()
        }
    }

    /// Real time. A hung download fails the test instead of the suite.
    const LIMIT: Duration = Duration::from_secs(60);

    async fn fetch(
        client: &IaClient,
        file: &FileMetadata,
        dir: &Path,
        opts: &DownloadOpts,
    ) -> Result<FileDownloadResult> {
        tokio::time::timeout(
            LIMIT,
            download_file(client, "big-item", file, dir, opts, None),
        )
        .await
        .expect("download_file hung past the time limit")
    }

    /// `bytes=A-B` or `bytes=A-`.
    fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
        let (a, b) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
        let a = a.parse().ok()?;
        let b = if b.is_empty() {
            None
        } else {
            Some(b.parse().ok()?)
        };
        Some((a, b))
    }

    fn range_of(request: &Request) -> Option<String> {
        request
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    /// Serves `body`: a 206 for the range asked for, a 200 without `Range`.
    /// A queued response for a range's first byte is served instead, once.
    #[derive(Clone)]
    struct RangeServer {
        body: Arc<Vec<u8>>,
        scripted: Arc<Mutex<HashMap<u64, VecDeque<ResponseTemplate>>>>,
        /// Answer every request with a 200 and the whole body.
        ignore_range: bool,
        /// Put this total in `Content-Range` instead of the real one.
        total: Option<u64>,
    }

    impl RangeServer {
        fn new(body: &[u8]) -> Self {
            Self {
                body: Arc::new(body.to_vec()),
                scripted: Arc::default(),
                ignore_range: false,
                total: None,
            }
        }

        /// Answer the next `times` requests for the range starting at
        /// `first` with `response`.
        fn script(self, first: u64, times: usize, response: ResponseTemplate) -> Self {
            self.scripted
                .lock()
                .unwrap()
                .entry(first)
                .or_default()
                .extend(std::iter::repeat_n(response, times));
            self
        }
    }

    impl Respond for RangeServer {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let range = range_of(request).as_deref().and_then(parse_range);
            if let Some((first, _)) = range {
                if let Some(scripted) = self
                    .scripted
                    .lock()
                    .unwrap()
                    .get_mut(&first)
                    .and_then(VecDeque::pop_front)
                {
                    return scripted;
                }
            }
            let len = self.body.len() as u64;
            match range {
                Some((first, last)) if !self.ignore_range && first < len => {
                    let last = last.unwrap_or(len - 1).min(len - 1);
                    let total = self.total.unwrap_or(len);
                    ResponseTemplate::new(206)
                        .insert_header("content-range", format!("bytes {first}-{last}/{total}"))
                        .insert_header("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT")
                        .set_body_bytes(self.body[first as usize..=last as usize].to_vec())
                }
                _ => ResponseTemplate::new(200)
                    .insert_header("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT")
                    .set_body_bytes(self.body.to_vec()),
            }
        }
    }

    async fn serve(responder: RangeServer) -> (MockServer, IaClient) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/big-item/big.bin"))
            .respond_with(responder)
            .mount(&server)
            .await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        (server, client)
    }

    /// The `Range` header of every request, in the order they arrived.
    async fn ranges(server: &MockServer) -> Vec<Option<String>> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(range_of)
            .collect()
    }

    fn sorted(mut v: Vec<Option<String>>) -> Vec<Option<String>> {
        v.sort();
        v
    }

    fn closed(first: u64, last: u64) -> Option<String> {
        Some(format!("bytes={first}-{last}"))
    }

    fn all_parts() -> Vec<Option<String>> {
        sorted(vec![
            closed(0, 1023),
            closed(1024, 2047),
            closed(2048, 3071),
            closed(3072, 4095),
            closed(4096, LEN as u64 - 1),
        ])
    }

    fn part_path(dir: &Path) -> PathBuf {
        dir.join("big.bin.part")
    }

    fn journal_at(dir: &Path) -> PathBuf {
        dir.join("big.bin.part.json")
    }

    fn read_journal(dir: &Path) -> Journal {
        serde_json::from_slice(&std::fs::read(journal_at(dir)).unwrap()).unwrap()
    }

    /// A `.part` holding `body` where `done` says, and zeros elsewhere, up
    /// to the furthest claimed byte; and the journal `json` beside it.
    fn seed(dir: &Path, body: &[u8], done: &[u64], json: &str) {
        let mut part = vec![0u8; 0];
        for (k, &d) in done.iter().enumerate() {
            let start = k as u64 * PART;
            if d > 0 {
                let end = (start + d) as usize;
                if part.len() < end {
                    part.resize(end, 0);
                }
                part[start as usize..end].copy_from_slice(&body[start as usize..end]);
            }
        }
        std::fs::write(part_path(dir), part).unwrap();
        std::fs::write(journal_at(dir), json).unwrap();
    }

    fn journal_json(body: &[u8], md5: &str, done: &[u64]) -> String {
        let done: Vec<String> = done.iter().map(u64::to_string).collect();
        format!(
            r#"{{"version":1,"size":{},"md5":"{md5}","part_size":{PART},"done":[{}]}}"#,
            body.len(),
            done.join(",")
        )
    }

    fn assert_finished(dir: &Path, body: &[u8]) {
        assert_eq!(std::fs::read(dir.join("big.bin")).unwrap(), body);
        assert!(!part_path(dir).exists(), ".part left behind");
        assert!(!journal_at(dir).exists(), "journal left behind");
        assert!(
            !dir.join("big.bin.part.json.tmp").exists(),
            "journal temporary left behind"
        );
    }

    // -- the grid and the journal --

    #[test]
    fn grid_cuts_the_file_into_fixed_parts() {
        let j = Journal::new(LEN as u64, Some("abc"), PART, 0);
        assert_eq!(j.done, vec![0; 5]);
        let bounds: Vec<_> = (0..5).map(|k| j.bounds(k)).collect();
        assert_eq!(
            bounds,
            vec![
                (0, 1024),
                (1024, 2048),
                (2048, 3072),
                (3072, 4096),
                (4096, LEN as u64)
            ]
        );
        assert_eq!(j.pending(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_size_on_a_boundary_has_no_empty_last_part() {
        let j = Journal::new(4 * PART, None, PART, 0);
        assert_eq!(j.done.len(), 4);
        assert_eq!(j.bounds(3), (3072, 4096));
    }

    #[test]
    fn prefix_maps_onto_the_grid() {
        let j = Journal::new(LEN as u64, None, PART, 2500);
        assert_eq!(j.done, vec![1024, 1024, 452, 0, 0]);
        assert_eq!(j.pending(), vec![2, 3, 4]);
        assert_eq!(j.bytes_done(), 2500);
        assert_eq!(j.contiguous_prefix(), 2500);
        assert_eq!(j.furthest(), 2500);
    }

    #[test]
    fn a_whole_file_prefix_leaves_nothing_pending() {
        let j = Journal::new(LEN as u64, None, PART, LEN as u64);
        assert!(j.pending().is_empty());
        assert_eq!(j.contiguous_prefix(), LEN as u64);
    }

    #[test]
    fn contiguous_prefix_stops_at_the_first_incomplete_part() {
        let mut j = Journal::new(LEN as u64, None, PART, 0);
        j.done = vec![1024, 100, 1024, 0, 500];
        assert_eq!(j.contiguous_prefix(), 1124);
        assert_eq!(j.bytes_done(), 2648);
        assert_eq!(j.furthest(), LEN as u64);
        assert_eq!(j.pending(), vec![1, 3]);
    }

    #[test]
    fn journal_format_is_pinned() {
        let j = Journal::new(LEN as u64, Some("abc"), PART, 1100);
        assert_eq!(
            serde_json::to_string(&j).unwrap(),
            r#"{"version":1,"size":4596,"md5":"abc","part_size":1024,"done":[1024,76,0,0,0]}"#
        );
        let none = Journal::new(LEN as u64, None, PART, 0);
        assert!(serde_json::to_string(&none)
            .unwrap()
            .contains(r#""md5":null"#));
    }

    #[test]
    fn journal_check_accepts_its_own_file() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let j = Journal::new(LEN as u64, file.md5.as_deref(), PART, 0);
        assert_eq!(j.check(&file), Ok(()));
    }

    #[test]
    fn journal_check_rejects_what_does_not_fit() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let good = Journal::new(LEN as u64, file.md5.as_deref(), PART, 0);
        let cases: Vec<(&str, Journal)> = vec![
            (
                "version",
                Journal {
                    version: 2,
                    ..good.clone()
                },
            ),
            (
                "size",
                Journal {
                    size: LEN as u64 + 1,
                    ..good.clone()
                },
            ),
            (
                "md5",
                Journal {
                    md5: Some("other".into()),
                    ..good.clone()
                },
            ),
            (
                "missing md5",
                Journal {
                    md5: None,
                    ..good.clone()
                },
            ),
            (
                "part size",
                Journal {
                    part_size: 0,
                    ..good.clone()
                },
            ),
            (
                "another part size",
                Journal {
                    part_size: 2 * PART,
                    done: vec![0; 3],
                    ..good.clone()
                },
            ),
            (
                "part count",
                Journal {
                    done: vec![0; 4],
                    ..good.clone()
                },
            ),
            (
                "done past its part",
                Journal {
                    done: vec![1025, 0, 0, 0, 0],
                    ..good.clone()
                },
            ),
            (
                "done past the last part",
                Journal {
                    done: vec![0, 0, 0, 0, 501],
                    ..good.clone()
                },
            ),
        ];
        for (what, j) in cases {
            assert!(j.check(&file).is_err(), "{what} was accepted");
        }
        let mut no_size = file.clone();
        no_size.size = None;
        assert!(
            good.check(&no_size).is_err(),
            "a file without a size was accepted"
        );
    }

    #[tokio::test]
    async fn write_journal_replaces_atomically_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.part.json");
        let mut j = Journal::new(LEN as u64, None, PART, 0);
        write_journal(&path, &j).await.unwrap();
        j.done[0] = 1024;
        write_journal(&path, &j).await.unwrap();
        let back: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, j);
        assert!(!dir.path().join("f.part.json.tmp").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_journal_replaces_a_link_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "untouched").unwrap();
        let path = dir.path().join("f.part.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        // A link planted at the temporary's path is not written through.
        let tmp = dir.path().join("f.part.json.tmp");
        let tmp_target = dir.path().join("tmp-target");
        std::fs::write(&tmp_target, "untouched").unwrap();
        std::os::unix::fs::symlink(&tmp_target, &tmp).unwrap();

        write_journal(&path, &Journal::new(LEN as u64, None, PART, 0))
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(&tmp_target).unwrap(), "untouched");
        assert!(!std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn applies_only_to_large_files_with_two_or_more_streams() {
        let _layout = small_layout();
        let big = file_meta("big.bin", &body_of(MIN as usize));
        let small = file_meta("small.bin", &body_of(MIN as usize - 1));
        let mut no_size = big.clone();
        no_size.size = None;
        let files_xml = file_meta("big-item_files.xml", &body_of(MIN as usize));

        assert!(applies(&opts(4), "big-item", &big));
        assert!(applies(&opts(2), "big-item", &big));
        assert!(!applies(&opts(1), "big-item", &big));
        assert!(!applies(&opts(0), "big-item", &big));
        assert!(!applies(&opts(4), "big-item", &small));
        assert!(!applies(&opts(4), "big-item", &no_size));
        assert!(!applies(&opts(4), "big-item", &files_xml));
    }

    #[test]
    fn default_streams_is_four_and_the_ceiling_eight() {
        assert_eq!(DownloadOpts::default().streams, 4);
        assert_eq!(DEFAULT_STREAMS, 4);
        assert_eq!(MAX_STREAMS, 8);
    }

    // -- choosing the path --

    #[tokio::test]
    async fn large_file_is_fetched_in_closed_ranges() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let result = fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, LEN as u64);
        assert_finished(dir.path(), &body);
        let got = ranges(&server).await;
        // The first request goes alone, for the first part.
        assert_eq!(got[0], closed(0, 1023));
        assert_eq!(sorted(got), all_parts());
    }

    #[tokio::test]
    async fn the_mtime_comes_from_the_first_response() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (_server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        let mtime = std::fs::metadata(dir.path().join("big.bin"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime, 1_704_067_200, "Last-Modified of the 206");
    }

    #[tokio::test]
    async fn one_stream_sends_one_plain_get() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(1)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![None]);
    }

    #[tokio::test]
    async fn a_file_below_the_threshold_sends_one_plain_get() {
        let _layout = small_layout();
        let body = body_of(MIN as usize - 1);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(8)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![None]);
    }

    #[tokio::test]
    async fn a_200_to_the_first_range_falls_back_to_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let mut responder = RangeServer::new(&body);
        responder.ignore_range = true;
        let (server, client) = serve(responder).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let result = fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![closed(0, 1023), None]);
    }

    #[tokio::test]
    async fn a_206_for_another_range_falls_back_to_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let wrong = ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes 0-99/{LEN}"))
            .set_body_bytes(body[..100].to_vec());
        let (server, client) = serve(RangeServer::new(&body).script(0, 1, wrong)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![closed(0, 1023), None]);
    }

    #[tokio::test]
    async fn a_206_without_content_range_falls_back_to_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let bare = ResponseTemplate::new(206).set_body_bytes(body[..1024].to_vec());
        let (server, client) = serve(RangeServer::new(&body).script(0, 1, bare)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![closed(0, 1023), None]);
    }

    #[tokio::test]
    async fn content_range_total_mismatch_fails_before_writing() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let mut responder = RangeServer::new(&body);
        responder.total = Some(LEN as u64 + 10);
        let (server, client) = serve(responder).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        assert!(
            matches!(err, IaError::ServerSizeMismatch { server_size, .. } if server_size == LEN as u64 + 10),
            "{err:?}"
        );
        assert!(!part_path(dir.path()).exists());
        assert!(!journal_at(dir.path()).exists());
        assert_eq!(ranges(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn an_error_on_the_first_request_is_the_files() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let busy = ResponseTemplate::new(503).insert_header("retry-after", "7");
        let (server, client) = serve(RangeServer::new(&body).script(0, 1, busy)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        // The per-file retry loop sees the status and the Retry-After.
        assert!(
            matches!(
                err,
                IaError::Http {
                    status: 503,
                    retry_after: Some(7),
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(ranges(&server).await.len(), 1);
        assert!(!journal_at(dir.path()).exists());
    }

    #[tokio::test]
    async fn count_views_leaves_cnt_off_the_first_request_only() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.count_views = true;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let queries: Vec<Option<String>> = requests
            .iter()
            .map(|r| r.url.query().map(str::to_string))
            .collect();
        assert_eq!(queries[0], None, "the first request counts the view");
        assert_eq!(queries.len(), 5);
        assert!(
            queries[1..].iter().all(|q| q.as_deref() == Some("cnt=0")),
            "{queries:?}"
        );
    }

    #[tokio::test]
    async fn without_count_views_every_request_carries_cnt_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.url.query() == Some("cnt=0")));
    }

    #[tokio::test]
    async fn progress_only_grows_and_ends_at_the_size() {
        // Parts of 64 KiB in a 2 MiB file: enough bytes for several
        // progress steps of 256 KiB.
        let _layout = LayoutOverride::new(64 * 1024, 256 * 1024, 16 * 1024);
        let body = body_of(2 * 1024 * 1024);
        let (_server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let seen: Arc<Mutex<Vec<(u64, DownloadStatus)>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let progress = move |p: DownloadProgress| {
            sink.lock().unwrap().push((p.bytes_downloaded, p.status));
        };

        tokio::time::timeout(
            LIMIT,
            download_file(
                &client,
                "big-item",
                &file,
                dir.path(),
                &opts(4),
                Some(&progress),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        let seen = seen.lock().unwrap();
        let downloading: Vec<u64> = seen
            .iter()
            .filter(|(_, s)| *s == DownloadStatus::Downloading)
            .map(|(b, _)| *b)
            .collect();
        assert!(!downloading.is_empty());
        assert!(
            downloading.windows(2).all(|w| w[0] <= w[1]),
            "{downloading:?}"
        );
        assert!(*downloading.last().unwrap() <= body.len() as u64);
        assert_eq!(seen.first().unwrap().1, DownloadStatus::Starting);
        assert_eq!(
            seen.last().unwrap(),
            &(body.len() as u64, DownloadStatus::Complete)
        );
        assert!(seen.iter().any(|(_, s)| *s == DownloadStatus::Verifying));
    }

    // -- resume --

    #[tokio::test]
    async fn resume_fetches_only_the_unfinished_parts() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 300, 0, 500];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        let (server, client) = serve(RangeServer::new(&body)).await;

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(
            sorted(ranges(&server).await),
            vec![closed(2348, 3071), closed(3072, 4095)]
        );
    }

    #[tokio::test]
    async fn a_complete_journal_finishes_without_a_request() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 1024, 1024, 500];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        let (server, client) = serve(RangeServer::new(&body)).await;

        let result = fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_finished(dir.path(), &body);
        assert!(ranges(&server).await.is_empty());
    }

    async fn assert_restarts_from_zero(dir: &Path, body: &[u8], file: &FileMetadata) {
        let (server, client) = serve(RangeServer::new(body)).await;
        fetch(&client, file, dir, &opts(4)).await.unwrap();
        assert_finished(dir, body);
        assert_eq!(sorted(ranges(&server).await), all_parts());
    }

    #[tokio::test]
    async fn a_journal_for_another_md5_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 300, 0, 500];
        // Bytes of an older version of the file, under its md5.
        let old = vec![0xEEu8; LEN];
        seed(
            dir.path(),
            &old,
            &done,
            &journal_json(&old, &md5_hex(&old), &done),
        );

        assert_restarts_from_zero(dir.path(), &body, &file).await;
    }

    #[tokio::test]
    async fn a_corrupt_journal_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), &body, &[1024, 1024], "{\"version\":1,\"size\":");

        assert_restarts_from_zero(dir.path(), &body, &file).await;
    }

    #[tokio::test]
    async fn a_journal_claiming_more_than_the_part_holds_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 1024, 0, 0];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        // The .part lost its tail.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(part_path(dir.path()))
            .unwrap();
        f.set_len(2000).unwrap();

        assert_restarts_from_zero(dir.path(), &body, &file).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_journal_that_is_a_link_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 300, 0, 500];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        let elsewhere = dir.path().join("elsewhere.json");
        std::fs::rename(journal_at(dir.path()), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, journal_at(dir.path())).unwrap();

        assert_restarts_from_zero(dir.path(), &body, &file).await;
        assert!(elsewhere.exists(), "the link's target was not touched");
    }

    #[tokio::test]
    async fn a_part_without_a_journal_is_adopted_as_a_prefix() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(part_path(dir.path()), &body[..2500]).unwrap();
        let (server, client) = serve(RangeServer::new(&body)).await;

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(
            sorted(ranges(&server).await),
            vec![closed(2500, 3071), closed(3072, 4095), closed(4096, 4595)]
        );
    }

    #[tokio::test]
    async fn a_part_longer_than_the_file_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(part_path(dir.path()), vec![1u8; LEN + 1]).unwrap();

        assert_restarts_from_zero(dir.path(), &body, &file).await;
    }

    #[tokio::test]
    async fn the_journal_exists_before_the_first_byte_is_written() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        // Every range gets its head and then no body, except the second
        // part, which fails at once: no byte is ever written, so the only
        // journal on disk is the one written before the first byte.
        let handler: RawHandler = Arc::new(|mut conn| {
            Box::pin(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if conn.first == 1024 {
                    let _ = conn
                        .stream
                        .write_all(
                            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                send_head(&mut conn).await;
                let mut sink = [0u8; 64];
                let _ = tokio::time::timeout(LIMIT, conn.stream.read(&mut sink)).await;
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(4);
        o.min_speed = 0;

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(matches!(err, IaError::Http { status: 404, .. }), "{err:?}");
        let j = read_journal(dir.path());
        assert_eq!(j.size, LEN as u64);
        assert_eq!(j.md5, file.md5);
        assert_eq!(j.part_size, PART);
        assert_eq!(j.done, vec![0; 5]);
    }

    #[tokio::test]
    async fn a_part_longer_than_the_file_beside_a_valid_journal_restarts_from_zero() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 0, 0, 0, 0];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(part_path(dir.path()))
            .unwrap();
        f.set_len(LEN as u64 + 5).unwrap();

        assert_restarts_from_zero(dir.path(), &body, &file).await;
    }

    #[tokio::test]
    async fn a_file_named_like_a_journal_without_a_part_is_left_alone_on_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        // An item file that happens to have the journal's name.
        std::fs::write(journal_at(dir.path()), "an item's own file").unwrap();
        let (_server, client) = serve(RangeServer::new(&body)).await;

        fetch(&client, &file, dir.path(), &opts(1)).await.unwrap();

        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), body);
        assert_eq!(
            std::fs::read_to_string(journal_at(dir.path())).unwrap(),
            "an item's own file"
        );
    }

    /// The state a crash leaves between the two steps of settling: the
    /// journal already rewritten to the contiguous prefix (1524 bytes), the
    /// `.part` not yet truncated and holding junk past it.
    fn crashed_while_settling(dir: &Path, body: &[u8], file: &FileMetadata) {
        let mut part = body[..1524].to_vec();
        part.extend(vec![0xAB; 3072 - 1524]);
        std::fs::write(part_path(dir), part).unwrap();
        std::fs::write(
            journal_at(dir),
            journal_json(body, file.md5.as_deref().unwrap(), &[1024, 500, 0, 0, 0]),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_crash_while_settling_resumes_correctly_on_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        crashed_while_settling(dir.path(), &body, &file);
        let (server, client) = serve(RangeServer::new(&body)).await;
        // Unverified, so an md5 check cannot hide junk left in the file.
        let mut o = opts(1);
        o.verify = false;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![Some("bytes=1524-".to_string())]);
    }

    #[tokio::test]
    async fn a_crash_while_settling_resumes_correctly_over_parallel_ranges() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        crashed_while_settling(dir.path(), &body, &file);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let mut o = opts(4);
        o.verify = false;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(
            sorted(ranges(&server).await),
            sorted(vec![
                closed(1524, 2047),
                closed(2048, 3071),
                closed(3072, 4095),
                closed(4096, 4595)
            ])
        );
    }

    // -- the single-stream path and a journal --

    #[tokio::test]
    async fn one_stream_settles_a_journal_to_its_prefix() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 500, 1024, 0, 0];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        let (server, client) = serve(RangeServer::new(&body)).await;

        fetch(&client, &file, dir.path(), &opts(1)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![Some("bytes=1524-".to_string())]);
    }

    #[tokio::test]
    async fn settling_rewrites_the_journal_before_truncating() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 500, 1024, 0, 0];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );

        settle_for_single_stream(&file, &part_path(dir.path()))
            .await
            .unwrap();

        assert!(!journal_at(dir.path()).exists());
        assert_eq!(std::fs::read(part_path(dir.path())).unwrap(), &body[..1524]);
    }

    #[tokio::test]
    async fn one_stream_discards_an_invalid_journal_and_its_part() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), &body, &[1024, 1024], "not json");
        let (server, client) = serve(RangeServer::new(&body)).await;

        fetch(&client, &file, dir.path(), &opts(1)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![None]);
    }

    #[tokio::test]
    async fn a_200_fallback_settles_the_journal_and_resumes_its_prefix() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let done = [1024, 1024, 0, 1024, 0];
        seed(
            dir.path(),
            &body,
            &done,
            &journal_json(&body, file.md5.as_deref().unwrap(), &done),
        );
        // The server stopped serving ranges since the last run: the first
        // request gets a 200, and so does the single stream's resume, which
        // then starts over from byte 0.
        let mut responder = RangeServer::new(&body);
        responder.ignore_range = true;
        let (server, client) = serve(responder).await;

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(
            ranges(&server).await,
            vec![closed(2048, 3071), Some("bytes=2048-".to_string())]
        );
    }

    // -- per-stream rules --

    fn busy(retry_after: &str) -> ResponseTemplate {
        ResponseTemplate::new(503).insert_header("retry-after", retry_after)
    }

    #[tokio::test]
    async fn a_503_on_one_range_is_retried_by_its_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body).script(2048, 2, busy("0"))).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        let got = ranges(&server).await;
        assert_eq!(
            got.iter().filter(|r| **r == closed(2048, 3071)).count(),
            3,
            "{got:?}"
        );
    }

    #[tokio::test]
    async fn retry_after_on_one_range_delays_that_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body).script(2048, 1, busy("2"))).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let started = std::time::Instant::now();

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "Retry-After was not waited"
        );
        assert_finished(dir.path(), &body);
        // The other streams went on while this one waited: the retried
        // request is the last to arrive.
        let got = ranges(&server).await;
        assert_eq!(got.len(), 6);
        assert_eq!(got.last().unwrap(), &closed(2048, 3071), "{got:?}");
    }

    #[tokio::test]
    async fn a_streams_budget_lasts_across_the_parts_it_takes() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // Four parts fail once each; two streams with one retry apiece
        // cannot absorb four failures, so one stream spends its budget on
        // its second part even though no part failed twice.
        let mut responder = RangeServer::new(&body);
        for first in [1024, 2048, 3072, 4096] {
            responder = responder.script(first, 1, busy("0"));
        }
        let (_server, client) = serve(responder).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(2);
        o.retries = 1;

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(
            matches!(err, IaError::RangeFailed { attempts: 2, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn count_views_counts_once_when_the_first_range_falls_back() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let mut responder = RangeServer::new(&body);
        responder.ignore_range = true;
        let (server, client) = serve(responder).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.count_views = true;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        let queries: Vec<Option<String>> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.query().map(str::to_string))
            .collect();
        assert_eq!(queries, vec![None, Some("cnt=0".to_string())]);
    }

    fn item_with(file: FileMetadata) -> crate::types::ItemMetadata {
        serde_json::from_value(serde_json::json!({
            "metadata": {"identifier": "big-item"},
            "files": [{
                "name": file.name,
                "source": "original",
                "size": file.size.unwrap().to_string(),
                "md5": file.md5.unwrap(),
                "mtime": "1700000000",
            }],
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_spent_stream_budget_fails_with_the_range_and_keeps_the_progress() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let (server, client) = serve(RangeServer::new(&body).script(2048, 100, busy("0"))).await;
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(4);
        o.destdir = dir.path().to_path_buf();
        o.no_directories = true;

        let item = download_item_with_metadata(
            &client,
            "big-item",
            &item_with(file.clone()),
            &o,
            Arc::new(tokio::sync::Semaphore::new(1)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(item.files_failed, 1);
        let DownloadStatus::Failed(message) = &item.results[0].status else {
            panic!("{:?}", item.results[0].status);
        };
        assert!(
            message.contains("failed on bytes 2048-3071 after 3 attempts"),
            "{message}"
        );
        // Two retries for the stream, and no second round from the
        // per-file loop.
        let got = ranges(&server).await;
        assert_eq!(
            got.iter().filter(|r| **r == closed(2048, 3071)).count(),
            3,
            "{got:?}"
        );
        // The failed part has nothing on record; the other streams were
        // stopped with whatever they had received.
        let journal = read_journal(dir.path());
        assert_eq!(journal.done[2], 0);
        let missing: Vec<Option<String>> = journal
            .pending()
            .into_iter()
            .map(|k| {
                let (first, last) = journal.span(k);
                closed(first, last)
            })
            .collect();
        assert!(missing.contains(&closed(2048, 3071)));

        // A later run fetches only what the journal lacks.
        let (server, client) = serve(RangeServer::new(&body)).await;
        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();
        assert_finished(dir.path(), &body);
        assert_eq!(sorted(ranges(&server).await), sorted(missing));
    }

    #[tokio::test]
    async fn range_failed_carries_the_range_and_the_last_error() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (_server, client) = serve(RangeServer::new(&body).script(3072, 100, busy("0"))).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        let IaError::RangeFailed {
            file: name,
            start,
            end,
            attempts,
            source,
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(
            (name.as_str(), *start, *end, *attempts),
            ("big.bin", 3072, 4095, 3)
        );
        assert!(
            matches!(**source, IaError::Http { status: 503, .. }),
            "{source:?}"
        );
        assert!(!err.is_retryable());
        assert!(part_path(dir.path()).exists());
        assert!(journal_at(dir.path()).exists());
    }

    #[tokio::test]
    async fn a_404_on_one_range_fails_at_once() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) =
            serve(RangeServer::new(&body).script(2048, 100, ResponseTemplate::new(404))).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        assert!(matches!(err, IaError::Http { status: 404, .. }), "{err:?}");
        let got = ranges(&server).await;
        assert_eq!(got.iter().filter(|r| **r == closed(2048, 3071)).count(), 1);
    }

    #[tokio::test]
    async fn a_200_to_a_later_range_is_a_failed_request() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let whole = ResponseTemplate::new(200).set_body_bytes(body.clone());
        let (server, client) = serve(RangeServer::new(&body).script(3072, 1, whole)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        let got = ranges(&server).await;
        assert_eq!(got.iter().filter(|r| **r == closed(3072, 4095)).count(), 2);
    }

    #[tokio::test]
    async fn a_206_for_the_wrong_range_on_a_later_request_is_a_failed_request() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let wrong = ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes 0-1023/{LEN}"))
            .set_body_bytes(body[..1024].to_vec());
        let (server, client) = serve(RangeServer::new(&body).script(3072, 1, wrong)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        let got = ranges(&server).await;
        assert_eq!(got.iter().filter(|r| **r == closed(3072, 4095)).count(), 2);
    }

    #[tokio::test]
    async fn a_wrong_total_on_a_later_range_fails_permanently() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let wrong = ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes 3072-4095/{}", LEN + 1))
            .set_body_bytes(body[3072..4096].to_vec());
        let (_server, client) = serve(RangeServer::new(&body).script(3072, 100, wrong)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        assert!(matches!(err, IaError::ServerSizeMismatch { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn md5_mismatch_after_assembly_keeps_the_bytes_and_drops_the_journal() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let mut file = file_meta("big.bin", &body);
        file.md5 = Some(md5_hex(b"something else"));
        let (_server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        assert!(matches!(err, IaError::ChecksumMismatch { .. }), "{err:?}");
        assert_eq!(
            std::fs::read(dir.path().join("big.bin.md5-mismatch")).unwrap(),
            body
        );
        assert!(!part_path(dir.path()).exists());
        assert!(!journal_at(dir.path()).exists());
    }

    #[tokio::test]
    async fn without_verify_the_assembled_file_is_not_hashed() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let mut file = file_meta("big.bin", &body);
        file.md5 = Some(md5_hex(b"something else"));
        let (_server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(4);
        o.verify = false;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
    }

    // -- raw loopback server: concurrency, dropped bodies, stalls --

    /// One request on the raw server.
    struct RawConn {
        stream: tokio::net::TcpStream,
        /// The first and last byte asked for; the whole body without Range.
        first: u64,
        last: u64,
        ranged: bool,
        /// How many earlier requests asked for a range starting at `first`.
        nth: usize,
        body: Arc<Vec<u8>>,
    }

    type RawHandler = Arc<dyn Fn(RawConn) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

    async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        let mut acc = Vec::new();
        loop {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&acc).to_string()
    }

    /// Accept connections until the test ends, one request each, and hand
    /// each to `handler`. Returns the client and every request's `Range`.
    async fn spawn_raw(
        body: &[u8],
        handler: RawHandler,
    ) -> (IaClient, Arc<Mutex<Vec<Option<String>>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = Arc::new(body.to_vec());
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let counts: Arc<Mutex<HashMap<u64, usize>>> = Arc::default();
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let head = read_head(&mut stream).await;
                let range = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("range")
                        .then(|| value.trim().to_string())
                });
                log.lock().unwrap().push(range.clone());
                let len = body.len() as u64;
                let (first, last, ranged) = match range.as_deref().and_then(parse_range) {
                    Some((a, b)) => (a, b.unwrap_or(len - 1), true),
                    None => (0, len - 1, false),
                };
                let nth = {
                    let mut counts = counts.lock().unwrap();
                    let n = counts.entry(first).or_insert(0);
                    *n += 1;
                    *n - 1
                };
                let conn = RawConn {
                    stream,
                    first,
                    last,
                    ranged,
                    nth,
                    body: Arc::clone(&body),
                };
                tokio::spawn(handler(conn));
            }
        });
        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        (IaClient::from_config(config).unwrap(), seen)
    }

    /// Write the head for `conn`'s range, promising all of it.
    async fn send_head(conn: &mut RawConn) {
        use tokio::io::AsyncWriteExt;
        let len = conn.body.len();
        let head = if conn.ranged {
            format!(
                "HTTP/1.1 206 Partial Content\r\n\
                 content-length: {}\r\n\
                 content-range: bytes {}-{}/{len}\r\n\
                 connection: close\r\n\r\n",
                conn.last - conn.first + 1,
                conn.first,
                conn.last,
            )
        } else {
            format!("HTTP/1.1 200 OK\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n")
        };
        conn.stream.write_all(head.as_bytes()).await.unwrap();
    }

    fn slice(conn: &RawConn) -> Vec<u8> {
        conn.body[conn.first as usize..=conn.last as usize].to_vec()
    }

    async fn serve_whole(mut conn: RawConn) {
        use tokio::io::AsyncWriteExt;
        send_head(&mut conn).await;
        let bytes = slice(&conn);
        let _ = conn.stream.write_all(&bytes).await;
        let _ = conn.stream.flush().await;
    }

    #[tokio::test]
    async fn the_streams_run_at_once_on_separate_connections() {
        let _layout = small_layout();
        let body = body_of(4 * PART as usize);
        // The three requests after the first are held until all three are
        // open at once; one connection at a time would never get there.
        let gate = Arc::new(tokio::sync::Barrier::new(3));
        let handler: RawHandler = Arc::new(move |conn| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                if conn.first != 0 {
                    gate.wait().await;
                }
                serve_whole(conn).await;
            })
        });
        let (client, seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        let result = tokio::time::timeout(
            Duration::from_secs(20),
            download_file(&client, "big-item", &file, dir.path(), &opts(4), None),
        )
        .await
        .expect("the streams did not run at once")
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_finished(dir.path(), &body);
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    /// Serve every request after holding it open for a moment, and return
    /// the most connections that were open at once.
    async fn count_concurrency(body: &[u8], streams: usize) -> usize {
        use std::sync::atomic::AtomicUsize;
        let open = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let (o, m) = (Arc::clone(&open), Arc::clone(&most));
        let handler: RawHandler = Arc::new(move |conn| {
            let (open, most) = (Arc::clone(&o), Arc::clone(&m));
            Box::pin(async move {
                let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(300)).await;
                serve_whole(conn).await;
                open.fetch_sub(1, Ordering::SeqCst);
            })
        });
        let (client, _seen) = spawn_raw(body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", body);
        fetch(&client, &file, dir.path(), &opts(streams))
            .await
            .unwrap();
        assert_finished(dir.path(), body);
        most.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn streams_above_the_ceiling_use_the_ceiling() {
        let _layout = small_layout();
        // Twelve parts, so more than eight streams could start.
        let body = body_of(12 * PART as usize);
        assert_eq!(count_concurrency(&body, 50).await, MAX_STREAMS);
    }

    #[tokio::test]
    async fn the_stream_count_is_the_most_connections_open_at_once() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        assert_eq!(count_concurrency(&body, 3).await, 3);
    }

    #[tokio::test]
    async fn a_dropped_body_re_requests_the_rest_of_the_part() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let handler: RawHandler = Arc::new(|mut conn| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                if conn.first == 1024 && conn.nth == 0 {
                    // Promise the whole part, send 100 bytes, hang up.
                    send_head(&mut conn).await;
                    let bytes = slice(&conn);
                    let _ = conn.stream.write_all(&bytes[..100]).await;
                    let _ = conn.stream.flush().await;
                    return;
                }
                serve_whole(conn).await;
            })
        });
        let (client, seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        let seen = seen.lock().unwrap().clone();
        assert!(seen.contains(&closed(1124, 2047)), "{seen:?}");
    }

    #[tokio::test]
    async fn a_dropped_body_spends_the_stream_error_budget_then_a_request() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // The part at 1024 drops after 10 bytes on every request: three
        // re-requests (0.5 s, 1.5 s, 4.5 s), then one failed request, and
        // with retries 0 that fails the file.
        let handler: RawHandler = Arc::new(|mut conn| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                if (1024..2048).contains(&conn.first) {
                    send_head(&mut conn).await;
                    let bytes = slice(&conn);
                    let _ = conn.stream.write_all(&bytes[..10]).await;
                    let _ = conn.stream.flush().await;
                    return;
                }
                serve_whole(conn).await;
            })
        });
        let (client, seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.retries = 0;

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(
            matches!(
                err,
                IaError::RangeFailed {
                    start: 1024,
                    end: 2047,
                    attempts: 1,
                    ..
                }
            ),
            "{err:?}"
        );
        let seen = seen.lock().unwrap().clone();
        let part_requests: Vec<_> = seen
            .iter()
            .flatten()
            .filter(|r| r.ends_with("-2047"))
            .cloned()
            .collect();
        assert_eq!(
            part_requests,
            vec![
                "bytes=1024-2047",
                "bytes=1034-2047",
                "bytes=1044-2047",
                "bytes=1054-2047"
            ],
        );
        assert_eq!(read_journal(dir.path()).done[1], 40);
    }

    const DRIP: Duration = Duration::from_millis(250);

    /// One byte of the range every [`DRIP`], until the client hangs up.
    async fn drip(mut conn: RawConn) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        send_head(&mut conn).await;
        let bytes = slice(&conn);
        let (mut rd, mut wr) = conn.stream.into_split();
        let mut sink = [0u8; 64];
        for b in bytes {
            tokio::select! {
                _ = tokio::time::sleep(DRIP) => {
                    if wr.write_all(&[b]).await.is_err() || wr.flush().await.is_err() {
                        return;
                    }
                }
                closed = rd.read(&mut sink) => {
                    if matches!(closed, Ok(0) | Err(_)) {
                        return;
                    }
                }
            }
        }
    }

    fn shrink_stall_policy() -> stall::PolicyOverride {
        stall::PolicyOverride::new(Duration::from_secs(2), Duration::from_secs(1))
    }

    #[tokio::test]
    async fn a_stall_on_one_stream_re_requests_the_rest_of_its_part() {
        let _layout = small_layout();
        let _policy = shrink_stall_policy();
        let body = body_of(LEN);
        let handler: RawHandler = Arc::new(|conn| {
            Box::pin(async move {
                if conn.first == 1024 && conn.nth == 0 {
                    drip(conn).await;
                } else {
                    serve_whole(conn).await;
                }
            })
        });
        let (client, seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &opts(4)).await.unwrap();

        assert_finished(dir.path(), &body);
        let seen = seen.lock().unwrap().clone();
        let resumed: Vec<u64> = seen
            .iter()
            .flatten()
            .filter_map(|r| parse_range(r))
            .filter(|&(a, b)| (1025..2048).contains(&a) && b == Some(2047))
            .map(|(a, _)| a)
            .collect();
        assert_eq!(resumed.len(), 1, "{seen:?}");
        assert!(resumed[0] <= 1024 + 12, "resumed from {}", resumed[0]);
    }

    #[tokio::test]
    async fn a_spent_stall_budget_fails_with_download_stalled() {
        let _layout = small_layout();
        let _policy = shrink_stall_policy();
        let body = body_of(LEN);
        let handler: RawHandler = Arc::new(|conn| {
            Box::pin(async move {
                if (1024..2048).contains(&conn.first) {
                    drip(conn).await;
                } else {
                    serve_whole(conn).await;
                }
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.retries = 1;

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(
            matches!(err, IaError::DownloadStalled { stalls: 2, .. }),
            "{err:?}"
        );
        assert!(!err.is_retryable());
        let j = read_journal(dir.path());
        assert!(j.done[1] > 0 && j.done[1] < 1024, "{:?}", j.done);
        assert_eq!(j.done[0], 1024);
    }

    #[tokio::test]
    async fn a_body_past_the_end_of_its_part_is_never_written() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // The last part's first answer claims the right range but sends a
        // chunked body 100 bytes longer; written, it would run past the end
        // of the file.
        let handler: RawHandler = Arc::new(|mut conn| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                if conn.first == 4096 && conn.nth == 0 {
                    let mut data = slice(&conn);
                    data.extend(vec![0xEE; 100]);
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\n\
                         content-range: bytes {}-{}/{}\r\n\
                         transfer-encoding: chunked\r\n\
                         connection: close\r\n\r\n{:x}\r\n",
                        conn.first,
                        conn.last,
                        conn.body.len(),
                        data.len()
                    );
                    let _ = conn.stream.write_all(head.as_bytes()).await;
                    let _ = conn.stream.write_all(&data).await;
                    let _ = conn.stream.write_all(b"\r\n0\r\n\r\n").await;
                    let _ = conn.stream.flush().await;
                    return;
                }
                serve_whole(conn).await;
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.verify = false;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_part_that_is_a_link_is_removed_and_its_target_untouched() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, body_of(3000)).unwrap();
        std::os::unix::fs::symlink(&target, part_path(dir.path())).unwrap();

        assert_restarts_from_zero(dir.path(), &body, &file).await;
        assert_eq!(std::fs::read(&target).unwrap(), body_of(3000));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_planted_at_the_part_mid_download_is_never_written_through() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();
        let (part, link_to) = (part_path(dir.path()), target.clone());
        // Before answering for the third part, swap the .part for a link.
        let handler: RawHandler = Arc::new(move |conn| {
            let (part, link_to) = (part.clone(), link_to.clone());
            Box::pin(async move {
                if conn.first == 2048 {
                    // In one step: a link made beside it, renamed over it,
                    // so no stream ever finds the .part missing or regular
                    // between a removal and the link.
                    let staged = part.with_extension("staged-link");
                    std::os::unix::fs::symlink(&link_to, &staged).unwrap();
                    std::fs::rename(&staged, &part).unwrap();
                }
                serve_whole(conn).await;
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        // The stream answered for the third part opens the .part after the
        // link is in place. It, or a stream that saw the link first, removes
        // the link and fails the file; a stream whose open raced the rename
        // finds the path replaced and fails; a stream that opens after the
        // removal finds no .part and fails. Which failure arrives first
        // depends on scheduling; all are right, and none writes through the
        // link.
        assert!(
            matches!(&err, IaError::ResumeFailed { reason, .. }
                if reason.contains("symlink") || reason.contains("replaced"))
                || matches!(&err, IaError::Io(e) if e.kind() == std::io::ErrorKind::NotFound),
            "{err:?}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        // The link is gone and nothing put a .part back.
        assert!(std::fs::symlink_metadata(part_path(dir.path())).is_err());
    }

    #[tokio::test]
    async fn a_part_removed_mid_download_is_not_recreated() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        let part = part_path(dir.path());
        // Before answering for the third part, the .part goes away. A
        // stream that recreated it would leave a file with holes beside a
        // journal that claims the first part.
        let gone = part.clone();
        let handler: RawHandler = Arc::new(move |conn| {
            let gone = gone.clone();
            Box::pin(async move {
                if conn.first == 2048 {
                    let _ = std::fs::remove_file(&gone);
                }
                serve_whole(conn).await;
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;

        let err = fetch(&client, &file, dir.path(), &opts(4))
            .await
            .unwrap_err();

        assert!(
            matches!(&err, IaError::Io(e) if e.kind() == std::io::ErrorKind::NotFound),
            "{err:?}"
        );
        assert!(!part.exists(), "a stream recreated the .part");

        // A later run ignores the journal (it has no .part) and starts over.
        let (server, client) = serve(RangeServer::new(&body)).await;
        let mut o = opts(4);
        o.verify = false;
        fetch(&client, &file, dir.path(), &o).await.unwrap();
        assert_finished(dir.path(), &body);
        assert_eq!(sorted(ranges(&server).await), all_parts());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuse_link_removes_a_link_and_reports_it_once_or_many_times() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", b"x");
        let part = part_path(dir.path());
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();

        // Missing and regular: nothing to refuse.
        refuse_link(&file, &part).await.unwrap();
        std::fs::write(&part, "bytes").unwrap();
        refuse_link(&file, &part).await.unwrap();
        std::fs::remove_file(&part).unwrap();

        // Several streams checking the same link at once: each reports the
        // link, none the missing file the first removal leaves.
        for _ in 0..50 {
            std::os::unix::fs::symlink(&target, &part).unwrap();
            let results =
                futures::future::join_all((0..4).map(|_| refuse_link(&file, &part))).await;
            assert!(
                results.iter().all(|r| match r {
                    Err(IaError::ResumeFailed { reason, .. }) => reason.contains("symlink"),
                    Ok(()) => true,
                    Err(_) => false,
                }),
                "{results:?}"
            );
            assert!(results.iter().any(Result::is_err));
            assert!(std::fs::symlink_metadata(&part).is_err());
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_open_that_followed_a_link_is_refused_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", b"x");
        let part = part_path(dir.path());
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();

        // A regular .part, opened and still at its path: accepted.
        std::fs::write(&part, "bytes").unwrap();
        let opened = fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .unwrap();
        ensure_opened_at_path(&file, &opened, &part).await.unwrap();
        drop(opened);

        // The state the check-then-open race leaves: the path became a link
        // after the check, and the open followed it to the target.
        std::fs::remove_file(&part).unwrap();
        std::os::unix::fs::symlink(&target, &part).unwrap();
        let opened = fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .unwrap();
        let err = ensure_opened_at_path(&file, &opened, &part)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, IaError::ResumeFailed { reason, .. } if reason.contains("symlink")),
            "{err:?}"
        );
        assert!(
            std::fs::symlink_metadata(&part).is_err(),
            "the link was kept"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");

        // The .part was opened, then another regular file put at its path.
        std::fs::write(&part, "first").unwrap();
        let opened = fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, "second").unwrap();
        std::fs::rename(&other, &part).unwrap();
        let err = ensure_opened_at_path(&file, &opened, &part)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, IaError::ResumeFailed { reason, .. } if reason.contains("replaced")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_failing_stream_stops_the_others_with_their_progress_recorded() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // Part 1 trickles one byte every 250 ms and stays below every
        // checkpoint; part 2 fails at once. The trickling stream must be
        // told to stop and record what it has.
        let handler: RawHandler = Arc::new(|mut conn| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                match conn.first {
                    1024 => drip(conn).await,
                    2048 => {
                        tokio::time::sleep(Duration::from_millis(1200)).await;
                        let _ = conn
                            .stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                            .await;
                    }
                    _ => serve_whole(conn).await,
                }
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.min_speed = 0;

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(matches!(err, IaError::Http { status: 404, .. }), "{err:?}");
        let j = read_journal(dir.path());
        assert!(j.done[1] >= 2, "the stopped stream recorded {:?}", j.done);
        assert_eq!(
            std::fs::read(part_path(dir.path())).unwrap()[1024..1024 + j.done[1] as usize],
            body[1024..1024 + j.done[1] as usize]
        );
    }
}
