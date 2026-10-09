//! fanotify watcher: the privileged Linux path.
//!
//! One `FAN_MARK_FILESYSTEM` mark covers the whole filesystem, so there is no
//! per-directory watch budget and no tree walk at start. `FAN_REPORT_DFID_NAME`
//! makes each event carry the parent directory's file handle plus the entry
//! name, which `open_by_handle_at` turns into a path. All of that needs root:
//! `FAN_MARK_FILESYSTEM` requires `CAP_SYS_ADMIN` and `open_by_handle_at`
//! requires `CAP_DAC_READ_SEARCH`. Since Linux 5.13 an unprivileged process
//! may create an FID group but still may not mark a filesystem, so there is no
//! unprivileged variant of this design; [`super::LinuxWatcher`] falls back to
//! inotify instead.
//!
//! Kernel requirements: `FAN_MARK_FILESYSTEM` (4.20) and `FAN_REPORT_DFID_NAME`
//! (5.9). Older kernels fail `fanotify_init` with `EINVAL` and are reported as
//! such. fanotify has no replay, so there is no resume token; the engine
//! sweeps on start. Queue overflow arrives as `FAN_Q_OVERFLOW` and is
//! surfaced as `Overflow`.
//!
//! Constants are written from the kernel UAPI (`linux/fanotify.h`) rather
//! than taken from `libc`, which omits several of them.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{EventKind, PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};
use everyfiles_core::{Error, Result};

// fanotify_init flags
const FAN_CLOEXEC: u32 = 0x1;
const FAN_NONBLOCK: u32 = 0x2;
const FAN_CLASS_NOTIF: u32 = 0x0;
const FAN_REPORT_DIR_FID: u32 = 0x400;
const FAN_REPORT_NAME: u32 = 0x800;
const FAN_REPORT_DFID_NAME: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
// fanotify_mark flags
const FAN_MARK_ADD: u32 = 0x1;
const FAN_MARK_FILESYSTEM: u32 = 0x100;
// event mask bits
const FAN_ATTRIB: u64 = 0x4;
const FAN_CLOSE_WRITE: u64 = 0x8;
const FAN_MOVED_FROM: u64 = 0x40;
const FAN_MOVED_TO: u64 = 0x80;
const FAN_CREATE: u64 = 0x100;
const FAN_DELETE: u64 = 0x200;
const FAN_DELETE_SELF: u64 = 0x400;
const FAN_MOVE_SELF: u64 = 0x800;
const FAN_Q_OVERFLOW: u64 = 0x4000;
const FAN_ONDIR: u64 = 0x4000_0000;
// event info record types
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;
const FAN_EVENT_INFO_TYPE_OLD_DFID_NAME: u8 = 10;
const FAN_EVENT_INFO_TYPE_NEW_DFID_NAME: u8 = 12;
const FANOTIFY_METADATA_VERSION: u8 = 3;

const MASK: u64 = FAN_CREATE
    | FAN_DELETE
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_ATTRIB
    | FAN_CLOSE_WRITE
    | FAN_DELETE_SELF
    | FAN_MOVE_SELF
    | FAN_ONDIR;

const BUF_SIZE: usize = 256 * 1024;
const IDLE: Duration = Duration::from_millis(20);
/// Bound on the handle→path cache.
const CACHE_MAX: usize = 16 * 1024;

/// Whole-filesystem watcher. Requires root.
pub struct FanotifyWatcher {
    worker: Option<JoinHandle<()>>,
    rx: Option<Receiver<WatchEvent>>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for FanotifyWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FanotifyWatcher")
            .field("running", &self.worker.is_some())
            .finish()
    }
}

impl Default for FanotifyWatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Why fanotify is unavailable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Not root / no `CAP_SYS_ADMIN`.
    Privilege,
    /// Kernel too old for `FAN_REPORT_DFID_NAME` or `FAN_MARK_FILESYSTEM`.
    Kernel,
    Other(i32),
}

impl Unavailable {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Privilege => "fanotify needs root (CAP_SYS_ADMIN)",
            Self::Kernel => "kernel lacks fanotify FID reporting (needs 5.9+)",
            Self::Other(_) => "fanotify_init failed",
        }
    }
}

