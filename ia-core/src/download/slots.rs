//! The connection budget per datanode (#16): at most
//! [`MAX_CONNECTIONS_PER_SERVER`] connections to one server at once,
//! counting every download connection, whole files and byte ranges alike.
//!
//! Every request a download sends takes a slot on the server it goes to
//! before it is sent and keeps it until its response is dropped; a request
//! that finds the server full waits for a slot. Within a process the slots
//! are a semaphore per server whose permit count is that server's live
//! budget: it halves on a 429 or 5xx from the server and grows back by one
//! per calm minute (see [`ServerSlots::report`]). Across processes
//! ([`ServerSlots::across_processes`]) each slot is also a file,
//! `<dir>/<server>/slot-<i>`, held by an exclusive advisory lock for as long
//! as the in-process slot is held; the kernel releases the lock when the
//! file is closed or the process ends, however it ends. See "Contract",
//! "Slots", "Back-off" and "Across processes" in
//! `docs/plans/2026-11-03-parallel-range-downloads.md`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// The most connections `ia` opens to one datanode at once, across every
/// `ia` process on the machine: the limit agreed with IA operations on
/// 2026-11-05.
pub const MAX_CONNECTIONS_PER_SERVER: usize = 6;

/// How long a server must answer without a 429 or 5xx before its budget
/// grows back by one.
const GROWTH_INTERVAL: Duration = Duration::from_secs(60);

/// The first wait when every slot file is locked by another process.
const FIRST_POLL: Duration = Duration::from_millis(50);

/// The longest wait between two looks at the slot files.
const LAST_POLL: Duration = Duration::from_millis(500);

/// How long a request waits on other processes' slot files before one
/// warning says so, once per server: at the default log level a wait is
/// otherwise silent, and a suspended `ia` keeps its slots.
const WAIT_WARNING: Duration = Duration::from_secs(10);

/// A shared table of download slots: for each server, a live budget of at
/// most [`MAX_CONNECTIONS_PER_SERVER`] connections.
///
/// Clones share the table, so every download that clones one
/// [`DownloadOpts`](super::DownloadOpts) shares the budgets; a new
/// `ServerSlots` starts every server at the full budget. A server is a URL
/// authority: its host, and its port when the URL gives one.
///
/// [`ServerSlots::default`] keeps the cap within the process.
/// [`ServerSlots::across_processes`] also shares it with every other process
/// that uses the same directory.
///
/// ```
/// use ia_core::download::{DownloadOpts, ServerSlots, MAX_CONNECTIONS_PER_SERVER};
///
/// let opts = DownloadOpts::default();
/// assert_eq!(opts.server_slots.max(), MAX_CONNECTIONS_PER_SERVER);
/// ```
#[derive(Debug, Clone)]
pub struct ServerSlots {
    inner: Arc<Table>,
}

/// A server's budget dropped after it answered 429 or a 5xx: the argument
/// of the callback given to [`ServerSlots::with_notices`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BudgetDrop {
    /// The server that answered, as a URL authority
    /// (`ia800807.us.archive.org`). The budget that dropped is that of its
    /// item's datanode pair when the item's metadata names one.
    pub server: String,
    /// The status that caused the drop: 429, or 500 to 599.
    pub status: u16,
    /// The budget before the drop.
    pub before: usize,
    /// The budget after it.
    pub after: usize,
}

/// The callback for [`BudgetDrop`]s.
struct Notices(Arc<dyn Fn(&BudgetDrop) + Send + Sync>);

impl std::fmt::Debug for Notices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notices(..)")
    }
}

#[derive(Debug)]
struct Table {
    max: usize,
    servers: Mutex<HashMap<String, Arc<Node>>>,
    growth_interval: Duration,
    /// Files this process has started that hold no slot yet, on any server
    /// (their server is known only once their first request is answered).
    starting: AtomicUsize,
    notices: OnceLock<Notices>,
    /// The slot files' directory, when the cap is shared across processes.
    files: Option<PathBuf>,
    /// Set, with one warning, when the slot files cannot be used; the cap
    /// is then per process for the rest of the run.
    files_failed: AtomicBool,
    /// The servers a long wait on other processes has been reported for.
    waits_reported: Mutex<std::collections::HashSet<String>>,
}

/// One server's slots in this process.
#[derive(Debug)]
struct Node {
    /// Holds the live budget as permits: those free plus those in use,
    /// less those owed.
    semaphore: Arc<Semaphore>,
    state: Mutex<NodeState>,
}

#[derive(Debug)]
struct NodeState {
    /// The live budget, 1 to `max`.
    budget: usize,
    /// Slots of this server held in this process.
    in_use: usize,
    /// Permits to forget as they come back, after a drop below the slots
    /// in use.
    owed: usize,
    /// Counts the drops; a slot taken under an older one does not cause
    /// another (one drop per round of answers).
    epoch: u64,
    /// The last drop or growth: the budget grows by one each time
    /// `growth_interval` passes after it without a 429 or 5xx.
    calm_since: Instant,
    /// Range streams asked to stop because the budget fell below the slots
    /// in use.
    shed: usize,
}

