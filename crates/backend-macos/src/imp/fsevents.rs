//! FSEvents watcher with a persisted, replayable position.
//!
//! Hand-declared FFI rather than a crate: `fsevent-sys` is deprecated and
//! lacks `FSEventsCopyUUIDForDevice`, and `notify` discards event ids, which
//! the resume design needs. The surface is small: one stream, one callback,
//! a dispatch queue, and two CoreFoundation constructors for the path array.
//!
//! Facts the design rests on (from the SDK header):
//!
//! - Event ids are monotonic per *system*, across reboots; the database they
//!   index is per *volume*. So `(device UUID, last id)` is a valid resume
//!   token as long as the UUID still matches, and the stream replays
//!   historical events first, then a `HistoryDone` sentinel, then live ones.
//! - Without `kFSEventStreamCreateFlagUseCFTypes`, the callback receives a
//!   plain `char**` of UTF-8 paths. No CF parsing needed on the hot path.
//! - The stream is a *hint* channel, never an authoritative log: flags can be
//!   coalesced and events dropped. Every path event is therefore reconciled
//!   against the filesystem by the engine, and `MustScanSubDirs`, drops, and
//!   id wraps are surfaced explicitly.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{EventKind, PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};
use everyfiles_core::{Error, Result};

// ---- FFI ------------------------------------------------------------------

type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFUUIDRef = *const c_void;
type CFIndex = isize;
type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;

#[repr(C)]
struct FSEventStreamContext {
    version: CFIndex,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[repr(C)]
struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

/// `CFUUIDBytes` is sixteen `UInt8` fields; this is layout-identical.
#[repr(C)]
#[derive(Clone, Copy)]
struct CFUUIDBytes {
    bytes: [u8; 16],
}

type Callback = unsafe extern "C" fn(
    stream: *const c_void,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const u32,
    event_ids: *const u64,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: Callback,
        context: *const FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
    fn FSEventsCopyUUIDForDevice(dev: libc::dev_t) -> CFUUIDRef;
    fn FSEventsGetCurrentEventId() -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
    fn CFStringCreateWithFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const c_char,
    ) -> CFStringRef;
    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        num_values: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}

// Create flags.
const K_NO_DEFER: u32 = 0x02;
const K_WATCH_ROOT: u32 = 0x04;
const K_FILE_EVENTS: u32 = 0x10;
// Event flags.
const F_MUST_SCAN_SUBDIRS: u32 = 0x1;
const F_USER_DROPPED: u32 = 0x2;
const F_KERNEL_DROPPED: u32 = 0x4;
const F_EVENT_IDS_WRAPPED: u32 = 0x8;
const F_HISTORY_DONE: u32 = 0x10;
const F_ROOT_CHANGED: u32 = 0x20;
const F_ITEM_CREATED: u32 = 0x100;
const F_ITEM_REMOVED: u32 = 0x200;
const F_ITEM_INODE_META_MOD: u32 = 0x400;
const F_ITEM_RENAMED: u32 = 0x800;
const F_ITEM_MODIFIED: u32 = 0x1000;
const F_ITEM_CHANGE_OWNER: u32 = 0x4000;
const F_ITEM_XATTR_MOD: u32 = 0x8000;

const K_EVENT_ID_SINCE_NOW: u64 = 0xFFFF_FFFF_FFFF_FFFF;

// ---- callback state ---------------------------------------------------------

struct Shared {
    tx: Sender<WatchEvent>,
    latest: AtomicU64,
}

