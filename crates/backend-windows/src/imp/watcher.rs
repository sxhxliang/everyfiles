//! `ReadDirectoryChangesW` watcher: unprivileged, recursive, no resume.
//!
//! A dedicated thread blocks in the synchronous call and forwards parsed
//! `FILE_NOTIFY_INFORMATION` records over a channel. `stop` unblocks it with
//! `CancelIoEx` on the directory handle. The call reports `ERROR_NOTIFY_ENUM_DIR`
//! when its buffer overflowed, which is surfaced as `Overflow` so the engine
//! sweeps rather than silently missing changes. Events carry no size or
//! mtime; the engine stats each path once, which is fine at update rates.
//!
//! The USN journal (elevated) is the resumable alternative and lands as a
//! separate tier.

use std::ffi::OsString;
use std::mem::offset_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{EventKind, PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};
use everyfiles_core::{Error, Result};
use windows_sys::Win32::Foundation::{
    ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, GetLastError,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::CancelIoEx;

use crate::imp::sys;

const BUF_SIZE: usize = 64 * 1024;
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// Raw handle value, shared with the worker so `stop` can cancel its I/O.
/// Handles are process-wide integers; only the owner closes it.
#[derive(Clone, Copy)]
struct HandleId(isize);
// SAFETY: it is just an integer identifying a kernel object.
unsafe impl Send for HandleId {}
unsafe impl Sync for HandleId {}

pub struct RdcwWatcher {
    worker: Option<JoinHandle<()>>,
    rx: Option<Receiver<WatchEvent>>,
    stop: Arc<AtomicBool>,
    handle: Option<HandleId>,
}

impl std::fmt::Debug for RdcwWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdcwWatcher")
            .field("running", &self.worker.is_some())
            .finish()
    }
}

impl Default for RdcwWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RdcwWatcher {
    pub fn new() -> Self {
        Self {
            worker: None,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }
}

fn worker(root: PathBuf, h: OwnedHandle, tx: Sender<WatchEvent>, stop: Arc<AtomicBool>) {
    let mut buf = vec![0u8; BUF_SIZE];
    while !stop.load(Ordering::Relaxed) {
        let mut returned = 0u32;
        // SAFETY: `h` is a live directory handle opened with
        // FILE_LIST_DIRECTORY | FILE_FLAG_BACKUP_SEMANTICS; `buf` is writable
        // for BUF_SIZE bytes, the length we pass; synchronous mode (no
        // OVERLAPPED, no completion routine) is permitted by the API.
        let ok = unsafe {
            ReadDirectoryChangesW(
                h.as_raw_handle(),
                buf.as_mut_ptr().cast(),
                BUF_SIZE as u32,
                1,
                FILTER,
                &mut returned,
                std::ptr::null_mut(),
                None,
            )
        };
        if ok == 0 {
            // SAFETY: trivially safe FFI call.
            let err = unsafe { GetLastError() };
            if err == ERROR_OPERATION_ABORTED || stop.load(Ordering::Relaxed) {
                break;
            }
            if err == ERROR_NOTIFY_ENUM_DIR {
                let _ = tx.send(WatchEvent::Overflow);
                continue;
            }
            let _ = tx.send(WatchEvent::RescanNeeded);
            break;
        }
        if returned == 0 {
            // Too many changes for the buffer: the kernel reports nothing.
            let _ = tx.send(WatchEvent::Overflow);
            continue;
        }
        if parse_notifications(&buf[..returned as usize], &root, &tx).is_err() {
            let _ = tx.send(WatchEvent::Overflow);
        }
    }
}

fn parse_notifications(
    buf: &[u8],
    root: &Path,
    tx: &Sender<WatchEvent>,
) -> std::result::Result<(), ()> {
    const HDR: usize = size_of::<FILE_NOTIFY_INFORMATION>();
    const NAME_OFF: usize = offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    let mut pos = 0usize;
    loop {
        if pos + HDR > buf.len() {
            return Err(());
        }
        // SAFETY: bounds checked above; every field is a plain integer, so
        // reading any bit pattern unaligned is sound.
        let info: FILE_NOTIFY_INFORMATION =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(pos).cast()) };
        let n = info.FileNameLength as usize;
        let start = pos + NAME_OFF;
        let end = start.checked_add(n).ok_or(())?;
        if n % 2 != 0 || end > buf.len() {
            return Err(());
        }
        let units: Vec<u16> = buf[start..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let rel = OsString::from_wide(&units);
        let kind = match info.Action {
            FILE_ACTION_ADDED => EventKind::Created,
            FILE_ACTION_REMOVED => EventKind::Removed,
            FILE_ACTION_MODIFIED => EventKind::Modified,
            FILE_ACTION_RENAMED_OLD_NAME | FILE_ACTION_RENAMED_NEW_NAME => EventKind::Renamed,
            _ => EventKind::Unknown,
        };
        let _ = tx.send(WatchEvent::Path {
            path: root.join(rel),
            kind,
            deep: false,
        });
        let next = info.NextEntryOffset as usize;
        if next == 0 {
            return Ok(());
        }
        pos = pos.checked_add(next).ok_or(())?;
    }
}

impl Watcher for RdcwWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: false,
            needs_privilege: false,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        sys::open_dir(root).map_err(|e| Error::io(root, e))?;
        Ok(ProbeReport {
            granted: true,
            active: "read-directory-changes",
            reason: Some("no replay across restarts; the engine sweeps on start"),
            kernel_queue_bytes: Some(BUF_SIZE as u64),
        })
    }

    fn start(&mut self, root: &Path, _since: Option<&ResumeToken>) -> Result<()> {
        if self.worker.is_some() {
            self.stop()?;
        }
        let root = std::path::absolute(root)?;
        let h = sys::open_dir(&root).map_err(|e| Error::io(&root, e))?;
        let (tx, rx) = bounded(16 * 1024);
        self.stop = Arc::new(AtomicBool::new(false));
        self.handle = Some(HandleId(h.as_raw_handle() as isize));
        let stop = Arc::clone(&self.stop);
        self.worker = Some(std::thread::spawn(move || worker(root, h, tx, stop)));
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
        if let Some(HandleId(h)) = self.handle.take() {
            // SAFETY: cancelling pending I/O on a handle the worker still owns
            // is the documented way to unblock a synchronous
            // ReadDirectoryChangesW from another thread. A failure (nothing
            // pending) is harmless.
            unsafe { CancelIoEx(h as RawHandle, std::ptr::null()) };
        }
        if let Some(j) = self.worker.take() {
            let _ = j.join();
        }
        self.rx = None;
        Ok(())
    }
}

impl Drop for RdcwWatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
