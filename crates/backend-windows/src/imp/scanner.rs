//! The unprivileged Windows scanner: one `FileIdBothDirectoryInfo` loop per
//! directory, which returns name, size, mtime, attributes, and file id in
//! bulk with no per-file call and no elevation. Works on NTFS, ReFS, and FAT.
//!
//! The one addition is a `symlink_metadata` per *subdirectory*: the listing
//! is the same lazily refreshed directory-index copy that `FindFirstFile`
//! reads, and NTFS does not update a subdirectory's last-write time there
//! until that directory is next opened. The engine's spot-check and sweep
//! compare recorded directory mtimes against a handle-based stat, so the
//! scanner records that value rather than the stale copy (see
//! `GenericScanner::read_dir` for the SDK reference).
//!
//! The NTFS USN journal (elevated) is a separate, later tier: it is faster
//! for cold enumeration and is the watcher's resumable position, but it
//! carries neither size nor mtime, so this walk is what supplies them.

use std::path::Path;

use everyfiles_backend_generic::GenericScanner;
use everyfiles_core::platform::NameEncoding;
use everyfiles_core::scan::{
    EntryAttrs, EntryStat, Kind, ProbeReport, RawEntry, RootInfo, ScanCaps, ScanMode, Scanner,
    VolumeId, VolumeIdKind, name_bytes, system_time_to_ns,
};
use everyfiles_core::{Error, Result};

use crate::imp::sys;

/// 64 KiB: comfortably above the largest possible single entry (~620 bytes).
const BUF_SIZE: usize = 64 * 1024;

/// Bulk directory walk via `GetFileInformationByHandleEx`. Unprivileged.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsScanner {
    fallback: GenericScanner,
}

impl WindowsScanner {
    pub fn new() -> Self {
        Self::default()
    }

    fn read_dir_bulk(&self, dir: &Path, out: &mut Vec<RawEntry>) -> std::io::Result<()> {
        let h = sys::open_dir(dir)?;
        let mut buf = vec![0u8; BUF_SIZE];
        let mut restart = true;
        let start_len = out.len();
        loop {
            let n = match sys::fetch(&h, restart, &mut buf)? {
                sys::Fetch::Done => return Ok(()),
                sys::Fetch::Some(n) => n,
            };
            restart = false;
            let parsed = sys::parse_entries(&buf[..n], |e| {
                let kind = if e.is_reparse() {
                    // Junctions and symlinks: recorded, never descended.
                    Kind::Symlink
                } else if e.is_dir() {
                    Kind::Dir
                } else {
                    Kind::File
                };
                let mut attrs = 0u8;
                if e.is_hidden() {
                    attrs |= EntryAttrs::HIDDEN;
                }
                if e.is_system() {
                    attrs |= EntryAttrs::SYSTEM;
                }
                if e.is_reparse() {
                    attrs |= EntryAttrs::REPARSE;
                }
                // Authoritative mtime for directories (module docs). A stat
                // that fails because the directory vanished mid-listing keeps
                // the listed value; the engine's reconcile handles the rest.
                let mtime = if kind == Kind::Dir {
                    std::fs::symlink_metadata(dir.join(&e.name))
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map_or(e.mtime_ns, system_time_to_ns)
                } else {
                    e.mtime_ns
                };
                out.push(RawEntry {
                    name: name_bytes(&e.name).into(),
                    kind,
                    size: Some(e.size),
                    mtime: Some(mtime),
                    native_id: Some(e.file_id),
                    native_parent: None,
                    attrs: EntryAttrs(attrs),
                });
            });
            if parsed.is_err() {
                out.truncate(start_len);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "malformed FILE_ID_BOTH_DIR_INFO chain",
                ));
            }
        }
    }

    fn declined(e: &std::io::Error) -> bool {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
        matches!(
            e.raw_os_error(),
            Some(c) if c == ERROR_INVALID_PARAMETER as i32 || c == ERROR_NOT_SUPPORTED as i32
        ) || e.kind() == std::io::ErrorKind::InvalidData
    }
}

impl Scanner for WindowsScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: true,
            provides_mtime: true,
            provides_native_ids: true,
            parallel_safe: true,
            requires_privilege: false,
            encoding: NameEncoding::Wtf8,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        let mut tmp = Vec::new();
        match self.read_dir_bulk(root, &mut tmp) {
            Ok(()) => Ok(ProbeReport {
                granted: true,
                active: "file-id-both-dir-info",
                reason: None,
                kernel_queue_bytes: None,
            }),
            Err(e) if Self::declined(&e) => Ok(ProbeReport {
                granted: false,
                active: "std-read-dir",
                reason: Some("filesystem declined FileIdBothDirectoryInfo"),
                kernel_queue_bytes: None,
            }),
            Err(e) => Err(Error::io(root, e)),
        }
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        self.fallback.root_info(root)
    }

    fn volume_id(&self, root: &Path) -> Result<VolumeId> {
        let (serial, fs) = sys::volume_identity(root).map_err(|e| Error::io(root, e))?;
        let mut bytes = Vec::with_capacity(4 + fs.len());
        bytes.extend_from_slice(&serial.to_le_bytes());
        bytes.extend_from_slice(name_bytes(&fs));
        Ok(VolumeId {
            kind: VolumeIdKind::WinSerial,
            bytes,
        })
    }

    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>> {
        self.fallback.stat_entry(path)
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        match self.read_dir_bulk(dir, out) {
            Ok(()) => Ok(()),
            Err(e) if Self::declined(&e) => self.fallback.read_dir(dir, out),
            Err(e) => Err(Error::io(dir, e)),
        }
    }
}
