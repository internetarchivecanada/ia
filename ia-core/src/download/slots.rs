//! The cap on range connections (`--max-connections-per-server`).
//!
//! IA operations asked for a cap on the connections one client opens for
//! parallel ranges (`--connections-per-file`). Every range request takes a
//! slot before it is sent and keeps it until its response is dropped; a
//! request that finds its server full waits for a slot. Files fetched on
//! one stream take no slot.
//!
//! Within a process the slots are a semaphore per server. Across processes
//! ([`ServerSlots::across_processes`]) each slot is also a file,
//! `<dir>/<server>/slot-<i>`, held by an exclusive advisory lock for as long
//! as the in-process slot is held; the kernel releases the lock when the
//! file is closed or the process ends, however it ends. See "Per-server
//! cap" and "Across processes" in
//! `docs/plans/2026-11-03-parallel-range-downloads.md`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The cap unless the caller sets another: IA operations' limit for one
/// client.
pub const DEFAULT_MAX_CONNECTIONS_PER_SERVER: usize = 6;

/// The first wait when every slot file is locked by another process.
const FIRST_POLL: Duration = Duration::from_millis(50);

/// The longest wait between two looks at the slot files.
const LAST_POLL: Duration = Duration::from_millis(500);

/// How long a request waits on other processes' slot files before one
/// warning says so, once per server: at the default log level a wait is
/// otherwise silent, and a suspended `ia` keeps its slots.
const WAIT_WARNING: Duration = Duration::from_secs(10);

/// A shared table of download slots, `max` per server.
///
/// Clones share the table, so every download that clones one
/// [`DownloadOpts`](super::DownloadOpts) shares the cap; a new
/// `ServerSlots` starts with every slot free. A server is a URL authority:
/// its host, and its port when the URL gives one.
///
/// [`ServerSlots::new`] keeps the cap within the process.
/// [`ServerSlots::across_processes`] also shares it with every other process
/// that uses the same directory.
///
/// ```
/// use ia_core::download::{DownloadOpts, ServerSlots};
///
/// let mut opts = DownloadOpts::default();
/// opts.server_slots = ServerSlots::new(2);
/// assert_eq!(opts.clone().server_slots.max(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct ServerSlots {
    inner: Arc<Table>,
}

#[derive(Debug)]
struct Table {
    max: usize,
    per_server: Mutex<HashMap<String, Arc<Semaphore>>>,
    /// The slot files' directory, when the cap is shared across processes.
    files: Option<PathBuf>,
    /// Set, with one warning, when the slot files cannot be used; the cap
    /// is then per process for the rest of the run.
    files_failed: AtomicBool,
    /// The servers a long wait on other processes has been reported for.
    waits_reported: Mutex<std::collections::HashSet<String>>,
}

/// A held slot: the in-process permit and, across processes, the locked
/// slot file. Dropping it gives both back.
#[derive(Debug)]
pub(crate) struct SlotGuard {
    permit: Option<OwnedSemaphorePermit>,
    file: Option<std::fs::File>,
}

impl Drop for SlotGuard {
    /// The file is unlocked and closed before the permit goes back. The
    /// next task of this process, woken by the permit, then finds the file
    /// free instead of waiting a poll for it; and on Windows the lock goes
    /// at once, where closing a handle releases its locks only when the
    /// system gets to it.
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Through the trait, as `try_lock` is (see `try_lock_one`).
            let _ = fs4::FileExt::unlock(&file);
        }
        drop(self.permit.take());
    }
}

