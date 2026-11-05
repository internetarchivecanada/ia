//! Parallel Range downloads within one file (#16).
//!
//! A file of [`MIN_SIZE`] or more is cut into consecutive [`PART_SIZE`]
//! parts on a fixed grid. Up to [`DownloadOpts::connections_per_file`]
//! streams, each on its own HTTP/1.1 connection, take the next unfetched
//! part from a shared queue and fetch it with one closed `Range` request,
//! writing it at its offset in the `.part` file. Each request holds a slot
//! in [`DownloadOpts::server_slots`] (see the `slots` module). Measured
//! on 2026-10-30, four such connections ran 2.41 times as fast as one, and
//! eight 3.50 times; four ranges multiplexed on one HTTP/2 connection
//! gained nothing (`docs/plans/parallel-transfer-measurement-2026-10-30.md`).
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

/// The most connections one file uses; a larger
/// [`DownloadOpts::connections_per_file`] is treated as this. The
/// measurement tested up to eight connections.
pub const MAX_CONNECTIONS_PER_FILE: usize = 8;

/// The length of every part but the last. rclone's default chunk size for
/// the same feature; at the per-stream rate measured with four streams a
/// part takes about 20 s, so the first-byte wait of its request (a
/// `/download/` redirect, measured at 0.16 to 6.2 s) costs a few percent.
const PART_SIZE: u64 = 64 * 1024 * 1024;

/// The smallest file fetched in parallel: four parts, one for each of the
/// 4 connections the measurement recommended. rclone's default cutoff for
/// the same feature.
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
    /// A file at the journal's path that is not a journal, and why: it is
    /// too large to be one, cannot be read, or does not parse as one. It is
    /// most likely an item file that happens to have the name, so it is
    /// never removed or replaced; the `.part`, if any, is taken as a
    /// contiguous prefix, and the file is fetched on one stream.
    Foreign(String),
}

