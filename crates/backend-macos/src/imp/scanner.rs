//! The macOS scanner: one `getattrlistbulk` loop per directory, falling back
//! to the portable backend wherever the kernel declines.
//!
//! Firmlinks: entries such as `/usr/local` are stubs on the sealed system
//! volume that redirect to the data volume. Listing the parent reports the
//! stub's own attributes (its mtime equals the system snapshot's), while
//! opening the path resolves through the firmlink, so the *contents* are
//! enumerated correctly. Only the directory's own mtime is off, and the
//! engine's sweep overwrites it with the `lstat` value on its first pass.

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use everyfiles_backend_generic::GenericScanner;
use everyfiles_core::platform::NameEncoding;
use everyfiles_core::scan::{
    EntryAttrs, EntryStat, Kind, ProbeReport, RawEntry, RootInfo, ScanCaps, ScanMode, Scanner,
    VolumeId, VolumeIdKind, name_bytes, os_str_from_bytes, system_time_to_ns,
};
use everyfiles_core::{Error, Result};

use crate::imp::sys;

/// 1 MiB per call: ~12k entries at ~80 bytes each.
const BUF_SIZE: usize = 1 << 20;

thread_local! {
    static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Bulk enumeration via `getattrlistbulk(2)`. Unprivileged.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacScanner {
    fallback: GenericScanner,
}

impl MacScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Why a directory could not be read in bulk; the caller falls back.
    fn declined(e: &std::io::Error) -> bool {
        matches!(
            e.raw_os_error(),
            Some(libc::ENOTSUP | libc::EINVAL | libc::ERANGE)
        )
    }

    fn read_dir_bulk(
        &self,
        dir: &Path,
        out: &mut Vec<RawEntry>,
    ) -> std::result::Result<(), BulkFailure> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(dir)
            .map_err(BulkFailure::Io)?;
        let fd = file.as_raw_fd();
        let mut req = sys::bulk_request();
        let start_len = out.len();
        BUF.with_borrow_mut(|buf| {
            if buf.len() < BUF_SIZE {
                buf.resize(BUF_SIZE, 0);
            }
            loop {
                let n = match sys::bulk(fd, &mut req, buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => n,
                    Err(e) if Self::declined(&e) => {
                        out.truncate(start_len);
                        return Err(BulkFailure::Declined);
                    }
                    Err(e) => return Err(BulkFailure::Io(e)),
                };
                let mut deferred: Vec<Vec<u8>> = Vec::new();
                let parsed = sys::parse_entries(buf, n, |e| {
                    // The kernel could not stat this entry, or did not say what
                    // it is: stat it ourselves below rather than guess.
                    if e.error != 0 || e.kind.is_none() {
                        deferred.push(e.name.to_vec());
                        return;
                    }
                    out.push(RawEntry {
                        name: e.name.into(),
                        kind: e.kind.unwrap_or(Kind::Other),
                        size: e.size,
                        mtime: e.mtime,
                        native_id: e.fileid,
                        native_parent: None,
                        attrs: EntryAttrs::default(),
                    });
                });
                if parsed.is_err() {
                    out.truncate(start_len);
                    return Err(BulkFailure::Declined);
                }
                for name in deferred {
                    // SAFETY: `name` came from the kernel as the entry's bytes,
                    // which is exactly what `OsStr::as_encoded_bytes` yields on
                    // macOS (UTF-8 / arbitrary bytes).
                    let os = unsafe { os_str_from_bytes(&name) };
                    if let Ok(md) = std::fs::symlink_metadata(dir.join(os)) {
                        out.push(RawEntry {
                            name: name.into(),
                            kind: kind_of(&md.file_type()),
                            size: Some(md.len()),
                            mtime: md.modified().ok().map(system_time_to_ns),
                            native_id: None,
                            native_parent: None,
                            attrs: EntryAttrs::default(),
                        });
                    }
                }
            }
        })
    }
}

enum BulkFailure {
    /// Filesystem does not support the call (or the buffer was unusable);
    /// use the portable path for this directory.
    Declined,
    Io(std::io::Error),
}

fn kind_of(ft: &std::fs::FileType) -> Kind {
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

impl Scanner for MacScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: true,
            provides_mtime: true,
            provides_native_ids: true,
            parallel_safe: true,
            requires_privilege: false,
            encoding: NameEncoding::Bytes,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        let mut tmp = Vec::new();
        match self.read_dir_bulk(root, &mut tmp) {
            Ok(()) => Ok(ProbeReport {
                granted: true,
                active: "getattrlistbulk",
                reason: None,
                kernel_queue_bytes: None,
            }),
            Err(BulkFailure::Declined) => Ok(ProbeReport {
                granted: false,
                active: "std-read-dir",
                reason: Some("filesystem declined getattrlistbulk"),
                kernel_queue_bytes: None,
            }),
            Err(BulkFailure::Io(e)) => Err(Error::io(root, e)),
        }
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        self.fallback.root_info(root)
    }

    fn volume_id(&self, root: &Path) -> Result<VolumeId> {
        match sys::volume_uuid(root) {
            Ok(uuid) => Ok(VolumeId {
                kind: VolumeIdKind::MacUuid,
                bytes: uuid.to_vec(),
            }),
            // No UUID (some network/synthetic volumes): fall back to the
            // generic heuristic rather than report nothing.
            Err(_) => self.fallback.volume_id(root),
        }
    }

    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>> {
        self.fallback.stat_entry(path)
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        match self.read_dir_bulk(dir, out) {
            Ok(()) => Ok(()),
            Err(BulkFailure::Declined) => self.fallback.read_dir(dir, out),
            Err(BulkFailure::Io(e)) => Err(Error::io(dir, e)),
        }
    }
}

/// Keep `name_bytes` in the public surface of this module for symmetry with
/// the other backends; the bulk path never needs it because the kernel
/// already hands us raw bytes.
#[allow(dead_code)]
fn _name_bytes_is_the_sanctioned_way(s: &std::ffi::OsStr) -> &[u8] {
    name_bytes(s)
}