impl ServerSlots {
    /// At most `max` download connections to each server at once, within
    /// this process. `0` is treated as `1`, and a value above tokio's
    /// [`Semaphore::MAX_PERMITS`] (more than any client can open) as that.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self {
            inner: Arc::new(Table {
                max: max.clamp(1, Semaphore::MAX_PERMITS),
                per_server: Mutex::new(HashMap::new()),
                files: None,
                files_failed: AtomicBool::new(false),
                waits_reported: Mutex::default(),
            }),
        }
    }

    /// At most `max` download connections to each server at once, counted
    /// across every process that uses slot files under `dir`, such as the
    /// one [`default_slot_dir`] names.
    ///
    /// Each slot is the file `<dir>/<server>/slot-<i>`, `i` below `max`,
    /// held by an exclusive advisory lock (`flock` on Unix, `LockFileEx` on
    /// Windows) for as long as the slot is held. The files are created when
    /// first needed and never removed. Processes with different caps share
    /// the files, and each tries only its own first `max`. If the files
    /// cannot be used (the directory cannot be created, or the file system
    /// does not lock), one warning is logged and the cap holds within this
    /// process only; a download never fails because of them.
    #[must_use]
    pub fn across_processes(max: usize, dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Table {
                max: max.clamp(1, Semaphore::MAX_PERMITS),
                per_server: Mutex::new(HashMap::new()),
                files: Some(dir.into()),
                files_failed: AtomicBool::new(false),
                waits_reported: Mutex::default(),
            }),
        }
    }

    /// The most connections to one server at once.
    #[must_use]
    pub fn max(&self) -> usize {
        self.inner.max
    }

    /// Wait for a slot on `server` and hold it until the guard is dropped.
    /// `None` only if the semaphore was closed, which this code never does.
    pub(crate) async fn acquire(&self, server: &str) -> Option<SlotGuard> {
        let semaphore = {
            let mut per_server = self
                .inner
                .per_server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(
                per_server
                    .entry(server.to_string())
                    .or_insert_with(|| Arc::new(Semaphore::new(self.inner.max))),
            )
        };
        if semaphore.available_permits() == 0 {
            tracing::debug!(
                server,
                max = self.inner.max,
                "waiting for a connection slot"
            );
        }
        let permit = semaphore.acquire_owned().await.ok()?;
        let file = self.lock_slot_file(server).await;
        Some(SlotGuard {
            permit: Some(permit),
            file,
        })
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
        let started = tokio::time::Instant::now();
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
                             --max-connections-per-server holds within this process only",
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
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONNECTIONS_PER_SERVER)
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
        assert_eq!(DEFAULT_MAX_CONNECTIONS_PER_SERVER, 6);
        assert_eq!(ServerSlots::new(0).max(), 1);
        assert_eq!(ServerSlots::new(9).max(), 9);
    }

    #[tokio::test]
    async fn a_cap_too_large_for_a_semaphore_is_held_at_the_largest() {
        let slots = ServerSlots::new(usize::MAX);
        assert_eq!(slots.max(), Semaphore::MAX_PERMITS);
        assert!(free(&slots, "a.example").await.is_some());
    }

    #[tokio::test]
    async fn a_full_server_makes_the_next_request_wait_until_a_slot_is_given_back() {
        let slots = ServerSlots::new(2);
        let first = free(&slots, "a.example").await.expect("first slot");
        let _second = free(&slots, "a.example").await.expect("second slot");
        assert!(free(&slots, "a.example").await.is_none(), "a third slot");
        drop(first);
        assert!(free(&slots, "a.example").await.is_some(), "the freed slot");
    }

    #[tokio::test]
    async fn each_server_has_its_own_slots() {
        let slots = ServerSlots::new(1);
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
        let slots = ServerSlots::new(1);
        let _held = free(&slots, "a.example").await.expect("slot");
        assert!(free(&slots.clone(), "a.example").await.is_none(), "a clone");
        assert!(
            free(&ServerSlots::new(1), "a.example").await.is_some(),
            "a new table"
        );
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
        let a = ServerSlots::across_processes(2, dir.path());
        let b = ServerSlots::across_processes(2, dir.path());
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
        let small = ServerSlots::across_processes(2, dir.path());
        let large = ServerSlots::across_processes(3, dir.path());
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
        let slots = ServerSlots::across_processes(1, dir.path());
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
        let slots = ServerSlots::across_processes(1, dir.path());

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
        let slots = ServerSlots::across_processes(1, &file);

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