/// Read and check the journal at `path` for `file`, whose partial download
/// is at `part_path`.
///
/// A regular file there that cannot be one of this code's journals (too
/// large, unreadable, or not parsing as one) is [`Loaded::Foreign`] and is
/// left alone. A journal of this code is checked against the file and the
/// `.part`: besides [`Journal::check`], the `.part` must be a regular file
/// that holds every byte the journal claims and is no longer than the file.
/// A journal with no `.part` beside it describes nothing and is invalid.
async fn load_journal(path: &Path, file: &FileMetadata, part_path: &Path) -> Result<Loaded> {
    let meta = match fs::symlink_metadata(path).await {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::Absent),
        Err(e) => return Ok(Loaded::Foreign(format!("it cannot be examined: {e}"))),
    };
    if meta.file_type().is_symlink() {
        return Ok(Loaded::Invalid("it is a symlink".to_string()));
    }
    if !meta.is_file() {
        return Ok(Loaded::Invalid("it is not a regular file".to_string()));
    }
    if meta.len() > MAX_JOURNAL_BYTES {
        return Ok(Loaded::Foreign(format!("it is {} bytes long", meta.len())));
    }
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) => return Ok(Loaded::Foreign(format!("it cannot be read: {e}"))),
    };
    let journal: Journal = match serde_json::from_slice(&bytes) {
        Ok(journal) => journal,
        Err(e) => return Ok(Loaded::Foreign(format!("it does not parse: {e}"))),
    };
    let held = match fs::symlink_metadata(part_path).await {
        Ok(part) if part.is_file() => part.len(),
        Ok(_) => {
            return Ok(Loaded::Invalid(
                "the .part beside it is not a regular file".to_string(),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded::Invalid("there is no .part beside it".to_string()))
        }
        Err(e) => return Err(e.into()),
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

/// Whether `file` is fetched over parallel ranges: not pinned to one stream,
/// a size in the metadata at or above the threshold, and not the item's own
/// `_files.xml`, whose size is unknowable. Whether it then splits is decided
/// when its first range request is answered (see [`stream_count`]).
pub(super) fn applies(opts: &DownloadOpts, identifier: &str, file: &FileMetadata) -> bool {
    !matches!(opts.connections_per_file, Some(0 | 1))
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
        Loaded::Foreign(why) => {
            debug!(
                file = %file.name,
                path = %path.display(),
                reason = %why,
                "the file at the part journal's path is not a journal; leaving it"
            );
            Ok(())
        }
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
    /// A file that is not a journal sits at the journal's path (see
    /// [`Loaded::Foreign`]); no request was sent, and the caller fetches the
    /// file on one stream.
    JournalPathTaken,
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
    starting: Option<super::slots::Starting>,
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
        Loaded::Foreign(why) => {
            info!(
                file = %file.name,
                path = %journal_file.display(),
                reason = %why,
                "a file that is not a part journal has the journal's name; downloading on one stream"
            );
            return Ok(Outcome::JournalPathTaken);
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
        // The file stops counting as one to come once this request holds a
        // slot, and ends with its answer (see `fetch_response_via`).
        let (response, slot) =
            fetch_range(client, url, file, span, opts.count_views, opts, starting).await?;
        if !answers_span(&response, identifier, file, span)? {
            drop(response);
            drop(slot);
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
        let server = slot.as_ref().map(|guard| guard.server().to_string());
        let streams = stream_count(opts, server.as_deref(), pending.len()).await;
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
            active: std::sync::atomic::AtomicUsize::new(streams),
        };
        run_streams(&shared, streams, first, (response, slot)).await?;
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

/// How many streams a file with `pending` parts runs, decided once, after
/// its first range request was answered on `server` (holding one slot).
///
/// Pinned (`connections_per_file` `Some(n)`): `n`, at most
/// [`MAX_CONNECTIONS_PER_FILE`] and the parts. Automatic (`None`): one plus
/// the room on `server` (the slots free after one for every other file
/// about to start), at most [`MAX_CONNECTIONS_PER_FILE`] and the parts; so a
/// lone large file fills its server's budget, and among other files it
/// takes nothing they are waiting for.
async fn stream_count(opts: &DownloadOpts, server: Option<&str>, pending: usize) -> usize {
    let wanted = match opts.connections_per_file {
        Some(n) => n,
        None => match server {
            Some(server) => 1 + opts.server_slots.room(server).await,
            None => 1,
        },
    };
    wanted.clamp(1, MAX_CONNECTIONS_PER_FILE).min(pending)
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
    opts: &DownloadOpts,
    starting: Option<super::slots::Starting>,
) -> Result<Answer> {
    let (response, slot) = super::fetch_response_via(
        client.range_http(),
        client,
        url,
        Some(super::ByteRange::Span { first, last }),
        count_views,
        Some(opts),
        starting,
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
    Ok((response, slot))
}

/// A range request's response and the per-server slot it holds (see
/// [`ServerSlots`]); the slot goes back when it is dropped.
type Answer = (reqwest::Response, super::Slot);

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
    /// The file's streams still running.
    active: std::sync::atomic::AtomicUsize,
}

impl Shared<'_> {
    fn next_part(&self) -> Option<usize> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    /// Whether the stream holding `slot` should stop now, because its
    /// server's budget fell below the connections in use (see
    /// `SlotGuard::take_shed`). Only while another stream of the file runs,
    /// so a file always keeps one; a stream that stops is no longer counted.
    /// The streams of a file run in one task, so nothing runs between the
    /// count and its update.
    fn sheds(&self, slot: Option<&super::slots::SlotGuard>) -> bool {
        let Some(slot) = slot else {
            return false;
        };
        if self.active.load(Ordering::Relaxed) <= 1 || !slot.take_shed() {
            return false;
        }
        self.active.fetch_sub(1, Ordering::Relaxed);
        true
    }

    /// Put part `k` back at the front of the queue, for a surviving stream.
    fn give_back(&self, k: usize) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_front(k);
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
    answer: Answer,
) -> Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut first = Some((first, answer));
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
    /// The stream stopped to lower its server's connections; its part is
    /// back on the queue.
    Shed,
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
///
/// The stop is checked before `work` on every poll (`biased`). A stream
/// that fails the file gives its server slot back before the stop goes out,
/// which wakes a stream waiting for that slot; polled in random order, that
/// stream could take the slot and open a connection before it saw the stop.
/// Checked first, the stop wins and `work` is dropped without being polled
/// again, so no stream opens a connection once the file has failed.
async fn or_stopped<F: std::future::Future>(
    stop: &mut watch::Receiver<bool>,
    work: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = stopped(stop) => None,
        output = work => Some(output),
    }
}

/// One stream: fetch part `first` from its response, if given, then take
/// parts from the queue until it is empty or the stream is told to stop.
async fn stream(
    shared: &Shared<'_>,
    mut first: Option<(usize, Answer)>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut budget = Budget::new(shared.opts.retries);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let (k, answer) = match first.take() {
            Some((k, answer)) => (k, Some(answer)),
            None => match shared.next_part() {
                Some(k) => (k, None),
                None => {
                    shared.active.fetch_sub(1, Ordering::Relaxed);
                    return Ok(());
                }
            },
        };
        match fetch_part(shared, k, answer, &mut budget, &mut stop).await? {
            Flow::Next => {}
            Flow::Stopped => {
                shared.active.fetch_sub(1, Ordering::Relaxed);
                return Ok(());
            }
            Flow::Shed => return Ok(()),
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
) -> Result<Option<Answer>> {
    loop {
        // Only the file's first request may count a view (see `download`).
        // Waiting for a server slot is part of the request, so a stream
        // told to stop stops waiting.
        let Some(sent) = or_stopped(
            stop,
            fetch_range(
                shared.client,
                shared.url,
                shared.file,
                span,
                false,
                shared.opts,
                None,
            ),
        )
        .await
        else {
            return Ok(None);
        };
        let err = match sent {
            // A response that is not used goes, with its slot, before the
            // stream waits to send the request again.
            Ok((response, slot)) => {
                match answers_span(&response, shared.identifier, shared.file, span) {
                    Ok(true) => return Ok(Some((response, slot))),
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
                }
            }
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
    /// The server's budget fell below the connections in use, and this
    /// stream gives way.
    Shed,
}

/// Fetch the rest of part `k`, starting from `response` when given. Body
/// errors, stalls and failed requests are handled by the stream's rules
/// (see the plan); progress is recorded every checkpoint and whenever a
/// response ends.
async fn fetch_part(
    shared: &Shared<'_>,
    k: usize,
    mut response: Option<Answer>,
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
        let (body_response, slot) = match response.take() {
            Some(answer) => answer,
            None => match request_span(shared, k, span, budget, stop).await? {
                Some(answer) => answer,
                None => return Ok(Flow::Stopped),
            },
        };
        if shared.sheds(slot.as_ref()) {
            drop(body_response);
            drop(slot);
            shared.give_back(k);
            return Ok(Flow::Shed);
        }
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
                        // Never write into the next part: of a chunk that
                        // runs past the end of the range, only the bytes in
                        // it are written, and the stream ends there. With
                        // the part then complete, nothing more is asked.
                        let room = len - done;
                        let n = (chunk.len() as u64).min(room);
                        out.write_all(&chunk[..n as usize]).await?;
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
                        if chunk.len() as u64 > room {
                            break Ended::Error(IaError::ResumeFailed {
                                file: shared.file.name.clone(),
                                reason: format!(
                                    "the server sent more than bytes {}-{}",
                                    span.0, span.1
                                ),
                            });
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
                _ = ticker.tick() => {
                    if shared.sheds(slot.as_ref()) {
                        break Ended::Shed;
                    }
                    if let Some(d) = detector.as_mut() {
                        if let Some(observed) = d.check(tokio::time::Instant::now()) {
                            break Ended::Stalled { observed, window_secs: d.window_secs() };
                        }
                    }
                }
            }
        };
        // Close the connection and give its slot back (before any wait or
        // next request), then put what arrived on record: it is the offset
        // of the next request, or of a later run.
        drop(body);
        drop(slot);
        shared.record(k, &mut out, done).await?;
        drop(out);
        // The part may have arrived in full just as a stall check or a body
        // error won the race with the end of the body: it is done, and
        // nothing is spent on it.
        if done == len && !matches!(ended, Ended::Shed) {
            break;
        }

        let err = match ended {
            Ended::Done => break,
            Ended::Shed => {
                if done < len {
                    shared.give_back(k);
                }
                return Ok(Flow::Shed);
            }
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
    use crate::download::{download_file, download_item_with_metadata, ServerSlots};
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

    /// `connections` per file, two retries, and room for every connection
    /// a test opens unless the test sets the per-server cap itself.
    fn opts(connections: usize) -> DownloadOpts {
        DownloadOpts {
            connections_per_file: Some(connections),
            retries: 2,
            server_slots: ServerSlots::with_cap(64),
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
    fn the_split_is_automatic_by_default_eight_at_most_and_six_per_server() {
        let defaults = DownloadOpts::default();
        assert_eq!(defaults.connections_per_file, None);
        assert_eq!(MAX_CONNECTIONS_PER_FILE, 8);
        assert_eq!(defaults.server_slots.max(), 6);
        assert_eq!(crate::download::MAX_CONNECTIONS_PER_SERVER, 6);
    }

    #[tokio::test]
    async fn the_default_options_split_a_lone_large_file() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(sorted(ranges(&server).await), all_parts());
    }

    #[tokio::test]
    async fn the_hidden_pin_of_one_never_splits() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (server, client) = serve(RangeServer::new(&body)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let o = DownloadOpts {
            connections_per_file: Some(1),
            ..Default::default()
        };

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
        assert_eq!(ranges(&server).await, vec![None]);
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

    /// Run a download over parallel ranges and one over a single stream on a
    /// directory holding an item file at the journal's path, and return the
    /// `Range` of every request. The item file must come through untouched
    /// and the download must finish.
    async fn download_beside_an_item_file(
        dir: &Path,
        body: &[u8],
        file: &FileMetadata,
        item_file: &str,
        streams: usize,
    ) -> Vec<Option<String>> {
        let (server, client) = serve(RangeServer::new(body)).await;
        fetch(&client, file, dir, &opts(streams)).await.unwrap();
        assert_eq!(std::fs::read(dir.join("big.bin")).unwrap(), body);
        assert!(!part_path(dir).exists());
        assert_eq!(
            std::fs::read_to_string(journal_at(dir)).unwrap(),
            item_file,
            "the item file at the journal's path was changed"
        );
        ranges(&server).await
    }

    #[tokio::test]
    async fn an_interrupted_file_beside_an_item_file_named_like_its_journal_resumes_on_one_stream()
    {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        // The item holds big.bin and big.bin.part.json; the second was
        // downloaded, the first interrupted after 2500 bytes on one stream.
        let item_file = r#"{"an item's own":"json"}"#;
        for streams in [4, 1] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(journal_at(dir.path()), item_file).unwrap();
            std::fs::write(part_path(dir.path()), &body[..2500]).unwrap();

            let got =
                download_beside_an_item_file(dir.path(), &body, &file, item_file, streams).await;

            // No range request at all: the parallel path does not run.
            assert_eq!(
                got,
                vec![Some("bytes=2500-".to_string())],
                "streams {streams}"
            );
        }
    }

    #[tokio::test]
    async fn a_fresh_file_beside_an_item_file_named_like_its_journal_goes_on_one_stream() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let file = file_meta("big.bin", &body);
        let dir = tempfile::tempdir().unwrap();
        // Not JSON at all, and a truncated journal-like text: neither is
        // this code's journal.
        for item_file in ["plain text", "{\"version\":1,\"size\":"] {
            std::fs::write(journal_at(dir.path()), item_file).unwrap();
            let _ = std::fs::remove_file(dir.path().join("big.bin"));

            let got = download_beside_an_item_file(dir.path(), &body, &file, item_file, 4).await;

            assert_eq!(got, vec![None], "{item_file}");
        }
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
        // A journal of this code, written for another version of the file.
        let old = vec![0xEEu8; LEN];
        seed(
            dir.path(),
            &old,
            &[1024, 1024],
            &journal_json(&old, &md5_hex(&old), &[1024, 1024, 0, 0, 0]),
        );
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
    async fn a_body_one_byte_past_its_range_completes_the_part_with_the_right_bytes() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // Every answer for the last part, and for the second, carries one
        // byte more than the range: junk past the end of the file, and the
        // next part's first byte in the middle of it.
        let mut last = body[4096..].to_vec();
        last.push(0xEE);
        let over_last = ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes 4096-4595/{LEN}"))
            .set_body_bytes(last);
        let over_second = ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes 1024-2047/{LEN}"))
            .set_body_bytes(body[1024..2049].to_vec());
        let (server, client) = serve(RangeServer::new(&body).script(4096, 100, over_last).script(
            1024,
            100,
            over_second,
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.verify = false;

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
        // Each part was complete after one answer: nothing was re-requested.
        assert_eq!(sorted(ranges(&server).await), all_parts());
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
        /// The path of the request line, without the query.
        path: String,
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
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let conn = RawConn {
                    stream,
                    path,
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

    /// Open connections, now and at most, on one server or several.
    #[derive(Default)]
    struct Gauge {
        open: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
    }

    impl Gauge {
        fn enter(&self) {
            let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
        }

        fn leave(&self) {
            self.open.fetch_sub(1, Ordering::SeqCst);
        }

        fn most(&self) -> usize {
            self.most.load(Ordering::SeqCst)
        }
    }

    /// A raw server that holds every request open for `hold`, counted in
    /// its own gauge and in every gauge of `also`, then serves it. A request
    /// for `/download/...` is redirected to `/data/...` on the same server
    /// when `redirect` is set.
    async fn holding_server(
        body: &[u8],
        hold: Duration,
        redirect: bool,
        also: Vec<Arc<Gauge>>,
    ) -> (IaClient, Arc<Gauge>) {
        let gauge = Arc::new(Gauge::default());
        let mut gauges = also;
        gauges.push(Arc::clone(&gauge));
        let handler: RawHandler = Arc::new(move |mut conn| {
            let gauges = gauges.clone();
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                gauges.iter().for_each(|g| g.enter());
                tokio::time::sleep(hold).await;
                // Counted down before the answer goes out, so the client's
                // next request can never be counted alongside this one.
                gauges.iter().for_each(|g| g.leave());
                if redirect && conn.path.starts_with("/download/") {
                    let to = conn.path.replacen("/download/", "/data/", 1);
                    let _ = conn
                        .stream
                        .write_all(
                            format!(
                                "HTTP/1.1 302 Found\r\nlocation: {to}\r\n\
                                 content-length: 0\r\nconnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await;
                } else {
                    serve_whole(conn).await;
                }
            })
        });
        let (client, _seen) = spawn_raw(body, handler).await;
        (client, gauge)
    }

    const HOLD: Duration = Duration::from_millis(300);

    /// Fetch `body` as one file with `opts` from a holding server and return
    /// the most connections it saw open at once.
    async fn count_concurrency(body: &[u8], opts: &DownloadOpts) -> usize {
        let (client, gauge) = holding_server(body, HOLD, false, vec![]).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", body);
        fetch(&client, &file, dir.path(), opts).await.unwrap();
        assert_finished(dir.path(), body);
        gauge.most()
    }

    #[tokio::test]
    async fn connections_above_the_ceiling_use_the_ceiling() {
        let _layout = small_layout();
        // Twelve parts, so more than eight streams could start.
        let body = body_of(12 * PART as usize);
        assert_eq!(
            count_concurrency(&body, &opts(50)).await,
            MAX_CONNECTIONS_PER_FILE
        );
    }

    #[tokio::test]
    async fn the_connection_count_is_the_most_connections_open_at_once() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        assert_eq!(count_concurrency(&body, &opts(3)).await, 3);
    }

    // -- the per-server cap --

    #[tokio::test]
    async fn eight_connections_for_one_file_hold_at_the_default_cap_of_six() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        let mut o = opts(8);
        o.server_slots = ServerSlots::default();
        assert_eq!(count_concurrency(&body, &o).await, 6);
    }

    /// An item of `n` small files, all with the body `body`.
    fn item_of(n: usize, body: &[u8]) -> crate::types::ItemMetadata {
        let files: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "name": format!("f{i}.bin"),
                    "source": "original",
                    "size": body.len().to_string(),
                    "md5": md5_hex(body),
                    "mtime": "1700000000",
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "metadata": {"identifier": "big-item"},
            "files": files,
        }))
        .unwrap()
    }

    /// Download an item of `n` files into `dir` with `jobs` files at once,
    /// and check every file arrived whole.
    async fn download_files(
        client: &IaClient,
        n: usize,
        body: &[u8],
        jobs: usize,
        opts: &DownloadOpts,
        dir: &Path,
    ) {
        let mut o = opts.clone();
        o.destdir = dir.to_path_buf();
        o.no_directories = true;
        let item = tokio::time::timeout(
            LIMIT,
            download_item_with_metadata(
                client,
                "big-item",
                &item_of(n, body),
                &o,
                Arc::new(tokio::sync::Semaphore::new(jobs)),
                None,
            ),
        )
        .await
        .expect("the item download hung")
        .unwrap();
        assert_eq!(item.files_downloaded, n, "{:?}", item.results);
        for i in 0..n {
            assert_eq!(std::fs::read(dir.join(format!("f{i}.bin"))).unwrap(), body);
        }
    }

    #[tokio::test]
    async fn a_first_request_in_flight_holds_its_slot_and_is_not_a_file_to_come() {
        let body = body_of(2048);
        let arrived = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&arrived);
        let policy: Policy = Arc::new(move |_, _, _| {
            seen.store(true, Ordering::SeqCst);
            Reply::Hold(Duration::from_millis(500))
        });
        let (client, _) = recording_server(&body, policy).await;
        let o = DownloadOpts::default();
        let url = format!("http://{}/download/x/small.bin", client.host());
        let server = super::super::server_of(&url);

        let sent = {
            let (client, o, url) = (client.clone(), o.clone(), url.clone());
            tokio::spawn(async move {
                let starting = o.server_slots.starting();
                super::super::fetch_response_via(
                    client.range_http(),
                    &client,
                    &url,
                    None,
                    false,
                    Some(&o),
                    Some(starting),
                )
                .await
                .map(|_| ())
            })
        };
        while !arrived.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Its request holds one of the six; nothing else is to come.
        assert_eq!(o.server_slots.room(&server).await, 5);
        sent.await.unwrap().unwrap();
        assert_eq!(o.server_slots.room(&server).await, 6);
    }

    #[tokio::test]
    async fn whole_files_under_a_drop_give_connections_back_after_the_grace() {
        // 300 KiB per file, 1 KiB at a time with a 20 ms pause before each:
        // 6 s or more each.
        let body = body_of(300 * 1024);
        let gauge = Arc::new(Gauge::default());
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let answered_429 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler: RawHandler = {
            let (gauge, refused, answered) = (
                Arc::clone(&gauge),
                Arc::clone(&refused),
                Arc::clone(&answered_429),
            );
            Arc::new(move |mut conn| {
                let (gauge, refused, answered) = (
                    Arc::clone(&gauge),
                    Arc::clone(&refused),
                    Arc::clone(&answered),
                );
                Box::pin(async move {
                    use tokio::io::AsyncWriteExt;
                    gauge.enter();
                    // The first request for f0.bin, once the others are
                    // open: 429, so the budget halves from 6 to 3.
                    if conn.path.ends_with("/f0.bin") && !refused.swap(true, Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let _ = conn
                            .stream
                            .write_all(
                                b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\n\
                                  content-length: 0\r\nconnection: close\r\n\r\n",
                            )
                            .await;
                        answered.store(true, Ordering::SeqCst);
                        gauge.leave();
                        return;
                    }
                    let len = conn.body.len();
                    let head = if conn.ranged {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\naccept-ranges: bytes\r\n\
                             content-length: {}\r\ncontent-range: bytes {}-{}/{len}\r\n\
                             connection: close\r\n\r\n",
                            conn.last - conn.first + 1,
                            conn.first,
                            conn.last,
                        )
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\naccept-ranges: bytes\r\n\
                             content-length: {len}\r\nconnection: close\r\n\r\n"
                        )
                    };
                    let bytes = slice(&conn);
                    if conn.stream.write_all(head.as_bytes()).await.is_ok() {
                        for chunk in bytes.chunks(1024) {
                            // The pause comes first, so the request counts
                            // as closed once its last byte is written, as
                            // the client sees it.
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            // A connection the client closed fails here.
                            if conn.stream.write_all(chunk).await.is_err() {
                                break;
                            }
                        }
                    }
                    gauge.leave();
                })
            })
        };
        let (client, ranges) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();

        let run = {
            let (client, body, dir) = (client.clone(), body.clone(), dir.path().to_path_buf());
            tokio::spawn(async move {
                download_files(&client, 5, &body, 8, &DownloadOpts::default(), &dir).await;
            })
        };
        // The drop comes at 0.3 s; the grace ends 3 s later and the next
        // one-second check gives the connection back, by about 4.3 s. No
        // file can end before 6 s (300 pauses of 20 ms), so up to 5.8 s every
        // open connection is a transfer: wait for 3 or fewer, then the server
        // must see no more until 5.8 s. (Later, a file's next request may
        // reach the server before it has counted the last one closed.)
        let start = tokio::time::Instant::now();
        let window_end = start + Duration::from_millis(5800);
        while !answered_429.load(Ordering::SeqCst) && tokio::time::Instant::now() < window_end {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(answered_429.load(Ordering::SeqCst), "no 429 by 5.8 s");
        while gauge.open.load(Ordering::SeqCst) > 3 && tokio::time::Instant::now() < window_end {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let open = gauge.open.load(Ordering::SeqCst);
        assert!(open <= 3, "{open} still open 5.8 s in");
        gauge.most.store(open, Ordering::SeqCst);
        tokio::time::sleep_until(window_end).await;
        let most_in_window = gauge.most();
        run.await.unwrap();

        assert!(most_in_window <= 3, "{most_in_window} open after the grace");
        assert!(
            ranges.lock().unwrap().iter().any(Option::is_some),
            "no whole file was resumed with Range"
        );
    }

    #[tokio::test]
    async fn eight_jobs_on_one_server_hold_at_six() {
        let body = body_of(2048);
        let (client, gauge) = holding_server(&body, HOLD, false, vec![]).await;
        let dir = tempfile::tempdir().unwrap();

        download_files(&client, 8, &body, 8, &DownloadOpts::default(), dir.path()).await;

        assert_eq!(gauge.most(), 6);
    }

    #[tokio::test]
    async fn two_servers_with_a_cap_of_six_reach_six_each_and_twelve_in_all() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        let all = Arc::new(Gauge::default());
        let (a, gauge_a) = holding_server(&body, HOLD, false, vec![Arc::clone(&all)]).await;
        let (b, gauge_b) = holding_server(&body, HOLD, false, vec![Arc::clone(&all)]).await;
        let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let file = file_meta("big.bin", &body);
        // One set of options, so one table of slots, for both servers.
        let mut shared = opts(8);
        shared.server_slots = ServerSlots::default();

        let (ra, rb) = tokio::join!(
            fetch(&a, &file, dir_a.path(), &shared),
            fetch(&b, &file, dir_b.path(), &shared),
        );
        ra.unwrap();
        rb.unwrap();

        assert_finished(dir_a.path(), &body);
        assert_finished(dir_b.path(), &body);
        assert_eq!((gauge_a.most(), gauge_b.most()), (6, 6));
        assert_eq!(all.most(), 12);
    }

    #[tokio::test]
    async fn a_cap_of_one_serialises_a_parallel_download_through_a_redirect() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // Every request is redirected once on the same server: the redirect
        // hop and the request it leads to each need the one slot, so a
        // request that held its slot across the redirect would wait forever.
        let (client, gauge) = holding_server(&body, Duration::from_millis(20), true, vec![]).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.server_slots = ServerSlots::with_cap(1);

        let result = fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_finished(dir.path(), &body);
        assert_eq!(gauge.most(), 1);
    }

    #[tokio::test]
    async fn a_stream_waiting_for_a_slot_stops_when_another_fails_the_file() {
        let _layout = small_layout();
        let body = body_of(LEN);
        // A cap of 1: the streams take turns. The third part fails for good;
        // every stream still waiting for the slot must give up without
        // opening a connection. Each connection is logged, as `None` when it
        // closes before sending a request.
        let order: Arc<Mutex<Vec<Option<u64>>>> = Arc::default();
        let log = Arc::clone(&order);
        let handler: RawHandler = Arc::new(move |mut conn| {
            let log = Arc::clone(&log);
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                log.lock()
                    .unwrap()
                    .push((!conn.path.is_empty()).then_some(conn.first));
                if conn.first == 2048 {
                    let _ = conn
                        .stream
                        .write_all(
                            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                serve_whole(conn).await;
            })
        });
        let (client, _seen) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.server_slots = ServerSlots::with_cap(1);

        let err = fetch(&client, &file, dir.path(), &o).await.unwrap_err();

        assert!(matches!(err, IaError::Http { status: 404, .. }), "{err:?}");
        // Give a stream that did not stop the time to connect.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let order = order.lock().unwrap();
        assert!(
            order.iter().all(Option::is_some),
            "a connection without a request: {order:?}"
        );
        assert_eq!(
            order.last(),
            Some(&Some(2048)),
            "a connection after the failure: {order:?}"
        );
    }

    /// A raw server whose first answer for a request starting at `first`
    /// promises the rest of the range, sends 100 bytes and hangs up.
    async fn dropping_once_at(body: &[u8], first: u64) -> IaClient {
        let handler: RawHandler = Arc::new(move |mut conn| {
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                if conn.first == first && conn.nth == 0 {
                    send_head(&mut conn).await;
                    let bytes = slice(&conn);
                    let _ = conn.stream.write_all(&bytes[..100]).await;
                    let _ = conn.stream.flush().await;
                    return;
                }
                serve_whole(conn).await;
            })
        });
        spawn_raw(body, handler).await.0
    }

    #[tokio::test]
    async fn a_cap_of_one_lets_a_range_stream_re_request_after_a_dropped_body() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let client = dropping_once_at(&body, 1024).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);
        let mut o = opts(4);
        o.server_slots = ServerSlots::with_cap(1);

        fetch(&client, &file, dir.path(), &o).await.unwrap();

        assert_finished(dir.path(), &body);
    }

    #[tokio::test]
    async fn a_cap_of_one_serialises_jobs_through_a_redirect() {
        let body = body_of(2048);
        let (client, gauge) = holding_server(&body, Duration::from_millis(20), true, vec![]).await;
        let dir = tempfile::tempdir().unwrap();
        let o = DownloadOpts {
            server_slots: ServerSlots::with_cap(1),
            ..Default::default()
        };

        download_files(&client, 4, &body, 8, &o, dir.path()).await;

        assert_eq!(gauge.most(), 1);
    }

    // -- the contract, and the back-off --

    /// One request as the recording server saw it.
    #[derive(Debug, Clone)]
    struct Visit {
        path: String,
        range: Option<(u64, u64)>,
        arrived: std::time::Instant,
        left: std::time::Instant,
    }

    /// What the recording server does with a request.
    enum Reply {
        /// Hold it this long, then serve it.
        Hold(Duration),
        /// Answer with this status and `Retry-After: 0`, after 50 ms: long
        /// enough for every request sent with it to have arrived, so that
        /// one arriving after the answer was sent after it.
        Status(u16),
    }

    type Policy = Arc<dyn Fn(&str, Option<(u64, u64)>, usize) -> Reply + Send + Sync>;

    /// A raw server that asks `policy` (path, range, how many requests came
    /// before) what to do with each request and records every one; a
    /// request is open from its arrival until just before its answer.
    async fn recording_server(body: &[u8], policy: Policy) -> (IaClient, Arc<Mutex<Vec<Visit>>>) {
        let visits: Arc<Mutex<Vec<Visit>>> = Arc::default();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = Arc::clone(&visits);
        let handler: RawHandler = Arc::new(move |mut conn| {
            let (log, policy, count) = (Arc::clone(&log), Arc::clone(&policy), Arc::clone(&count));
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                let arrived = std::time::Instant::now();
                let range = conn.ranged.then_some((conn.first, conn.last));
                let nth = count.fetch_add(1, Ordering::SeqCst);
                let reply = policy(&conn.path, range, nth);
                match reply {
                    Reply::Hold(hold) => tokio::time::sleep(hold).await,
                    Reply::Status(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
                log.lock().unwrap().push(Visit {
                    path: conn.path.clone(),
                    range,
                    arrived,
                    left: std::time::Instant::now(),
                });
                match reply {
                    Reply::Hold(_) => serve_whole(conn).await,
                    Reply::Status(status) => {
                        let _ = conn
                            .stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 {status} Busy\r\nretry-after: 0\r\n\
                                     content-length: 0\r\nconnection: close\r\n\r\n"
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                }
            })
        });
        let (client, _seen) = spawn_raw(body, handler).await;
        (client, visits)
    }

    /// The most visits open at once among `visits`.
    fn most_open(visits: &[Visit]) -> usize {
        visits
            .iter()
            .map(|v| {
                visits
                    .iter()
                    .filter(|w| w.arrived <= v.arrived && v.arrived < w.left)
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    fn holding(hold: Duration) -> Policy {
        Arc::new(move |_, _, _| Reply::Hold(hold))
    }

    #[tokio::test]
    async fn a_lone_large_file_fills_its_servers_six_connections() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        let (client, visits) = recording_server(&body, holding(HOLD)).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_finished(dir.path(), &body);
        let visits = visits.lock().unwrap();
        assert_eq!(most_open(&visits), 6);
        assert_eq!(visits.len(), 12, "one request per part");
    }

    #[tokio::test]
    async fn fifty_small_files_run_six_at_a_time_and_none_is_split() {
        let body = body_of(2048);
        let (client, visits) = recording_server(&body, holding(Duration::from_millis(100))).await;
        let dir = tempfile::tempdir().unwrap();

        download_files(&client, 50, &body, 8, &DownloadOpts::default(), dir.path()).await;

        let visits = visits.lock().unwrap();
        assert_eq!(most_open(&visits), 6);
        assert!(visits.iter().all(|v| v.range.is_none()), "a range request");
        assert_eq!(visits.len(), 50);
    }

    #[tokio::test]
    async fn three_large_files_share_six_and_each_starts_before_any_splits() {
        let _layout = small_layout();
        let body = body_of(LEN);
        let (client, visits) = recording_server(&body, holding(HOLD)).await;
        let dir = tempfile::tempdir().unwrap();

        download_files(&client, 3, &body, 8, &DownloadOpts::default(), dir.path()).await;

        let mut visits = visits.lock().unwrap().clone();
        assert_eq!(most_open(&visits), 6);
        visits.sort_by_key(|v| v.arrived);
        // Files before ranges: every file's first part was asked for before
        // any file's second stream.
        let first_parts = visits
            .iter()
            .position(|v| v.range.is_some_and(|(a, _)| a != 0))
            .expect("some file split");
        let starts: std::collections::HashSet<&str> = visits[..first_parts]
            .iter()
            .filter(|v| v.range.is_some_and(|(a, _)| a == 0))
            .map(|v| v.path.as_str())
            .collect();
        assert_eq!(starts.len(), 3, "{visits:#?}");
    }

    #[tokio::test]
    async fn three_servers_reach_six_each_and_no_more_than_jobs_in_all() {
        let body = body_of(2048);
        // 24 files in all: with -j 24 every file has its place and each
        // server's 8 are runnable at once.
        for jobs in [24, 8] {
            let all = Arc::new(Gauge::default());
            let mut servers = Vec::new();
            for _ in 0..3 {
                servers.push(holding_server(&body, HOLD, false, vec![Arc::clone(&all)]).await);
            }
            let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
            // One table of slots and one -j for the whole run, as the CLI
            // has.
            let shared = DownloadOpts::default();
            let jobs_sem = Arc::new(tokio::sync::Semaphore::new(jobs));
            let runs = servers.iter().zip(&dirs).map(|((client, _), dir)| {
                let mut o = shared.clone();
                o.destdir = dir.path().to_path_buf();
                o.no_directories = true;
                let jobs_sem = Arc::clone(&jobs_sem);
                let body = body.clone();
                async move {
                    download_item_with_metadata(
                        client,
                        "big-item",
                        &item_of(8, &body),
                        &o,
                        jobs_sem,
                        None,
                    )
                    .await
                    .unwrap()
                }
            });
            let results = futures::future::join_all(runs).await;
            assert!(results.iter().all(|r| r.files_downloaded == 8));
            let each: Vec<usize> = servers.iter().map(|(_, g)| g.most()).collect();
            if jobs == 24 {
                assert_eq!(each, vec![6, 6, 6]);
                assert_eq!(all.most(), 18);
            } else {
                // How the 8 places fall across the three items is timing;
                // the bound is what matters.
                assert!(each.iter().all(|&m| m <= 6), "{each:?}");
                assert!(all.most() <= 8, "{} open with -j 8", all.most());
            }
        }
    }

    /// Download 16 small files with `-j 8` from a server that answers the
    /// first request with `status`, then holds every request; return the
    /// most requests open at once among those that came after that answer.
    async fn after_a_drop(status: u16) -> usize {
        let body = body_of(2048);
        let policy: Policy = Arc::new(move |_, _, nth| {
            if nth == 0 {
                Reply::Status(status)
            } else {
                Reply::Hold(Duration::from_millis(200))
            }
        });
        let (client, visits) = recording_server(&body, policy).await;
        let dir = tempfile::tempdir().unwrap();
        // The request answered with the status was the first to arrive, and
        // left at once.
        let o = DownloadOpts {
            retries: 3,
            ..Default::default()
        };

        download_files(&client, 16, &body, 8, &o, dir.path()).await;

        let visits = visits.lock().unwrap().clone();
        let answered = visits.iter().map(|v| v.left).min().expect("visits");
        let later: Vec<Visit> = visits
            .into_iter()
            .filter(|v| v.arrived > answered)
            .collect();
        most_open(&later)
    }

    #[tokio::test]
    async fn after_a_429_the_server_sees_at_most_three() {
        assert_eq!(after_a_drop(429).await, 3);
    }

    #[tokio::test]
    async fn after_a_503_the_server_sees_at_most_three() {
        assert_eq!(after_a_drop(503).await, 3);
    }

    #[tokio::test]
    async fn a_split_file_under_a_drop_sheds_range_streams_and_completes() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        // The first request for the fourth part fails with a 503; every
        // other request is held a moment.
        let seen_503 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&seen_503);
        let policy: Policy = Arc::new(move |_, range, _| {
            if range.is_some_and(|(a, _)| a == 3 * PART) && !flag.swap(true, Ordering::SeqCst) {
                Reply::Status(503)
            } else {
                Reply::Hold(Duration::from_millis(200))
            }
        });
        let (client, visits) = recording_server(&body, policy).await;
        let dir = tempfile::tempdir().unwrap();
        let file = file_meta("big.bin", &body);

        fetch(&client, &file, dir.path(), &DownloadOpts::default())
            .await
            .unwrap();

        assert_finished(dir.path(), &body);
        let visits = visits.lock().unwrap().clone();
        let answered = visits
            .iter()
            .filter(|v| v.range.is_some_and(|(a, _)| a == 3 * PART))
            .map(|v| v.left)
            .min()
            .expect("the 503");
        // Streams whose answers came after the drop gave way, and their
        // parts were asked for again by the streams that stayed: some part
        // other than the one that failed was requested twice.
        let mut times: HashMap<u64, usize> = HashMap::new();
        for v in &visits {
            if let Some((a, _)) = v.range {
                *times.entry(a / PART).or_default() += 1;
            }
        }
        assert!(
            times.iter().any(|(&k, &n)| k != 3 && n > 1),
            "no stream gave way: {times:?}"
        );
        // From the drop on, at most 3 requests at once.
        let later: Vec<Visit> = visits
            .into_iter()
            .filter(|v| v.arrived > answered)
            .collect();
        assert!(most_open(&later) <= 3, "{later:#?}");
    }

    /// The metadata of an item with one file `big.bin` of `body`, on the
    /// datanodes `d1` and `d2`.
    fn item_on(body: &[u8], d1: &str, d2: &str) -> crate::types::ItemMetadata {
        serde_json::from_value(serde_json::json!({
            "metadata": {"identifier": "big-item"},
            "files": [{
                "name": "big.bin",
                "source": "original",
                "size": body.len().to_string(),
                "md5": md5_hex(body),
                "mtime": "1700000000",
            }],
            "d1": d1,
            "d2": d2,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn an_items_two_datanodes_share_one_budget_of_six() {
        let _layout = small_layout();
        let body = body_of(12 * PART as usize);
        // Two "datanodes", counted together, and a front server whose
        // /download/ redirect alternates between them, as archive.org's may.
        let both = Arc::new(Gauge::default());
        let (a, _) = holding_server(&body, HOLD, false, vec![Arc::clone(&both)]).await;
        let (b, _) = holding_server(&body, HOLD, false, vec![Arc::clone(&both)]).await;
        let nodes = [a.host().to_string(), b.host().to_string()];
        let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let to = nodes.clone();
        let handler: RawHandler = Arc::new(move |mut conn| {
            let node = to[turn.fetch_add(1, Ordering::SeqCst) % 2].clone();
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                let _ = conn
                    .stream
                    .write_all(
                        format!(
                            "HTTP/1.1 302 Found\r\nlocation: http://{node}{}\r\n\
                             content-length: 0\r\nconnection: close\r\n\r\n",
                            conn.path
                        )
                        .as_bytes(),
                    )
                    .await;
            })
        });
        let (front, _) = spawn_raw(&body, handler).await;
        let dir = tempfile::tempdir().unwrap();
        // Eight streams pinned: more than one server's six.
        let o = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            no_directories: true,
            connections_per_file: Some(8),
            ..Default::default()
        };

        let item = download_item_with_metadata(
            &front,
            "big-item",
            &item_on(&body, &nodes[0], &nodes[1]),
            &o,
            Arc::new(tokio::sync::Semaphore::new(8)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(item.files_downloaded, 1, "{:?}", item.results);
        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), body);
        let most = both.most();
        assert!(most <= 6, "{most} open on the two datanodes together");
        assert!(most > 1, "the file was not split");
    }

    /// A node that keeps every connection open for more requests until the
    /// client closes it, as a datanode may, and counts open TCP connections
    /// in `gauge`, from accept to close: idle ones too. Each request is
    /// held 300 ms, then answered with the whole of `body`.
    async fn keep_alive_node(body: &[u8], gauge: Arc<Gauge>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let body = Arc::new(body.to_vec());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (gauge, body) = (Arc::clone(&gauge), Arc::clone(&body));
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    gauge.enter();
                    loop {
                        if read_head(&mut stream).await.is_empty() {
                            break;
                        }
                        tokio::time::sleep(HOLD).await;
                        let head =
                            format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
                        if stream.write_all(head.as_bytes()).await.is_err()
                            || stream.write_all(&body).await.is_err()
                        {
                            break;
                        }
                    }
                    gauge.leave();
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn eight_whole_files_on_one_pair_open_six_connections_and_keep_none() {
        let body = body_of(2048);
        // The pair's two nodes, counted together, behind a front server
        // whose /download/ redirect alternates between them.
        let both = Arc::new(Gauge::default());
        let nodes = [
            keep_alive_node(&body, Arc::clone(&both)).await,
            keep_alive_node(&body, Arc::clone(&both)).await,
        ];
        let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let to = nodes.clone();
        let handler: RawHandler = Arc::new(move |mut conn| {
            let node = to[turn.fetch_add(1, Ordering::SeqCst) % 2].clone();
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                let _ = conn
                    .stream
                    .write_all(
                        format!(
                            "HTTP/1.1 302 Found\r\nlocation: http://{node}{}\r\n\
                             content-length: 0\r\nconnection: close\r\n\r\n",
                            conn.path
                        )
                        .as_bytes(),
                    )
                    .await;
            })
        });
        let (front, _) = spawn_raw(&body, handler).await;
        let files: Vec<serde_json::Value> = (0..8)
            .map(|i| {
                serde_json::json!({
                    "name": format!("f{i}.bin"),
                    "source": "original",
                    "size": body.len().to_string(),
                    "md5": md5_hex(&body),
                    "mtime": "1700000000",
                })
            })
            .collect();
        let item: crate::types::ItemMetadata = serde_json::from_value(serde_json::json!({
            "metadata": {"identifier": "big-item"},
            "files": files,
            "d1": nodes[0],
            "d2": nodes[1],
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let o = DownloadOpts {
            destdir: dir.path().to_path_buf(),
            no_directories: true,
            ..Default::default()
        };

        let done = download_item_with_metadata(
            &front,
            "big-item",
            &item,
            &o,
            Arc::new(tokio::sync::Semaphore::new(8)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(done.files_downloaded, 8, "{:?}", done.results);
        for i in 0..8 {
            assert_eq!(
                std::fs::read(dir.path().join(format!("f{i}.bin"))).unwrap(),
                body
            );
        }
        // Six transfers at once, each on a TCP connection of its own.
        assert_eq!(both.most(), 6, "TCP connections open on the pair at once");
        // And none kept open once the download is done.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while both.open.load(Ordering::SeqCst) > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            both.open.load(Ordering::SeqCst),
            0,
            "idle connections kept open"
        );
    }

    #[tokio::test]
    async fn every_download_connection_carries_one_request_and_asks_to_close() {
        let _layout = small_layout();
        let big = body_of(12 * PART as usize);
        // A server that keeps connections open, and records how many
        // requests each carried and whether each asked to close.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let most_per_connection = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let without_close = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (most, open_ended, all) = (
            Arc::clone(&most_per_connection),
            Arc::clone(&without_close),
            Arc::clone(&requests),
        );
        let served = Arc::new(big.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (most, open_ended, all, body) = (
                    Arc::clone(&most),
                    Arc::clone(&open_ended),
                    Arc::clone(&all),
                    Arc::clone(&served),
                );
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let mut on_this = 0;
                    loop {
                        let head = read_head(&mut stream).await;
                        if head.is_empty() {
                            return;
                        }
                        on_this += 1;
                        most.fetch_max(on_this, Ordering::SeqCst);
                        all.fetch_add(1, Ordering::SeqCst);
                        let lower = head.to_ascii_lowercase();
                        if !lower.contains("\r\nconnection: close") {
                            open_ended.fetch_add(1, Ordering::SeqCst);
                        }
                        let range = lower.lines().find_map(|l| {
                            l.strip_prefix("range: bytes=").and_then(|r| {
                                let (a, b) = r.trim().split_once('-')?;
                                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                            })
                        });
                        // The small files are 2048 bytes; the large one is
                        // served by range.
                        let (status, extra, bytes) = match range {
                            Some((a, b)) => (
                                "206 Partial Content",
                                format!("content-range: bytes {a}-{b}/{}\r\n", body.len()),
                                body[a..=b].to_vec(),
                            ),
                            None if lower.contains("big.bin") => {
                                ("200 OK", String::new(), body.to_vec())
                            }
                            None => ("200 OK", String::new(), body[..2048].to_vec()),
                        };
                        let answer = format!(
                            "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\n\r\n",
                            bytes.len()
                        );
                        if stream.write_all(answer.as_bytes()).await.is_err()
                            || stream.write_all(&bytes).await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Small files on one stream each, then a large file split.
        download_files(
            &client,
            10,
            &big[..2048],
            8,
            &DownloadOpts::default(),
            dir.path(),
        )
        .await;
        fetch(
            &client,
            &file_meta("big.bin", &big),
            dir.path(),
            &DownloadOpts::default(),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), big);
        assert_eq!(requests.load(Ordering::SeqCst), 10 + 12);
        assert_eq!(
            most_per_connection.load(Ordering::SeqCst),
            1,
            "a connection was kept open for another request"
        );
        assert_eq!(without_close.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_whole_file_re_request_does_not_count_another_view() {
        let body = body_of(2048);
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        {
            let (lines, body) = (Arc::clone(&lines), body.clone());
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::AsyncWriteExt;
                    let head = read_head(&mut stream).await;
                    let first_line = head.lines().next().unwrap_or("").to_string();
                    let ranged = head.to_ascii_lowercase().contains("\r\nrange: bytes=100-");
                    lines.lock().unwrap().push(first_line);
                    if ranged {
                        let rest = &body[100..];
                        let answer = format!(
                            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\n\
                             content-range: bytes 100-{}/{}\r\n\r\n",
                            rest.len(),
                            body.len() - 1,
                            body.len()
                        );
                        let _ = stream.write_all(answer.as_bytes()).await;
                        let _ = stream.write_all(rest).await;
                    } else {
                        // Promise the whole file, send 100 bytes, hang up.
                        let answer =
                            format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
                        let _ = stream.write_all(answer.as_bytes()).await;
                        let _ = stream.write_all(&body[..100]).await;
                    }
                }
            });
        }
        let mut config = crate::config::IaConfig::default();
        config.general.host = format!("127.0.0.1:{port}");
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let o = DownloadOpts {
            count_views: true,
            ..Default::default()
        };

        fetch(&client, &file_meta("small.bin", &body), dir.path(), &o)
            .await
            .unwrap();

        assert_eq!(std::fs::read(dir.path().join("small.bin")).unwrap(), body);
        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            !lines[0].contains("cnt=0"),
            "the first request counts the view"
        );
        assert!(lines[1].contains("cnt=0"), "{lines:?}");
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
