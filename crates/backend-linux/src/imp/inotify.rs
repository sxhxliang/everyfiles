//! inotify watcher: the unprivileged Linux path.
//!
//! inotify has no recursive mode, so one watch descriptor is added per
//! directory, and newly created or moved-in directories get watches as
//! their events arrive. Two honest limits are built in rather than hidden:
//!
//! - **Watch budget.** `/proc/sys/fs/inotify/max_user_watches` caps the
//!   descriptors a user may hold (often 65536, sometimes 8192). Directories
//!   that cannot be watched are counted, and while any exist the watcher
//!   emits a periodic `RescanNeeded` so the engine's sweep covers the gap.
//! - **No replay.** Descriptors and rename cookies are per-instance, so there
//!   is no resume token; the engine sweeps on start.
//!
//! Queue overflow arrives as an event with `wd == -1` and is surfaced as
//! `Overflow`. Rename pairing by cookie is deliberately not attempted: the
//! engine reconciles both the old and new paths against the filesystem,
//! which is correct whether or not the pair arrives together.
//!
//! There is no `unsafe` in this file; `rustix` wraps the syscalls.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{EventKind, PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};
use everyfiles_core::{Error, Result};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

const BUF_SIZE: usize = 64 * 1024;
/// How often to request a sweep while some directories are unwatched.
const GAP_NOTICE_EVERY: Duration = Duration::from_secs(60);
/// Idle sleep between non-blocking reads.
const IDLE: Duration = Duration::from_millis(20);

#[derive(Debug, Default)]
struct Coverage {
    watched: AtomicU64,
    unwatched: AtomicU64,
}

/// Recursive-by-emulation inotify watcher. Unprivileged.
pub struct InotifyWatcher {
    worker: Option<JoinHandle<()>>,
    rx: Option<Receiver<WatchEvent>>,
    stop: Arc<AtomicBool>,
    coverage: Arc<Coverage>,
}

impl std::fmt::Debug for InotifyWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InotifyWatcher")
            .field("running", &self.worker.is_some())
            .field("watched", &self.coverage.watched.load(Ordering::Relaxed))
            .field(
                "unwatched",
                &self.coverage.unwatched.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl Default for InotifyWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl InotifyWatcher {
    pub fn new() -> Self {
        Self {
            worker: None,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            coverage: Arc::new(Coverage::default()),
        }
    }

    /// The per-user watch descriptor budget, if the kernel exposes it.
    pub fn max_user_watches() -> Option<u64> {
        std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// `(watched, unwatched)` directory counts so far.
    pub fn coverage(&self) -> (u64, u64) {
        (
            self.coverage.watched.load(Ordering::Relaxed),
            self.coverage.unwatched.load(Ordering::Relaxed),
        )
    }
}

fn watch_flags() -> WatchFlags {
    WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MODIFY
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO
        | WatchFlags::ATTRIB
        | WatchFlags::CLOSE_WRITE
        | WatchFlags::DELETE_SELF
        | WatchFlags::MOVE_SELF
        | WatchFlags::ONLYDIR
        | WatchFlags::DONT_FOLLOW
        | WatchFlags::EXCL_UNLINK
}

/// Add watches for `dir` and every directory below it (symlinks not followed).
fn add_tree(fd: &OwnedFd, dir: &Path, dirs: &mut HashMap<i32, PathBuf>, cov: &Coverage) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        match inotify::add_watch(fd, &d, watch_flags()) {
            Ok(wd) => {
                dirs.insert(wd, d.clone());
                cov.watched.fetch_add(1, Ordering::Relaxed);
            }
            // ENOSPC is the watch budget; anything else is a vanished or
            // unreadable directory. Either way it is counted, not fatal.
            Err(_) => {
                cov.unwatched.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(e.path());
                }
            }
        }
    }
}

