//! USN journal watcher: the privileged, resumable Windows path.
//!
//! NTFS keeps a change journal per volume. Reading it needs a handle on the
//! volume device (`\\.\C:`), which requires Administrator. In exchange the
//! journal gives a position, `NextUsn`, that can be persisted and resumed
//! from across restarts, so changes made while nothing was watching are
//! replayed instead of swept. The token also carries `UsnJournalID`; a
//! different id means the journal was recreated and the position is void.
//! If the stored position has fallen behind `FirstUsn`, the journal wrapped
//! and the token is stale too.
//!
//! A USN record names the entry and its parent by file reference number,
//! not by path, and carries no size or mtime. The parent is resolved with
//! `OpenFileById` + `GetFinalPathNameByHandleW` (cached), and the engine
//! stats each path once on reconcile. `USN_RECORD_V2` and `V3` are parsed by
//! documented byte offsets so no struct ABI is assumed beyond the version
//! and length fields.
//!
//! The USN-based *initial enumeration* (`FSCTL_ENUM_USN_DATA`) is not
//! implemented; scans use the unprivileged walk, which is what supplies size
//! and mtime anyway.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{EventKind, PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};
use everyfiles_core::{Error, Result};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED,
    ERROR_JOURNAL_NOT_ACTIVE, ERROR_OPERATION_ABORTED, GENERIC_READ, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_128, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdType, GetFinalPathNameByHandleW, GetVolumePathNameW, OPEN_EXISTING,
    OpenFileById,
};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V1,
    USN_REASON_BASIC_INFO_CHANGE, USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE,
    USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
};

const BUF_SIZE: usize = 64 * 1024;
const CACHE_MAX: usize = 16 * 1024;
const REASONS: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_BASIC_INFO_CHANGE;

/// Why the journal is unavailable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Opening the volume device needs Administrator.
    Privilege,
    /// The volume has no change journal (not NTFS/ReFS, or disabled).
    NoJournal,
    Other(u32),
}

impl Unavailable {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Privilege => "USN journal needs Administrator",
            Self::NoJournal => "volume has no USN journal",
            Self::Other(_) => "USN journal unavailable",
        }
    }
}

/// `C:\` for a path on `C:`.
fn volume_root(path: &Path) -> std::io::Result<Vec<u16>> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut root = [0u16; 512];
    // SAFETY: `wide` is NUL-terminated; `root` is writable for its length.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let n = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    Ok(root[..n].to_vec())
}