unsafe extern "C" fn callback(
    _stream: *const c_void,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const u32,
    event_ids: *const u64,
) {
    // SAFETY: `info` is the `*mut Shared` we leaked in `start` and only free
    // after `FSEventStreamInvalidate`, which guarantees no further callbacks;
    // the three arrays are `num_events` long per the FSEvents contract, and
    // without `UseCFTypes` `event_paths` is a `char**` of NUL-terminated
    // UTF-8 paths that live for the duration of the callback.
    let (shared, paths, flags, ids) = unsafe {
        (
            &*(info as *const Shared),
            std::slice::from_raw_parts(event_paths as *const *const c_char, num_events),
            std::slice::from_raw_parts(event_flags, num_events),
            std::slice::from_raw_parts(event_ids, num_events),
        )
    };
    for i in 0..num_events {
        let f = flags[i];
        shared.latest.fetch_max(ids[i], Ordering::Relaxed);
        if f & F_EVENT_IDS_WRAPPED != 0 || f & F_ROOT_CHANGED != 0 {
            let _ = shared.tx.send(WatchEvent::RescanNeeded);
            continue;
        }
        if f & (F_USER_DROPPED | F_KERNEL_DROPPED) != 0 {
            let _ = shared.tx.send(WatchEvent::Overflow);
            continue;
        }
        if f & F_HISTORY_DONE != 0 {
            continue;
        }
        // SAFETY: see above; each entry is a valid C string.
        let bytes = unsafe { CStr::from_ptr(paths[i]) }.to_bytes();
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        let kind = if f & F_ITEM_REMOVED != 0 {
            EventKind::Removed
        } else if f & F_ITEM_RENAMED != 0 {
            EventKind::Renamed
        } else if f & F_ITEM_CREATED != 0 {
            EventKind::Created
        } else if f & F_ITEM_MODIFIED != 0 {
            EventKind::Modified
        } else if f & (F_ITEM_INODE_META_MOD | F_ITEM_CHANGE_OWNER | F_ITEM_XATTR_MOD) != 0 {
            EventKind::Attrib
        } else {
            EventKind::Unknown
        };
        let _ = shared.tx.send(WatchEvent::Path {
            path,
            kind,
            deep: f & F_MUST_SCAN_SUBDIRS != 0,
        });
    }
}

// ---- watcher ------------------------------------------------------------------

struct Stream {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
    shared: *mut Shared,
}

/// FSEvents-backed watcher. Unprivileged.
pub struct FsEventsWatcher {
    latency: f64,
    stream: Option<Stream>,
    rx: Option<Receiver<WatchEvent>>,
    uuid: Option<[u8; 16]>,
    /// Last event id seen (or the id at start, before any event).
    latest_at_start: u64,
}

// SAFETY: the stream and queue are only touched from whichever thread owns
// this watcher (`start`/`stop`); FSEvents dispatches callbacks on its own
// queue and synchronizes internally; `shared` is accessed only through the
// channel and an atomic.
unsafe impl Send for FsEventsWatcher {}

impl std::fmt::Debug for FsEventsWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsEventsWatcher")
            .field("running", &self.stream.is_some())
            .finish()
    }
}

impl Default for FsEventsWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FsEventsWatcher {
    pub fn new() -> Self {
        Self {
            latency: 0.3,
            stream: None,
            rx: None,
            uuid: None,
            latest_at_start: 0,
        }
    }

    pub fn with_latency(mut self, seconds: f64) -> Self {
        self.latency = seconds.max(0.0);
        self
    }

    /// UUID of the volume holding `root`, or `None` for volumes FSEvents
    /// cannot identify (then only "since now" is possible).
    pub fn device_uuid(root: &Path) -> Result<Option<[u8; 16]>> {
        let md = std::fs::metadata(root).map_err(|e| Error::io(root, e))?;
        let dev = md.dev() as libc::dev_t;
        // SAFETY: plain FFI call; a NULL result is handled.
        let uuid = unsafe { FSEventsCopyUUIDForDevice(dev) };
        if uuid.is_null() {
            return Ok(None);
        }
        // SAFETY: `uuid` is a live CFUUIDRef we own; released below.
        let bytes = unsafe { CFUUIDGetUUIDBytes(uuid) }.bytes;
        unsafe { CFRelease(uuid) };
        Ok(Some(bytes))
    }

    fn latest(&self) -> u64 {
        match &self.stream {
            // SAFETY: `shared` is live while the stream exists.
            Some(s) => unsafe { &*s.shared }.latest.load(Ordering::Relaxed),
            None => self.latest_at_start,
        }
    }
}

fn encode_token(uuid: &[u8; 16], id: u64) -> ResumeToken {
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(uuid);
    v.extend_from_slice(&id.to_le_bytes());
    ResumeToken(v)
}

fn decode_token(t: &ResumeToken) -> Option<([u8; 16], u64)> {
    if t.0.len() != 24 {
        return None;
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&t.0[..16]);
    Some((uuid, u64::from_le_bytes(t.0[16..].try_into().ok()?)))
}