fn worker(
    root: PathBuf,
    fd: OwnedFd,
    tx: Sender<WatchEvent>,
    stop: Arc<AtomicBool>,
    cov: Arc<Coverage>,
) {
    let mut dirs: HashMap<i32, PathBuf> = HashMap::new();
    add_tree(&fd, &root, &mut dirs, &cov);
    let mut buf: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); BUF_SIZE];
    let mut last_gap_notice = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let mut pending_dirs: Vec<PathBuf> = Vec::new();
        {
            let mut reader = inotify::Reader::new(&fd, &mut buf);
            loop {
                match reader.next() {
                    Ok(ev) => {
                        let wd = ev.wd();
                        if wd == -1 {
                            let _ = tx.send(WatchEvent::Overflow);
                            continue;
                        }
                        let flags = ev.events();
                        if flags.contains(ReadFlags::IGNORED) {
                            dirs.remove(&wd);
                            continue;
                        }
                        let Some(dir) = dirs.get(&wd) else { continue };
                        let path = match ev.file_name() {
                            Some(n) => dir.join(OsStr::from_bytes(n.to_bytes())),
                            None => dir.clone(),
                        };
                        let is_dir = flags.contains(ReadFlags::ISDIR);
                        let arrived = flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO);
                        let kind = if flags.contains(ReadFlags::CREATE) {
                            EventKind::Created
                        } else if flags.intersects(ReadFlags::DELETE | ReadFlags::DELETE_SELF) {
                            EventKind::Removed
                        } else if flags.intersects(
                            ReadFlags::MOVED_FROM | ReadFlags::MOVED_TO | ReadFlags::MOVE_SELF,
                        ) {
                            EventKind::Renamed
                        } else if flags.intersects(ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE) {
                            EventKind::Modified
                        } else if flags.contains(ReadFlags::ATTRIB) {
                            EventKind::Attrib
                        } else {
                            EventKind::Unknown
                        };
                        if is_dir && arrived {
                            pending_dirs.push(path.clone());
                        }
                        let _ = tx.send(WatchEvent::Path {
                            path,
                            kind,
                            deep: is_dir && arrived,
                        });
                    }
                    Err(Errno::WOULDBLOCK) => break,
                    Err(_) => {
                        let _ = tx.send(WatchEvent::RescanNeeded);
                        return;
                    }
                }
            }
        }
        // Watch directories that appeared (and anything already inside them).
        for d in pending_dirs {
            add_tree(&fd, &d, &mut dirs, &cov);
        }
        if cov.unwatched.load(Ordering::Relaxed) > 0
            && last_gap_notice.elapsed() >= GAP_NOTICE_EVERY
        {
            let _ = tx.send(WatchEvent::RescanNeeded);
            last_gap_notice = Instant::now();
        }
        std::thread::sleep(IDLE);
    }
}

impl Watcher for InotifyWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: false,
            needs_privilege: false,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
            .map_err(|e| Error::io(root, e.into()))?;
        inotify::add_watch(&fd, root, watch_flags()).map_err(|e| Error::io(root, e.into()))?;
        Ok(ProbeReport {
            granted: true,
            active: "inotify",
            reason: Some("one watch per directory; no replay across restarts"),
            kernel_queue_bytes: Self::max_user_watches(),
        })
    }

    fn start(&mut self, root: &Path, _since: Option<&ResumeToken>) -> Result<()> {
        if self.worker.is_some() {
            self.stop()?;
        }
        let root = std::path::absolute(root)?;
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
            .map_err(|e| Error::io(&root, e.into()))?;
        let (tx, rx) = bounded(16 * 1024);
        self.stop = Arc::new(AtomicBool::new(false));
        self.coverage = Arc::new(Coverage::default());
        let stop = Arc::clone(&self.stop);
        let cov = Arc::clone(&self.coverage);
        self.worker = Some(std::thread::spawn(move || worker(root, fd, tx, stop, cov)));
        self.rx = Some(rx);
        Ok(())
    }

    fn poll(&mut self, timeout: Duration, out: &mut Vec<WatchEvent>) -> Result<PollOutcome> {
        let Some(rx) = &self.rx else {
            return Ok(PollOutcome::Closed);
        };
        match rx.recv_timeout(timeout) {
            Ok(ev) => {
                out.push(ev);
                let mut n = 1;
                while let Ok(ev) = rx.try_recv() {
                    out.push(ev);
                    n += 1;
                    if n >= 8192 {
                        break;
                    }
                }
                Ok(PollOutcome::Events(n))
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(PollOutcome::Timeout),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Ok(PollOutcome::Closed),
        }
    }

    fn resume_token(&self) -> ResumeToken {
        ResumeToken::default()
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.worker.take() {
            let _ = j.join();
        }
        self.rx = None;
        Ok(())
    }
}

impl Drop for InotifyWatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