impl Node {
    fn state(&self) -> std::sync::MutexGuard<'_, NodeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An item's two datanodes (`d1`, `d2` in its metadata), counted as one
/// server: `/download/` sends each request to either, so a budget per host
/// would let one item hold twice the cap. The key is the two host names,
/// sorted and joined with `+`, for the in-process budget and the slot-file
/// directory alike; two items on the same pair share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Datanodes {
    key: String,
    hosts: Vec<String>,
}

impl Datanodes {
    /// The pair of `item`, or `None` unless its metadata names both.
    pub(crate) fn of(item: &crate::types::ItemMetadata) -> Option<Self> {
        fn named(d: &Option<String>) -> Option<&str> {
            d.as_deref().map(str::trim).filter(|d| !d.is_empty())
        }
        let mut hosts = vec![
            named(&item.d1)?.to_ascii_lowercase(),
            named(&item.d2)?.to_ascii_lowercase(),
        ];
        hosts.sort();
        hosts.dedup();
        Some(Self {
            key: hosts.join("+"),
            hosts,
        })
    }

    /// The slot key for a request to `server` (a URL authority): the
    /// pair's, when `server` is one of its two hosts.
    pub(crate) fn key_for(&self, server: &str) -> Option<&str> {
        self.hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(server))
            .then_some(self.key.as_str())
    }
}

/// A held slot: the in-process permit and, across processes, the locked
/// slot file. Dropping it gives both back.
#[derive(Debug)]
pub(crate) struct SlotGuard {
    server: String,
    node: Arc<Node>,
    /// The budget's epoch when the slot was taken.
    epoch: u64,
    permit: Option<OwnedSemaphorePermit>,
    file: Option<std::fs::File>,
}

impl SlotGuard {
    /// The server this slot is on.
    pub(crate) fn server(&self) -> &str {
        &self.server
    }

    /// Whether the stream holding this slot should stop now, because the
    /// server's budget fell below the slots in use. Counts the stop: it
    /// answers `true` to as many callers as there are slots too many.
    pub(crate) fn take_shed(&self) -> bool {
        let mut state = self.node.state();
        if state.shed > 0 {
            state.shed -= 1;
            true
        } else {
            false
        }
    }
}

impl Drop for SlotGuard {
    /// The file is unlocked and closed before the permit goes back. The
    /// next task of this process, woken by the permit, then finds the file
    /// free instead of waiting a poll for it; and on Windows the lock goes
    /// at once, where closing a handle releases its locks only when the
    /// system gets to it. A permit owed after a drop is forgotten instead
    /// of returned.
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Through the trait, as `try_lock` is (see `try_lock_one`).
            let _ = fs4::FileExt::unlock(&file);
        }
        let mut state = self.node.state();
        state.in_use = state.in_use.saturating_sub(1);
        if let Some(permit) = self.permit.take() {
            if state.owed > 0 {
                state.owed -= 1;
                permit.forget();
            } else {
                drop(permit);
            }
        }
        state.shed = state.shed.min(state.in_use.saturating_sub(state.budget));
    }
}

/// A file counted as about to start (see [`ServerSlots::starting`]); the
/// count goes down when it is dropped.
#[derive(Debug)]
pub(crate) struct Starting(Arc<Table>);

impl Drop for Starting {
    fn drop(&mut self) {
        self.0.starting.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ServerSlots {
    fn build(max: usize, files: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Table {
                max: max.clamp(1, Semaphore::MAX_PERMITS),
                servers: Mutex::new(HashMap::new()),
                growth_interval: GROWTH_INTERVAL,
                starting: AtomicUsize::new(0),
                notices: OnceLock::new(),
                files,
                files_failed: AtomicBool::new(false),
                waits_reported: Mutex::default(),
            }),
        }
    }

    /// [`MAX_CONNECTIONS_PER_SERVER`] connections to each server at once,
    /// counted across every process that uses slot files under `dir`, such
    /// as the one [`default_slot_dir`] names.
    ///
    /// Each slot is the file `<dir>/<server>/slot-<i>`, held by an exclusive
    /// advisory lock (`flock` on Unix, `LockFileEx` on Windows) for as long
    /// as the slot is held. The files are created when first needed and
    /// never removed. If they cannot be used (the directory cannot be
    /// created, or the file system does not lock), one warning is logged
    /// and the cap holds within this process only; a download never fails
    /// because of them. Each process keeps its own live budget: a drop in
    /// one does not lower another's.
    #[must_use]
    pub fn across_processes(dir: impl Into<PathBuf>) -> Self {
        Self::build(MAX_CONNECTIONS_PER_SERVER, Some(dir.into()))
    }

    /// Call `notice` once for every drop of a server's budget, with the
    /// server, the status that caused it and the budget before and after.
    /// Set it before the table is shared; a second call is ignored.
    #[must_use]
    pub fn with_notices(self, notice: impl Fn(&BudgetDrop) + Send + Sync + 'static) -> Self {
        let _ = self.inner.notices.set(Notices(Arc::new(notice)));
        self
    }

    /// A table with another cap than [`MAX_CONNECTIONS_PER_SERVER`], within
    /// the process: tests only, where the cap is a parameter.
    #[cfg(test)]
    pub(crate) fn with_cap(max: usize) -> Self {
        Self::build(max, None)
    }