fn init_group() -> std::result::Result<OwnedFd, Unavailable> {
    // SAFETY: plain syscall wrapper with constant flags.
    let fd = unsafe {
        libc::fanotify_init(
            FAN_CLASS_NOTIF | FAN_CLOEXEC | FAN_NONBLOCK | FAN_REPORT_DFID_NAME,
            (libc::O_RDONLY | libc::O_LARGEFILE | libc::O_CLOEXEC) as u32,
        )
    };
    if fd >= 0 {
        // SAFETY: `fd` is a fresh descriptor we own.
        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    Err(match e {
        libc::EPERM => Unavailable::Privilege,
        libc::EINVAL | libc::ENOSYS => Unavailable::Kernel,
        other => Unavailable::Other(other),
    })
}

fn mark_filesystem(fan: &OwnedFd, root: &Path) -> std::result::Result<(), Unavailable> {
    let c =
        CString::new(root.as_os_str().as_bytes()).map_err(|_| Unavailable::Other(libc::EINVAL))?;
    // SAFETY: `fan` is a live fanotify fd; `c` is a valid NUL-terminated path.
    let r = unsafe {
        libc::fanotify_mark(
            fan.as_raw_fd(),
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            MASK,
            libc::AT_FDCWD,
            c.as_ptr(),
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    Err(match e {
        libc::EPERM => Unavailable::Privilege,
        libc::EINVAL => Unavailable::Kernel,
        other => Unavailable::Other(other),
    })
}

impl FanotifyWatcher {
    pub fn new() -> Self {
        Self {
            worker: None,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Can this process use fanotify on the filesystem holding `root`?
    pub fn availability(root: &Path) -> std::result::Result<(), Unavailable> {
        let fan = init_group()?;
        mark_filesystem(&fan, root)
    }
}

/// Resolve a kernel file handle (as laid out in the event buffer) to a path
/// through `open_by_handle_at` + `/proc/self/fd`.
fn resolve_handle(mount: &OwnedFd, handle: &[u8]) -> Option<PathBuf> {
    // SAFETY: `handle` points at a `struct file_handle` the kernel wrote
    // (header plus `handle_bytes` of payload, bounds-checked by the parser);
    // the kernel only reads it. `mount` is a live directory fd on the same
    // filesystem.
    let fd = unsafe {
        libc::open_by_handle_at(
            mount.as_raw_fd(),
            handle.as_ptr() as *mut libc::file_handle,
            libc::O_RDONLY | libc::O_PATH | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is ours; `OwnedFd` closes it.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    std::fs::read_link(format!("/proc/self/fd/{}", owned.as_raw_fd())).ok()
}

fn worker(
    root: PathBuf,
    fan: OwnedFd,
    mount: OwnedFd,
    tx: Sender<WatchEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; BUF_SIZE];
    let mut cache: HashMap<Vec<u8>, PathBuf> = HashMap::new();
    const MD: usize = size_of::<libc::fanotify_event_metadata>();

    while !stop.load(Ordering::Relaxed) {
        // SAFETY: `buf` is writable for BUF_SIZE bytes; the kernel writes at
        // most that many.
        let n = unsafe { libc::read(fan.as_raw_fd(), buf.as_mut_ptr().cast(), BUF_SIZE) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::Interrupted
            {
                std::thread::sleep(IDLE);
                continue;
            }
            let _ = tx.send(WatchEvent::RescanNeeded);
            return;
        }
        let n = n as usize;
        let mut off = 0usize;
        while off + MD <= n {
            // SAFETY: `off + MD <= n` was checked; the struct is plain integers.
            let md: libc::fanotify_event_metadata =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
            let ev_len = md.event_len as usize;
            let md_len = md.metadata_len as usize;
            if md.vers != FANOTIFY_METADATA_VERSION || ev_len < md_len || off + ev_len > n {
                let _ = tx.send(WatchEvent::RescanNeeded);
                return;
            }
            if md.fd >= 0 {
                // With FID reporting this is FAN_NOFD; close defensively.
                // SAFETY: closing a descriptor the kernel handed to us.
                unsafe { libc::close(md.fd) };
            }
            if md.mask & FAN_Q_OVERFLOW != 0 {
                let _ = tx.send(WatchEvent::Overflow);
            } else {
                let info = &buf[off + md_len..off + ev_len];
                for (handle, name) in parse_info(info) {
                    let dir = match cache.get(handle) {
                        Some(p) => p.clone(),
                        None => match resolve_handle(&mount, handle) {
                            Some(p) => {
                                if cache.len() >= CACHE_MAX {
                                    cache.clear();
                                }
                                cache.insert(handle.to_vec(), p.clone());
                                p
                            }
                            None => continue,
                        },
                    };
                    let path = match name {
                        Some(n) if n != b"." => dir.join(OsStr::from_bytes(n)),
                        _ => dir,
                    };
                    if !path.starts_with(&root) {
                        continue;
                    }
                    let m = md.mask;
                    let is_dir = m & FAN_ONDIR != 0;
                    let arrived = m & (FAN_CREATE | FAN_MOVED_TO) != 0;
                    let kind = if m & FAN_CREATE != 0 {
                        EventKind::Created
                    } else if m & (FAN_DELETE | FAN_DELETE_SELF) != 0 {
                        EventKind::Removed
                    } else if m & (FAN_MOVED_FROM | FAN_MOVED_TO | FAN_MOVE_SELF) != 0 {
                        EventKind::Renamed
                    } else if m & FAN_CLOSE_WRITE != 0 {
                        EventKind::Modified
                    } else if m & FAN_ATTRIB != 0 {
                        EventKind::Attrib
                    } else {
                        EventKind::Unknown
                    };
                    let _ = tx.send(WatchEvent::Path {
                        path,
                        kind,
                        deep: is_dir && arrived,
                    });
                }
            }
            off += ev_len;
        }
    }
}

/// Yield `(file_handle bytes, optional entry name)` for each directory-fid
/// info record in an event's info area.
fn parse_info(mut info: &[u8]) -> Vec<(&[u8], Option<&[u8]>)> {
    let mut out = Vec::with_capacity(2);
    while info.len() >= 4 {
        let ty = info[0];
        let len = u16::from_ne_bytes([info[2], info[3]]) as usize;
        if len < 4 || len > info.len() {
            break;
        }
        let rec = &info[..len];
        info = &info[len..];
        let carries_handle = matches!(
            ty,
            FAN_EVENT_INFO_TYPE_FID
                | FAN_EVENT_INFO_TYPE_DFID
                | FAN_EVENT_INFO_TYPE_DFID_NAME
                | FAN_EVENT_INFO_TYPE_OLD_DFID_NAME
                | FAN_EVENT_INFO_TYPE_NEW_DFID_NAME
        );
        if !carries_handle {
            continue;
        }
        // header(4) + fsid(8) + file_handle { handle_bytes u32, handle_type i32, f_handle[] }
        if rec.len() < 4 + 8 + 8 {
            continue;
        }
        let hb = u32::from_ne_bytes(rec[12..16].try_into().unwrap()) as usize;
        let handle_end = 12 + 8 + hb;
        if handle_end > rec.len() {
            continue;
        }
        let handle = &rec[12..handle_end];
        let name = if matches!(
            ty,
            FAN_EVENT_INFO_TYPE_DFID_NAME
                | FAN_EVENT_INFO_TYPE_OLD_DFID_NAME
                | FAN_EVENT_INFO_TYPE_NEW_DFID_NAME
        ) {
            CStr::from_bytes_until_nul(&rec[handle_end..])
                .ok()
                .map(CStr::to_bytes)
        } else {
            None
        };
        out.push((handle, name));
    }
    out
}

impl Watcher for FanotifyWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: false,
            needs_privilege: true,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        Ok(match Self::availability(root) {
            Ok(()) => ProbeReport {
                granted: true,
                active: "fanotify",
                reason: Some("whole-filesystem mark; no replay across restarts"),
                kernel_queue_bytes: None,
            },
            Err(u) => ProbeReport {
                granted: false,
                active: "inotify",
                reason: Some(u.reason()),
                kernel_queue_bytes: None,
            },
        })
    }

    fn start(&mut self, root: &Path, _since: Option<&ResumeToken>) -> Result<()> {
        if self.worker.is_some() {
            self.stop()?;
        }
        let root = std::path::absolute(root)?;
        let fan = init_group().map_err(|u| Error::NotSupported(u.reason()))?;
        mark_filesystem(&fan, &root).map_err(|u| Error::NotSupported(u.reason()))?;
        let mount = std::fs::File::open(&root).map_err(|e| Error::io(&root, e))?;
        let mount: OwnedFd = mount.into();
        let (tx, rx) = bounded(16 * 1024);
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop);
        self.worker = Some(std::thread::spawn(move || {
            worker(root, fan, mount, tx, stop)
        }));
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

impl Drop for FanotifyWatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one DFID_NAME info record the way the kernel lays it out.
    fn dfid_name(handle_payload: &[u8], name: &[u8]) -> Vec<u8> {
        let mut r = Vec::new();
        r.push(FAN_EVENT_INFO_TYPE_DFID_NAME);
        r.push(0);
        r.extend_from_slice(&[0, 0]); // len placeholder
        r.extend_from_slice(&[0u8; 8]); // fsid
        r.extend_from_slice(&(handle_payload.len() as u32).to_ne_bytes());
        r.extend_from_slice(&1i32.to_ne_bytes()); // handle_type
        r.extend_from_slice(handle_payload);
        r.extend_from_slice(name);
        r.push(0);
        while r.len() % 8 != 0 {
            r.push(0);
        }
        let len = r.len() as u16;
        r[2..4].copy_from_slice(&len.to_ne_bytes());
        r
    }

    #[test]
    fn parses_dfid_name_records() {
        let mut info = dfid_name(&[0xAA; 8], b"file.txt");
        info.extend(dfid_name(&[0xBB; 12], b"."));
        let got = parse_info(&info);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0.len(), 8 + 8);
        assert_eq!(got[0].1, Some(&b"file.txt"[..]));
        assert_eq!(got[1].1, Some(&b"."[..]));
        assert!(parse_info(&[0u8; 3]).is_empty());
        assert!(
            parse_info(&[2, 0, 0xff, 0xff]).is_empty(),
            "bogus length is ignored"
        );
    }
}