/// Open `\\.\C:` for reading the journal.
fn open_volume(path: &Path) -> std::result::Result<OwnedHandle, Unavailable> {
    let root =
        volume_root(path).map_err(|e| Unavailable::Other(e.raw_os_error().unwrap_or(0) as u32))?;
    // "C:\" -> "\\.\C:"
    let mut dev: Vec<u16> = r"\\.\".encode_utf16().collect();
    let trimmed = root.strip_suffix(&[b'\\' as u16]).unwrap_or(&root);
    dev.extend_from_slice(trimmed);
    dev.push(0);
    // SAFETY: `dev` is NUL-terminated; other arguments are plain values.
    let h: HANDLE = unsafe {
        CreateFileW(
            dev.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: trivially safe FFI call.
        let e = unsafe { GetLastError() };
        return Err(if e == ERROR_ACCESS_DENIED {
            Unavailable::Privilege
        } else {
            Unavailable::Other(e)
        });
    }
    // SAFETY: valid handle we own.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

#[derive(Debug, Clone, Copy)]
struct JournalInfo {
    id: u64,
    first_usn: i64,
    next_usn: i64,
}

fn query_journal(vol: &OwnedHandle) -> std::result::Result<JournalInfo, Unavailable> {
    // SAFETY: zero is a valid bit pattern for this all-integer struct.
    let mut data: USN_JOURNAL_DATA_V1 = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: `vol` is a live volume handle; `data` is writable for its size.
    let ok = unsafe {
        DeviceIoControl(
            vol.as_raw_handle(),
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            std::ptr::from_mut(&mut data).cast(),
            size_of::<USN_JOURNAL_DATA_V1>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: trivially safe FFI call.
        let e = unsafe { GetLastError() };
        return Err(if e == ERROR_JOURNAL_NOT_ACTIVE {
            Unavailable::NoJournal
        } else if e == ERROR_ACCESS_DENIED {
            Unavailable::Privilege
        } else {
            Unavailable::Other(e)
        });
    }
    Ok(JournalInfo {
        id: data.UsnJournalID,
        first_usn: data.FirstUsn,
        next_usn: data.NextUsn,
    })
}

/// A parsed record: parent reference (64- or 128-bit), name, reason.
struct Rec {
    parent: ParentRef,
    name: OsString,
    reason: u32,
    usn: i64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ParentRef {
    V2(u64),
    V3([u8; 16]),
}

/// Parse the records following the leading `next USN` in a read buffer.
fn parse_records(buf: &[u8], mut f: impl FnMut(Rec)) -> std::result::Result<(), ()> {
    let mut pos = 8usize; // skip the next-USN prefix
    while pos + 8 <= buf.len() {
        let u32_at = |i: usize| -> Option<u32> {
            buf.get(i..i + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let u16_at = |i: usize| -> Option<u16> {
            buf.get(i..i + 2)
                .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
        };
        let i64_at = |i: usize| -> Option<i64> {
            buf.get(i..i + 8)
                .map(|b| i64::from_le_bytes(b.try_into().unwrap()))
        };
        let len = u32_at(pos).ok_or(())? as usize;
        if len < 60 || pos + len > buf.len() {
            return Err(());
        }
        let major = u16_at(pos + 4).ok_or(())?;
        let (parent, usn, reason, name_len, name_off) = match major {
            2 => (
                ParentRef::V2(i64_at(pos + 16).ok_or(())? as u64),
                i64_at(pos + 24).ok_or(())?,
                u32_at(pos + 40).ok_or(())?,
                u16_at(pos + 56).ok_or(())? as usize,
                u16_at(pos + 58).ok_or(())? as usize,
            ),
            3 => {
                let mut id = [0u8; 16];
                id.copy_from_slice(buf.get(pos + 24..pos + 40).ok_or(())?);
                (
                    ParentRef::V3(id),
                    i64_at(pos + 40).ok_or(())?,
                    u32_at(pos + 56).ok_or(())?,
                    u16_at(pos + 72).ok_or(())? as usize,
                    u16_at(pos + 74).ok_or(())? as usize,
                )
            }
            // V4 (range tracking) carries no name; skip it by length.
            _ => {
                pos += (len + 7) & !7;
                continue;
            }
        };
        let ns = pos + name_off;
        let ne = ns.checked_add(name_len).ok_or(())?;
        if name_len % 2 != 0 || ne > pos + len {
            return Err(());
        }
        let units: Vec<u16> = buf[ns..ne]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        f(Rec {
            parent,
            name: OsString::from_wide(&units),
            reason,
            usn,
        });
        pos += (len + 7) & !7;
    }
    Ok(())
}

/// Path of a directory given its file reference number, via `OpenFileById`.
fn path_of(vol: &OwnedHandle, parent: &ParentRef) -> Option<PathBuf> {
    let desc = match parent {
        ParentRef::V2(frn) => FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: *frn as i64,
            },
        },
        ParentRef::V3(id) => FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: ExtendedFileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                ExtendedFileId: FILE_ID_128 { Identifier: *id },
            },
        },
    };
    // SAFETY: `vol` is a live handle on the same volume; `desc` is fully
    // initialized for the chosen `Type`.
    let h = unsafe {
        OpenFileById(
            vol.as_raw_handle(),
            &desc,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: valid handle we own; closed on drop.
    let h = unsafe { OwnedHandle::from_raw_handle(h) };
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe {
        GetFinalPathNameByHandleW(h.as_raw_handle(), buf.as_mut_ptr(), buf.len() as u32, 0)
    } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let s = OsString::from_wide(&buf[..n]);
    let s = s.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    Some(PathBuf::from(s))
}

fn worker(
    root: PathBuf,
    vol: OwnedHandle,
    journal_id: u64,
    start_usn: i64,
    latest: Arc<AtomicI64>,
    tx: Sender<WatchEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; BUF_SIZE];
    let mut cache: HashMap<ParentRef, PathBuf> = HashMap::new();
    let mut next = start_usn;
    while !stop.load(Ordering::Relaxed) {
        let input = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: next,
            ReasonMask: REASONS,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 1, // block until at least one byte is available
            UsnJournalID: journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut returned = 0u32;
        // SAFETY: `vol` is live; `input` is initialized; `buf` is writable
        // for BUF_SIZE bytes, the length passed.
        let ok = unsafe {
            DeviceIoControl(
                vol.as_raw_handle(),
                FSCTL_READ_USN_JOURNAL,
                std::ptr::from_ref(&input).cast(),
                size_of::<READ_USN_JOURNAL_DATA_V1>() as u32,
                buf.as_mut_ptr().cast(),
                BUF_SIZE as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: trivially safe FFI call.
            let e = unsafe { GetLastError() };
            if e == ERROR_OPERATION_ABORTED || stop.load(Ordering::Relaxed) {
                break;
            }
            if e == ERROR_JOURNAL_ENTRY_DELETED {
                // Our position fell off the journal: wrapped.
                let _ = tx.send(WatchEvent::RescanNeeded);
                if let Ok(j) = query_journal(&vol) {
                    next = j.next_usn;
                    continue;
                }
            }
            if e == ERROR_JOURNAL_DELETE_IN_PROGRESS || e == ERROR_JOURNAL_NOT_ACTIVE {
                let _ = tx.send(WatchEvent::RescanNeeded);
            }
            break;
        }
        let n = returned as usize;
        if n < 8 {
            continue;
        }
        next = i64::from_le_bytes(buf[..8].try_into().unwrap());
        let parsed = parse_records(&buf[..n], |rec| {
            latest.fetch_max(rec.usn, Ordering::Relaxed);
            let dir = match cache.get(&rec.parent) {
                Some(p) => p.clone(),
                None => match path_of(&vol, &rec.parent) {
                    Some(p) => {
                        if cache.len() >= CACHE_MAX {
                            cache.clear();
                        }
                        cache.insert(rec.parent.clone(), p.clone());
                        p
                    }
                    None => return,
                },
            };
            let path = dir.join(&rec.name);
            if !path.starts_with(&root) {
                return;
            }
            let r = rec.reason;
            let kind = if r & USN_REASON_FILE_CREATE != 0 {
                EventKind::Created
            } else if r & USN_REASON_FILE_DELETE != 0 {
                EventKind::Removed
            } else if r & (USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME) != 0 {
                EventKind::Renamed
            } else if r
                & (USN_REASON_DATA_EXTEND | USN_REASON_DATA_OVERWRITE | USN_REASON_DATA_TRUNCATION)
                != 0
            {
                EventKind::Modified
            } else if r & USN_REASON_BASIC_INFO_CHANGE != 0 {
                EventKind::Attrib
            } else {
                EventKind::Unknown
            };
            let _ = tx.send(WatchEvent::Path {
                path,
                kind,
                deep: false,
            });
        });
        latest.store(next, Ordering::Relaxed);
        if parsed.is_err() {
            let _ = tx.send(WatchEvent::Overflow);
        }
    }
}

/// Resumable change feed from the NTFS USN journal. Needs Administrator.
pub struct UsnWatcher {
    worker: Option<JoinHandle<()>>,
    rx: Option<Receiver<WatchEvent>>,
    stop: Arc<AtomicBool>,
    latest: Arc<AtomicI64>,
    journal_id: u64,
    vol_raw: Option<isize>,
}

impl std::fmt::Debug for UsnWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsnWatcher")
            .field("running", &self.worker.is_some())
            .field("journal_id", &self.journal_id)
            .finish()
    }
}

impl Default for UsnWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl UsnWatcher {
    pub fn new() -> Self {
        Self {
            worker: None,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            latest: Arc::new(AtomicI64::new(0)),
            journal_id: 0,
            vol_raw: None,
        }
    }

    /// Can this process read the journal of the volume holding `root`?
    pub fn availability(root: &Path) -> std::result::Result<(), Unavailable> {
        let vol = open_volume(root)?;
        query_journal(&vol).map(|_| ())
    }
}

fn encode_token(journal_id: u64, usn: i64) -> ResumeToken {
    let mut v = Vec::with_capacity(16);
    v.extend_from_slice(&journal_id.to_le_bytes());
    v.extend_from_slice(&usn.to_le_bytes());
    ResumeToken(v)
}

fn decode_token(t: &ResumeToken) -> Option<(u64, i64)> {
    if t.0.len() != 16 {
        return None;
    }
    Some((
        u64::from_le_bytes(t.0[..8].try_into().ok()?),
        i64::from_le_bytes(t.0[8..].try_into().ok()?),
    ))
}

impl Watcher for UsnWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: true,
            needs_privilege: true,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        Ok(match Self::availability(root) {
            Ok(()) => ProbeReport {
                granted: true,
                active: "usn-journal",
                reason: None,
                kernel_queue_bytes: None,
            },
            Err(u) => ProbeReport {
                granted: false,
                active: "read-directory-changes",
                reason: Some(u.reason()),
                kernel_queue_bytes: None,
            },
        })
    }

    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()> {
        if self.worker.is_some() {
            self.stop()?;
        }
        let root = std::path::absolute(root)?;
        let vol = open_volume(&root).map_err(|u| Error::NotSupported(u.reason()))?;
        let j = query_journal(&vol).map_err(|u| Error::NotSupported(u.reason()))?;
        let start_usn = match since {
            None => j.next_usn,
            Some(t) => {
                let Some((id, usn)) = decode_token(t) else {
                    return Err(Error::TokenStale);
                };
                if id != j.id || usn < j.first_usn || usn > j.next_usn {
                    return Err(Error::TokenStale);
                }
                usn
            }
        };
        self.journal_id = j.id;
        self.latest = Arc::new(AtomicI64::new(start_usn));
        self.vol_raw = Some(vol.as_raw_handle() as isize);
        let (tx, rx) = bounded(16 * 1024);
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop);
        let latest = Arc::clone(&self.latest);
        let jid = j.id;
        self.worker = Some(std::thread::spawn(move || {
            worker(root, vol, jid, start_usn, latest, tx, stop)
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
        if self.journal_id == 0 {
            return ResumeToken::default();
        }
        encode_token(self.journal_id, self.latest.load(Ordering::Relaxed))
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.vol_raw.take() {
            // SAFETY: cancelling I/O on a handle the worker still owns is the
            // documented way to unblock its synchronous DeviceIoControl.
            unsafe { CancelIoEx(h as RawHandle, std::ptr::null()) };
        }
        if let Some(j) = self.worker.take() {
            let _ = j.join();
        }
        self.rx = None;
        Ok(())
    }
}

impl Drop for UsnWatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let t = encode_token(0xABCD, -5);
        assert_eq!(decode_token(&t), Some((0xABCD, -5)));
        assert_eq!(decode_token(&ResumeToken(vec![1])), None);
    }

    /// Build a V2 record by documented offsets and parse it back.
    #[test]
    fn parses_v2_records() {
        let name: Vec<u16> = "Report.docx".encode_utf16().collect();
        let name_bytes = name.len() * 2;
        let len = (60 + name_bytes + 7) & !7;
        let mut rec = vec![0u8; len];
        rec[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        rec[4..6].copy_from_slice(&2u16.to_le_bytes());
        rec[16..24].copy_from_slice(&0x0001_0000_0000_002Au64.to_le_bytes()); // parent
        rec[24..32].copy_from_slice(&1234i64.to_le_bytes()); // usn
        rec[40..44].copy_from_slice(&USN_REASON_FILE_CREATE.to_le_bytes());
        rec[56..58].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        rec[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            rec[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        let mut buf = 9999i64.to_le_bytes().to_vec();
        buf.extend(rec.iter());
        buf.extend(rec.iter());
        let mut got = Vec::new();
        parse_records(&buf, |r| got.push((r.name, r.reason, r.usn, r.parent))).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "Report.docx");
        assert_eq!(got[0].1, USN_REASON_FILE_CREATE);
        assert_eq!(got[0].2, 1234);
        assert_eq!(got[0].3, ParentRef::V2(0x0001_0000_0000_002A));
        assert!(
            parse_records(&buf[..20], |_| {}).is_err(),
            "truncated record"
        );
    }
}