    /// [`Self::with_cap`] with slot files under `dir`.
    #[cfg(test)]
    pub(crate) fn across_processes_with_cap(max: usize, dir: impl Into<PathBuf>) -> Self {
        Self::build(max, Some(dir.into()))
    }

    /// The most connections to one server at once: the budget a server
    /// starts with, and never exceeds.
    #[must_use]
    pub fn max(&self) -> usize {
        self.inner.max
    }

    /// The live budget of `server` in this process.
    #[cfg(test)]
    pub(crate) fn budget(&self, server: &str) -> usize {
        let node = self.node(server);
        self.grow(&node);
        let budget = node.state().budget;
        budget
    }

    fn node(&self, server: &str) -> Arc<Node> {
        let mut servers = self
            .inner
            .servers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(servers.entry(server.to_string()).or_insert_with(|| {
            Arc::new(Node {
                semaphore: Arc::new(Semaphore::new(self.inner.max)),
                state: Mutex::new(NodeState {
                    budget: self.inner.max,
                    in_use: 0,
                    owed: 0,
                    epoch: 0,
                    calm_since: Instant::now(),
                    shed: 0,
                }),
            })
        }))
    }

    /// Add back one slot of budget for every `growth_interval` since the
    /// last drop or growth, up to `max`: an owed permit is cancelled first,
    /// else a permit is added. Returns when the next growth is due, if the
    /// budget is below `max`.
    fn grow(&self, node: &Node) -> Option<Instant> {
        let interval = self.inner.growth_interval;
        let now = Instant::now();
        let mut state = node.state();
        while state.budget < self.inner.max && now >= state.calm_since + interval {
            state.budget += 1;
            state.calm_since += interval;
            if state.owed > 0 {
                state.owed -= 1;
            } else {
                node.semaphore.add_permits(1);
            }
            tracing::debug!(budget = state.budget, "connection budget grew back by one");
        }
        state.shed = state.shed.min(state.in_use.saturating_sub(state.budget));
        (state.budget < self.inner.max).then(|| state.calm_since + interval)
    }

    /// Count a file as about to start until the returned value is dropped:
    /// it has its place under `-j` and holds no slot yet. A large file
    /// leaves room for such files before it splits (see [`Self::room`]).
    pub(crate) fn starting(&self) -> Starting {
        self.inner.starting.fetch_add(1, Ordering::Relaxed);
        Starting(Arc::clone(&self.inner))
    }

    /// The slots free on `server` now, after one for every file about to
    /// start: free means a permit of this process's live budget is
    /// available and, across processes, one of the slot files is unlocked
    /// (counted without taking it). How many extra range streams a file
    /// starting now may run.
    pub(crate) async fn room(&self, server: &str) -> usize {
        let node = self.node(server);
        self.grow(&node);
        let mut free = node.semaphore.available_permits();
        if let Some(dir) = self.inner.files.as_deref() {
            if !self.inner.files_failed.load(Ordering::Relaxed) {
                let (dir, server, max) = (dir.to_path_buf(), server.to_string(), self.inner.max);
                let unlocked =
                    tokio::task::spawn_blocking(move || count_unlocked(&dir, &server, max)).await;
                if let Ok(unlocked) = unlocked {
                    free = free.min(unlocked);
                }
            }
        }
        free.saturating_sub(self.inner.starting.load(Ordering::Relaxed))
    }

    /// Wait for a slot on `server` and hold it until the guard is dropped.
    /// The wait wakes when the budget grows, too. `None` only if the
    /// semaphore was closed, which this code never does.
    pub(crate) async fn acquire(&self, server: &str) -> Option<SlotGuard> {
        let node = self.node(server);
        if node.semaphore.available_permits() == 0 {
            tracing::debug!(
                server,
                max = self.inner.max,
                "waiting for a connection slot"
            );
        }
        let permit = loop {
            let next_growth = self.grow(&node);
            let acquire = Arc::clone(&node.semaphore).acquire_owned();
            match next_growth {
                Some(at) => {
                    tokio::select! {
                        biased;
                        permit = acquire => break permit.ok()?,
                        () = tokio::time::sleep_until(at) => {}
                    }
                }
                None => break acquire.await.ok()?,
            }
        };
        let epoch = {
            let mut state = node.state();
            state.in_use += 1;
            state.epoch
        };
        // From here the guard owns the permit, so an early return gives it
        // back.
        let mut guard = SlotGuard {
            server: server.to_string(),
            node,
            epoch,
            permit: Some(permit),
            file: None,
        };
        guard.file = self.lock_slot_file(server).await;
        Some(guard)
    }