impl Watcher for FsEventsWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: true,
            needs_privilege: false,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        let uuid = Self::device_uuid(root)?;
        Ok(ProbeReport {
            granted: true,
            active: if uuid.is_some() {
                "fsevents+resume"
            } else {
                "fsevents"
            },
            reason: uuid
                .is_none()
                .then_some("volume has no FSEvents UUID; no replay"),
            kernel_queue_bytes: None,
        })
    }

    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()> {
        if self.stream.is_some() {
            self.stop()?;
        }
        let uuid = Self::device_uuid(root)?;
        let since_when = match since {
            None => K_EVENT_ID_SINCE_NOW,
            Some(t) => {
                let Some((want, id)) = decode_token(t) else {
                    return Err(Error::TokenStale);
                };
                match uuid {
                    Some(have) if have == want => id,
                    _ => return Err(Error::TokenStale),
                }
            }
        };
        self.uuid = uuid;
        // SAFETY: plain FFI call.
        self.latest_at_start = if since_when == K_EVENT_ID_SINCE_NOW {
            unsafe { FSEventsGetCurrentEventId() }
        } else {
            since_when
        };

        let (tx, rx) = bounded(16 * 1024);
        let shared = Box::into_raw(Box::new(Shared {
            tx,
            latest: AtomicU64::new(self.latest_at_start),
        }));
        let cpath = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| Error::io(root, std::io::Error::other("path contains NUL")))?;

        // SAFETY: every pointer handed to CoreFoundation is valid for the
        // call; `cf_path`/`paths` are released after the stream is created
        // (it retains what it needs); `ctx` outlives `FSEventStreamCreate`,
        // which copies it; `shared` stays alive until `stop`.
        let stream = unsafe {
            let cf_path =
                CFStringCreateWithFileSystemRepresentation(std::ptr::null(), cpath.as_ptr());
            if cf_path.is_null() {
                drop(Box::from_raw(shared));
                return Err(Error::io(
                    root,
                    std::io::Error::other("CFString creation failed"),
                ));
            }
            let values = [cf_path];
            let paths = CFArrayCreate(
                std::ptr::null(),
                values.as_ptr(),
                1,
                &raw const kCFTypeArrayCallBacks,
            );
            CFRelease(cf_path);
            if paths.is_null() {
                drop(Box::from_raw(shared));
                return Err(Error::io(
                    root,
                    std::io::Error::other("CFArray creation failed"),
                ));
            }
            let ctx = FSEventStreamContext {
                version: 0,
                info: shared.cast(),
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let s = FSEventStreamCreate(
                std::ptr::null(),
                callback,
                &ctx,
                paths,
                since_when,
                self.latency,
                K_FILE_EVENTS | K_NO_DEFER | K_WATCH_ROOT,
            );
            CFRelease(paths);
            if s.is_null() {
                drop(Box::from_raw(shared));
                return Err(Error::io(
                    root,
                    std::io::Error::other("FSEventStreamCreate failed"),
                ));
            }
            let label = CString::new("everyfiles.fsevents").expect("static label");
            let queue = dispatch_queue_create(label.as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(s, queue);
            if FSEventStreamStart(s) == 0 {
                FSEventStreamInvalidate(s);
                FSEventStreamRelease(s);
                dispatch_release(queue);
                drop(Box::from_raw(shared));
                return Err(Error::io(
                    root,
                    std::io::Error::other("FSEventStreamStart failed"),
                ));
            }
            Stream {
                stream: s,
                queue,
                shared,
            }
        };
        self.stream = Some(stream);
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
        match self.uuid {
            Some(u) => encode_token(&u, self.latest()),
            None => ResumeToken::default(),
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(s) = self.stream.take() {
            let last = unsafe { &*s.shared }.latest.load(Ordering::Relaxed);
            self.latest_at_start = last;
            // SAFETY: Stop then Invalidate guarantee the callback will not run
            // again, after which releasing the stream, the queue, and the
            // leaked state is sound and happens exactly once.
            unsafe {
                FSEventStreamStop(s.stream);
                FSEventStreamInvalidate(s.stream);
                FSEventStreamRelease(s.stream);
                dispatch_release(s.queue);
                drop(Box::from_raw(s.shared));
            }
        }
        self.rx = None;
        Ok(())
    }
}

impl Drop for FsEventsWatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let u = [7u8; 16];
        let t = encode_token(&u, 0x0102_0304_0506_0708);
        assert_eq!(t.0.len(), 24);
        assert_eq!(decode_token(&t), Some((u, 0x0102_0304_0506_0708)));
        assert_eq!(decode_token(&ResumeToken(vec![1, 2, 3])), None);
    }
}