    /// Note the status `host` answered on the slot `guard`. A 429 or a
    /// 5xx halves the server's budget (rounded up, never below 1: 6, 3, 2, 1),
    /// unless the slot was taken before the last drop, so one round of such
    /// answers drops it once; either way the budget waits a full
    /// `growth_interval` again before growing. Free permits are forgotten
    /// at once and the rest owed, range streams over the new budget are
    /// asked to stop, and the notice callback runs. Other statuses do
    /// nothing.
    pub(crate) fn report(&self, guard: &SlotGuard, host: &str, status: u16) {
        if !(status == 429 || (500..=599).contains(&status)) {
            return;
        }
        let drop = {
            let mut state = guard.node.state();
            state.calm_since = Instant::now();
            if guard.epoch != state.epoch {
                return;
            }
            state.epoch += 1;
            let before = state.budget;
            // Rounded up: 6 → 3 → 2 → 1, the contract's sequence (integer
            // division would go from 3 straight to 1).
            let after = before.div_ceil(2).max(1);
            if after == before {
                return;
            }
            state.budget = after;
            let lower = before - after;
            let forgotten = guard.node.semaphore.forget_permits(lower);
            state.owed += lower - forgotten;
            state.shed = state.in_use.saturating_sub(after);
            BudgetDrop {
                server: host.to_string(),
                status,
                before,
                after,
            }
        };
        tracing::info!(
            server = %drop.server,
            status,
            before = drop.before,
            after = drop.after,
            "server asked us to slow down; connection budget lowered"
        );
        if let Some(notices) = self.inner.notices.get() {
            (notices.0)(&drop);
        }
    }

    /// With slot files in use, wait until one of this process's first
    /// `max` files for `server` is free and return it locked; `None` when
    /// the cap is per process, or once the files have failed.
    ///
    /// The in-process permit is already held, so this process is under its
    /// own cap and only other processes can hold its files. Each look at
    /// the files runs on the blocking pool: on a local disk it is a few
    /// non-blocking opens and locks, but on NFS each lock is a round trip,
    /// and a hung mount must not stall the async workers. A look abandoned
    /// with its request still finishes there, and a file it locked is closed
    /// with it, which releases the lock.
    async fn lock_slot_file(&self, server: &str) -> Option<std::fs::File> {
        let dir = self.inner.files.as_deref()?;
        let started = Instant::now();
        let mut wait = FIRST_POLL;
        loop {
            if self.inner.files_failed.load(Ordering::Relaxed) {
                return None;
            }
            let look = {
                let (dir, server, max) = (dir.to_path_buf(), server.to_string(), self.inner.max);
                tokio::task::spawn_blocking(move || try_lock_one(&dir, &server, max)).await
            };
            let look = look.unwrap_or_else(|e| Err((dir.to_path_buf(), std::io::Error::other(e))));
            match look {
                Ok(Some(file)) => return Some(file),
                Ok(None) => {
                    tracing::debug!(
                        server,
                        max = self.inner.max,
                        "every connection slot file is held by another process; waiting"
                    );
                    if started.elapsed() >= WAIT_WARNING && self.first_long_wait(server) {
                        tracing::warn!(
                            "waiting for a connection slot on {server}: all {} are held by \
                             other ia processes (a suspended one keeps its slots)",
                            self.inner.max
                        );
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(LAST_POLL);
                }
                Err((path, error)) => {
                    if !self.inner.files_failed.swap(true, Ordering::Relaxed) {
                        tracing::warn!(
                            "cannot use the connection slot files at {}: {error}; \
                             the per-server connection cap holds within this process only",
                            path.display()
                        );
                    }
                    return None;
                }
            }
        }
    }

    /// Whether this is the first long wait reported for `server`.
    fn first_long_wait(&self, server: &str) -> bool {
        self.inner
            .waits_reported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(server.to_string())
    }
}

/// How many of the first `max` slot files of `server` under `dir` another
/// open could lock now (a missing file counts as free). Each is locked and
/// unlocked at once; an error counts the file as free, as the slot files
/// are then given up anyway.
fn count_unlocked(dir: &Path, server: &str, max: usize) -> usize {
    (0..max)
        .filter(|&i| match std::fs::File::open(slot_path(dir, server, i)) {
            Ok(file) => match fs4::FileExt::try_lock(&file) {
                Ok(()) => {
                    let _ = fs4::FileExt::unlock(&file);
                    true
                }
                Err(fs4::TryLockError::WouldBlock) => false,
                Err(fs4::TryLockError::Error(_)) => true,
            },
            Err(_) => true,
        })
        .count()
}

/// Lock the first free one of the first `max` slot files of `server` under
/// `dir`, creating the server's directory and the files as they are first
/// needed. `Ok(None)` when every one is locked; the path and the error when
/// the files cannot be used at all (anything but "would block").
fn try_lock_one(
    dir: &Path,
    server: &str,
    max: usize,
) -> std::result::Result<Option<std::fs::File>, (PathBuf, std::io::Error)> {
    let server_dir = dir.join(server_dir_name(server));
    std::fs::create_dir_all(&server_dir).map_err(|e| (server_dir.clone(), e))?;
    for i in 0..max {
        let path = slot_path(dir, server, i);
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            // A slot file another user created (in a shared IA_CACHE_DIR)
            // may not be writable here; a lock needs only read access.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::fs::File::open(&path).map_err(|_| (path.clone(), e))?
            }
            Err(e) => return Err((path, e)),
        };
        // Called through the trait: from Rust 1.89 `File` has an inherent
        // `try_lock` with another error type, which would win otherwise.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(fs4::TryLockError::WouldBlock) => {}
            Err(fs4::TryLockError::Error(e)) => return Err((path, e)),
        }
    }
    Ok(None)
}

impl Default for ServerSlots {
    /// [`MAX_CONNECTIONS_PER_SERVER`] connections to each server at once,
    /// within this process.
    fn default() -> Self {
        Self::build(MAX_CONNECTIONS_PER_SERVER, None)
    }
}

/// The directory [`ServerSlots::across_processes`] uses by default:
/// `connections` under the per-user cache directory of `ia`. That is
/// `$IA_CACHE_DIR` when it is set, otherwise `ia` under the platform's cache
/// directory: `$XDG_CACHE_HOME` or `~/.cache` on Linux and other Unix
/// systems, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows. An
/// empty variable counts as unset, and a relative `IA_CACHE_DIR` is taken
/// from the current directory, once, so that processes started in
/// different directories still name the same files. `None` when none of
/// them is set.
#[must_use]
pub fn default_slot_dir() -> Option<PathBuf> {
    let root = cache_root_from(|name| std::env::var_os(name))?;
    let root = if root.is_relative() {
        std::env::current_dir().ok()?.join(root)
    } else {
        root
    };
    Some(root.join("connections"))
}

/// The cache root, reading environment variables through `var`.
fn cache_root_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = set("IA_CACHE_DIR") {
        return Some(dir);
    }
    platform_cache_dir(&set).map(|dir| dir.join("ia"))
}

/// The platform's per-user cache directory, by the `dirs` crate's rules.
#[cfg(target_os = "macos")]
fn platform_cache_dir(set: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    set("HOME").map(|home| home.join("Library").join("Caches"))
}

/// The platform's per-user cache directory, by the `dirs` crate's rules: a
/// relative `XDG_CACHE_HOME` is ignored, as the XDG specification says.
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_dir(set: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    set("XDG_CACHE_HOME")
        .filter(|dir| dir.is_absolute())
        .or_else(|| set("HOME").map(|home| home.join(".cache")))
}

/// The platform's per-user cache directory, by the `dirs` crate's rules.
#[cfg(windows)]
fn platform_cache_dir(set: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    set("LOCALAPPDATA")
}

/// No per-user cache directory is known on this platform.
#[cfg(not(any(unix, windows)))]
fn platform_cache_dir(_set: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    None
}

/// The directory name of `server` under the slot directory: every byte
/// other than a lowercase ASCII letter, a digit, `.` and `-` written as
/// `%XX`, and the dots too when the name is nothing but dots, so that no
/// name is special to the file system or steps outside the directory.
fn server_dir_name(server: &str) -> String {
    if server.is_empty() {
        return "%".to_string();
    }
    let only_dots = server.bytes().all(|b| b == b'.');
    server
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'0'..=b'9' | b'-' => char::from(b).to_string(),
            b'.' if !only_dots => ".".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Slot file `i` of `server` under `dir`.
fn slot_path(dir: &Path, server: &str, i: usize) -> PathBuf {
    dir.join(server_dir_name(server)).join(format!("slot-{i}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot on `server` if one comes within `wait`.
    async fn within(slots: &ServerSlots, server: &str, wait: Duration) -> Option<SlotGuard> {
        tokio::time::timeout(wait, slots.acquire(server))
            .await
            .ok()
            .flatten()
    }

    /// Whether a slot on `server` comes within a moment.
    async fn free(slots: &ServerSlots, server: &str) -> Option<SlotGuard> {
        within(slots, server, Duration::from_millis(100)).await
    }

    #[test]
    fn the_default_is_six_and_zero_means_one() {
        assert_eq!(ServerSlots::default().max(), 6);
        assert_eq!(MAX_CONNECTIONS_PER_SERVER, 6);
        assert_eq!(ServerSlots::with_cap(0).max(), 1);
        assert_eq!(ServerSlots::with_cap(9).max(), 9);
    }

    #[tokio::test]
    async fn a_cap_too_large_for_a_semaphore_is_held_at_the_largest() {
        let slots = ServerSlots::with_cap(usize::MAX);
        assert_eq!(slots.max(), Semaphore::MAX_PERMITS);
        assert!(free(&slots, "a.example").await.is_some());
    }

    #[tokio::test]
    async fn a_full_server_makes_the_next_request_wait_until_a_slot_is_given_back() {
        let slots = ServerSlots::with_cap(2);
        let first = free(&slots, "a.example").await.expect("first slot");
        let _second = free(&slots, "a.example").await.expect("second slot");
        assert!(free(&slots, "a.example").await.is_none(), "a third slot");
        drop(first);
        assert!(free(&slots, "a.example").await.is_some(), "the freed slot");
    }

    #[tokio::test]
    async fn each_server_has_its_own_slots() {
        let slots = ServerSlots::with_cap(1);
        let _a = free(&slots, "a.example").await.expect("a");
        assert!(
            free(&slots, "b.example").await.is_some(),
            "b is another server"
        );
        assert!(
            free(&slots, "a.example:8080").await.is_some(),
            "another port is another server"
        );
    }

    #[tokio::test]
    async fn clones_share_the_slots_and_new_tables_do_not() {
        let slots = ServerSlots::with_cap(1);
        let _held = free(&slots, "a.example").await.expect("slot");
        assert!(free(&slots.clone(), "a.example").await.is_none(), "a clone");
        assert!(
            free(&ServerSlots::with_cap(1), "a.example").await.is_some(),
            "a new table"
        );
    }

    // -- the live budget: back-off and regrowth --

    const S: &str = "s.example";

    /// Take `n` slots on [`S`], each at once.
    async fn take(slots: &ServerSlots, n: usize) -> Vec<SlotGuard> {
        let mut held = Vec::new();
        for i in 0..n {
            held.push(free(slots, S).await.unwrap_or_else(|| panic!("slot {i}")));
        }
        held
    }

    /// How many more slots on [`S`] come at once (they are given back).
    async fn available(slots: &ServerSlots) -> usize {
        let mut held = Vec::new();
        while let Some(g) = free(slots, S).await {
            held.push(g);
        }
        held.len()
    }

    #[tokio::test]
    async fn a_429_or_a_5xx_halves_the_budget_down_to_one() {
        let slots = ServerSlots::default();
        for (status, after) in [(429, 3), (503, 2), (500, 1), (429, 1), (599, 1)] {
            let g = free(&slots, S).await.unwrap();
            slots.report(&g, S, status);
            drop(g);
            assert_eq!(slots.budget(S), after, "after {status}");
        }
        assert_eq!(available(&slots).await, 1);
    }

    #[tokio::test]
    async fn other_statuses_leave_the_budget_alone() {
        let slots = ServerSlots::default();
        let g = free(&slots, S).await.unwrap();
        for status in [200, 206, 301, 404, 416, 428, 600] {
            slots.report(&g, S, status);
        }
        drop(g);
        assert_eq!(slots.budget(S), 6);
        assert_eq!(available(&slots).await, 6);
    }

    #[tokio::test]
    async fn one_round_of_answers_drops_the_budget_once() {
        let slots = ServerSlots::default();
        let held = take(&slots, 6).await;
        // Six connections answered 429 together: one drop, not three.
        for g in &held {
            slots.report(g, S, 429);
        }
        assert_eq!(slots.budget(S), 3);
        // A slot taken after the drop counts again.
        drop(held);
        let g = free(&slots, S).await.unwrap();
        slots.report(&g, S, 429);
        assert_eq!(slots.budget(S), 2);
    }

    #[tokio::test]
    async fn slots_in_use_over_the_new_budget_are_forgotten_as_they_return() {
        let slots = ServerSlots::default();
        let held = take(&slots, 5).await;
        slots.report(&held[0], S, 429);
        // One permit was free and is forgotten now; two of the five in use
        // are owed. The budget of 3 is full until those are back.
        assert!(free(&slots, S).await.is_none(), "a slot over the budget");
        drop(held);
        assert_eq!(available(&slots).await, 3);
    }

    #[tokio::test]
    async fn range_streams_over_the_budget_are_asked_to_stop() {
        let slots = ServerSlots::default();
        let held = take(&slots, 6).await;
        slots.report(&held[0], S, 503);
        let asked = held.iter().filter(|g| g.take_shed()).count();
        assert_eq!(asked, 3, "6 in use over a budget of 3");
        assert!(!held[0].take_shed(), "no more than that");

        // Slots given back lower the count still to stop.
        let held2 = take(&ServerSlots::default(), 0).await;
        drop(held2);
        let slots = ServerSlots::default();
        let mut held = take(&slots, 6).await;
        slots.report(&held[0], S, 503);
        held.truncate(4); // two whole files ended
        assert_eq!(held.iter().filter(|g| g.take_shed()).count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_grows_by_one_per_calm_interval_up_to_the_cap() {
        let slots = ServerSlots::default();
        let g = free(&slots, S).await.unwrap();
        slots.report(&g, S, 429);
        drop(g);
        assert_eq!(slots.budget(S), 3);

        tokio::time::advance(GROWTH_INTERVAL - Duration::from_secs(1)).await;
        assert_eq!(slots.budget(S), 3, "before a full interval");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(slots.budget(S), 4);
        assert_eq!(available(&slots).await, 4);

        tokio::time::advance(GROWTH_INTERVAL * 10).await;
        assert_eq!(slots.budget(S), 6, "never above the cap");
        assert_eq!(available(&slots).await, 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_429_or_5xx_restarts_the_calm_interval() {
        let slots = ServerSlots::default();
        let g = free(&slots, S).await.unwrap();
        slots.report(&g, S, 429); // 3
        drop(g);
        tokio::time::advance(GROWTH_INTERVAL / 2).await;
        // A late answer from a slot of the old round does not drop the
        // budget again, but the server is still in distress.
        let stale = SlotGuard {
            server: S.to_string(),
            node: slots.node(S),
            epoch: 0,
            permit: None,
            file: None,
        };
        slots.report(&stale, S, 503);
        tokio::time::advance(GROWTH_INTERVAL / 2 + Duration::from_secs(1)).await;
        assert_eq!(slots.budget(S), 3, "the interval started again at the 503");
        tokio::time::advance(GROWTH_INTERVAL / 2).await;
        assert_eq!(slots.budget(S), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_waiting_for_a_slot_gets_one_when_the_budget_grows() {
        let slots = ServerSlots::default();
        let held = take(&slots, 6).await;
        slots.report(&held[0], S, 429);
        let one_left = held.into_iter().take(3).collect::<Vec<_>>();
        drop(one_left); // 3 held over budget 3? no: all 6 returned or owed
        assert_eq!(slots.budget(S), 3);
        let held = take(&slots, 3).await;
        let waiter = {
            let slots = slots.clone();
            tokio::spawn(async move { slots.acquire(S).await.is_some() })
        };
        tokio::time::advance(GROWTH_INTERVAL + Duration::from_millis(1)).await;
        assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("the waiter got no slot when the budget grew")
            .unwrap());
        drop(held);
    }

    #[tokio::test]
    async fn every_drop_is_noticed_once() {
        let seen: Arc<Mutex<Vec<BudgetDrop>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let slots =
            ServerSlots::default().with_notices(move |d| sink.lock().unwrap().push(d.clone()));
        let held = take(&slots, 6).await;
        for g in &held {
            slots.report(g, S, 429);
        }
        drop(held);
        for status in [503, 500, 429] {
            let g = free(&slots, S).await.unwrap();
            slots.report(&g, S, status);
        }
        let seen = seen.lock().unwrap();
        let drops: Vec<(u16, usize, usize)> =
            seen.iter().map(|d| (d.status, d.before, d.after)).collect();
        assert_eq!(drops, vec![(429, 6, 3), (503, 3, 2), (500, 2, 1)]);
        assert!(seen.iter().all(|d| d.server == S));
    }

    #[tokio::test]
    async fn room_is_the_free_slots_after_one_for_every_file_about_to_start() {
        let slots = ServerSlots::default();
        let _first = free(&slots, S).await.unwrap();
        assert_eq!(slots.room(S).await, 5);
        let a = slots.starting();
        let _b = slots.starting();
        assert_eq!(slots.room(S).await, 3);
        drop(a);
        assert_eq!(slots.room(S).await, 4);
        let _more = take(&slots, 5).await;
        assert_eq!(slots.room(S).await, 0);
    }

    #[tokio::test]
    async fn room_counts_slot_files_other_processes_hold() {
        let dir = tempfile::tempdir().unwrap();
        let other = ServerSlots::across_processes_with_cap(6, dir.path());
        let _theirs = take(&other, 4).await;
        let ours = ServerSlots::across_processes_with_cap(6, dir.path());
        assert_eq!(ours.room(S).await, 2);
    }

    // -- an item's datanode pair --

    fn item(d1: Option<&str>, d2: Option<&str>) -> crate::types::ItemMetadata {
        serde_json::from_value(serde_json::json!({
            "metadata": {"identifier": "x"},
            "files": [],
            "d1": d1,
            "d2": d2,
        }))
        .unwrap()
    }

    #[test]
    fn a_pair_is_its_two_hosts_sorted_and_joined() {
        let pair = Datanodes::of(&item(
            Some("ia800807.us.archive.org"),
            Some("IA600807.us.archive.org"),
        ))
        .expect("a pair");
        assert_eq!(pair.key, "ia600807.us.archive.org+ia800807.us.archive.org");
        assert_eq!(
            Datanodes::of(&item(
                Some("ia600807.us.archive.org"),
                Some("ia800807.us.archive.org")
            )),
            Some(pair.clone()),
            "the order of d1 and d2 does not matter"
        );
        assert_eq!(
            pair.key_for("ia800807.us.archive.org"),
            Some(pair.key.as_str())
        );
        assert_eq!(
            pair.key_for("IA600807.US.ARCHIVE.ORG"),
            Some(pair.key.as_str())
        );
        assert_eq!(pair.key_for("archive.org"), None);
        assert_eq!(pair.key_for("ia800808.us.archive.org"), None);
    }

    #[test]
    fn without_both_datanodes_there_is_no_pair() {
        assert_eq!(
            Datanodes::of(&item(Some("ia800807.us.archive.org"), None)),
            None
        );
        assert_eq!(
            Datanodes::of(&item(None, Some("ia800807.us.archive.org"))),
            None
        );
        assert_eq!(
            Datanodes::of(&item(Some(" "), Some("ia800807.us.archive.org"))),
            None
        );
        let one = Datanodes::of(&item(
            Some("ia800807.us.archive.org"),
            Some("ia800807.us.archive.org"),
        ))
        .unwrap();
        assert_eq!(one.key, "ia800807.us.archive.org");
    }

    // -- across processes --

    #[test]
    fn server_names_keep_hosts_and_encode_everything_else() {
        assert_eq!(
            server_dir_name("ia800807.us.archive.org"),
            "ia800807.us.archive.org"
        );
        assert_eq!(server_dir_name("127.0.0.1:8080"), "127.0.0.1%3A8080");
        assert_eq!(server_dir_name("[::1]:80"), "%5B%3A%3A1%5D%3A80");
        assert_eq!(server_dir_name("a-b.example"), "a-b.example");
        assert_eq!(server_dir_name("A.example"), "%41.example");
        assert_eq!(server_dir_name("a/b\\c"), "a%2Fb%5Cc");
        // Nothing that names this directory or its parent.
        assert_eq!(server_dir_name("."), "%2E");
        assert_eq!(server_dir_name(".."), "%2E%2E");
        assert_eq!(server_dir_name(""), "%");
    }

    #[test]
    fn slot_files_sit_under_one_directory_per_server() {
        let dir = Path::new("/cache/ia/connections");
        assert_eq!(
            slot_path(dir, "127.0.0.1:8080", 0),
            dir.join("127.0.0.1%3A8080").join("slot-0")
        );
        assert_eq!(
            slot_path(dir, "ia800807.us.archive.org", 5),
            dir.join("ia800807.us.archive.org").join("slot-5")
        );
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn ia_cache_dir_wins_and_empty_values_do_not_count() {
        assert_eq!(
            cache_root_from(vars(&[("IA_CACHE_DIR", "/x"), ("HOME", "/h")])),
            Some(PathBuf::from("/x"))
        );
        let unset = cache_root_from(vars(&[("IA_CACHE_DIR", ""), ("HOME", "/h")]));
        #[cfg(target_os = "macos")]
        assert_eq!(unset, Some(PathBuf::from("/h/Library/Caches/ia")));
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(unset, Some(PathBuf::from("/h/.cache/ia")));
        #[cfg(windows)]
        assert_eq!(unset, None);
        assert_eq!(cache_root_from(vars(&[])), None);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_cache_root_follows_xdg_then_home() {
        assert_eq!(
            cache_root_from(vars(&[("XDG_CACHE_HOME", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/c/ia"))
        );
        assert_eq!(
            cache_root_from(vars(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.cache/ia"))
        );
        // A relative XDG_CACHE_HOME is ignored, as the XDG rules say.
        assert_eq!(
            cache_root_from(vars(&[("XDG_CACHE_HOME", "c"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.cache/ia"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cache_root_is_library_caches() {
        assert_eq!(
            cache_root_from(vars(&[("XDG_CACHE_HOME", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/Library/Caches/ia"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cache_root_is_local_app_data() {
        assert_eq!(
            cache_root_from(vars(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")])),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local\ia"))
        );
    }

    /// Two tables on one directory stand for two processes: advisory locks
    /// are held per open file, so two opens in one process conflict as two
    /// processes' do.
    #[tokio::test]
    async fn two_processes_never_hold_more_than_the_cap_together() {
        let dir = tempfile::tempdir().unwrap();
        let a = ServerSlots::across_processes_with_cap(2, dir.path());
        let b = ServerSlots::across_processes_with_cap(2, dir.path());
        let first = free(&a, "s.example").await.expect("a first");
        let _second = free(&a, "s.example").await.expect("a second");

        assert!(
            within(&b, "s.example", Duration::from_millis(700))
                .await
                .is_none(),
            "b took a slot while a held both"
        );
        drop(first);
        assert!(
            within(&b, "s.example", Duration::from_secs(3))
                .await
                .is_some(),
            "b never took the slot a gave back"
        );
        assert!(
            free(&b, "t.example").await.is_some(),
            "another server has its own files"
        );
    }

    #[tokio::test]
    async fn a_larger_cap_uses_files_beyond_a_smaller_ones() {
        let dir = tempfile::tempdir().unwrap();
        let small = ServerSlots::across_processes_with_cap(2, dir.path());
        let large = ServerSlots::across_processes_with_cap(3, dir.path());
        let _s1 = free(&small, "s.example").await.expect("small 1");
        let _s2 = free(&small, "s.example").await.expect("small 2");
        let _l = free(&large, "s.example").await.expect("large takes slot-2");
        assert!(
            within(&large, "s.example", Duration::from_millis(700))
                .await
                .is_none(),
            "large holds 3 together with small"
        );
        let mut names: Vec<String> =
            std::fs::read_dir(dir.path().join(server_dir_name("s.example")))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        names.sort();
        assert_eq!(names, vec!["slot-0", "slot-1", "slot-2"]);
    }

    #[tokio::test]
    async fn a_slot_file_is_locked_while_held_and_kept_after() {
        use fs4::{FileExt, TryLockError};
        let dir = tempfile::tempdir().unwrap();
        let slots = ServerSlots::across_processes_with_cap(1, dir.path());
        let path = slot_path(dir.path(), "s.example", 0);

        let held = free(&slots, "s.example").await.expect("slot");
        let probe = std::fs::File::open(&path).unwrap();
        assert!(matches!(
            FileExt::try_lock(&probe),
            Err(TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(
            FileExt::try_lock(&probe).is_ok(),
            "the lock went with the slot"
        );
        FileExt::unlock(&probe).unwrap();
        assert!(path.exists(), "slot files are never removed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_slot_file_this_user_may_not_write_is_locked_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = slot_path(dir.path(), "s.example", 0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let slots = ServerSlots::across_processes_with_cap(1, dir.path());

        let held = free(&slots, "s.example").await.expect("a slot");

        assert!(
            !slots.inner.files_failed.load(Ordering::SeqCst),
            "the read-only file was given up"
        );
        assert!(held.file.is_some(), "no slot file is locked");
    }

    #[tokio::test]
    async fn a_slot_directory_that_cannot_be_made_falls_back_to_the_process_cap() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        let slots = ServerSlots::across_processes_with_cap(1, &file);

        let held = free(&slots, "s.example")
            .await
            .expect("a slot all the same");
        assert!(
            free(&slots, "s.example").await.is_none(),
            "the cap still holds within the process"
        );
        drop(held);
        assert!(free(&slots, "s.example").await.is_some());
        assert!(
            slots
                .inner
                .files_failed
                .load(std::sync::atomic::Ordering::SeqCst),
            "the files are given up for the rest of the run"
        );
    }
}
