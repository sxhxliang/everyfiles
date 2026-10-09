//! `getdents64` enumeration through `rustix::fs::Dir`.
//!
//! `getdents64` yields names, inode numbers, and `d_type` — nothing else.
//! Linux has no bulk call that adds size or mtime, so by default this backend
//! indexes names and types only and reports `provides_size = false`. The
//! optional `statx` pass fills them in for users who want size/mtime filters
//! and accept a much slower first scan.
//!
//! Correctness details:
//!
//! - Directories are opened with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`, and
//!   per-entry `statx` goes through the directory fd, so a rename racing the
//!   walk cannot redirect it.
//! - `d_type == DT_UNKNOWN` is real (XFS without `ftype`, many NFS mounts).
//!   Such an entry gets one `statx(STATX_TYPE)`; everything else is never
//!   stat'ed.
//! - `rustix` reads `d_type` straight from the dirent and never falls back
//!   to `stat` itself, which is exactly the behaviour wanted here. There is
//!   no `unsafe` in this file.

use std::ffi::CStr;
use std::path::Path;

use everyfiles_backend_generic::GenericScanner;
use everyfiles_core::platform::NameEncoding;
use everyfiles_core::scan::{
    EntryAttrs, EntryStat, Kind, ProbeReport, RawEntry, RootInfo, ScanCaps, ScanMode, Scanner,
    VolumeId, VolumeIdKind,
};
use everyfiles_core::{Error, Result};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, StatxFlags};

/// Parallel `getdents64` scanner. Unprivileged.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxScanner {
    with_stat: bool,
    fallback: GenericScanner,
}

impl LinuxScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a `statx` pass per entry so size and mtime are indexed.
    pub fn with_stat(mut self, yes: bool) -> Self {
        self.with_stat = yes;
        self
    }

    pub fn stats_entries(&self) -> bool {
        self.with_stat
    }
}

fn kind_of(ft: FileType) -> Kind {
    match ft {
        FileType::Directory => Kind::Dir,
        FileType::RegularFile => Kind::File,
        FileType::Symlink => Kind::Symlink,
        _ => Kind::Other,
    }
}

fn io(path: &Path, e: rustix::io::Errno) -> Error {
    Error::io(path, std::io::Error::from(e))
}

impl Scanner for LinuxScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: self.with_stat,
            provides_mtime: self.with_stat,
            provides_native_ids: true,
            parallel_safe: true,
            requires_privilege: false,
            encoding: NameEncoding::Bytes,
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        let fd = rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io(root, e))?;
        Dir::read_from(&fd).map_err(|e| io(root, e))?;
        Ok(ProbeReport {
            granted: true,
            active: if self.with_stat {
                "getdents64+statx"
            } else {
                "getdents64"
            },
            reason: None,
            kernel_queue_bytes: None,
        })
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        self.fallback.root_info(root)
    }

    /// Filesystem type, device, and root inode. A heuristic: there is no
    /// dependency-free filesystem UUID without libblkid, so the engine's
    /// spot-check carries the real weight here.
    fn volume_id(&self, root: &Path) -> Result<VolumeId> {
        let fs = rustix::fs::statfs(root).map_err(|e| io(root, e))?;
        let st = rustix::fs::stat(root).map_err(|e| io(root, e))?;
        let mut bytes = Vec::with_capacity(24);
        // Field widths differ by architecture; widen explicitly.
        #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
        {
            bytes.extend_from_slice(&(fs.f_type as i64).to_le_bytes());
            bytes.extend_from_slice(&(st.st_dev as u64).to_le_bytes());
            bytes.extend_from_slice(&(st.st_ino as u64).to_le_bytes());
        }
        Ok(VolumeId {
            kind: VolumeIdKind::LinuxHeuristic,
            bytes,
        })
    }

    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>> {
        self.fallback.stat_entry(path)
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        let fd = rustix::fs::open(
            dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io(dir, e))?;
        let d = Dir::read_from(&fd).map_err(|e| io(dir, e))?;
        for entry in d {
            let e = entry.map_err(|e| io(dir, e))?;
            let name: &CStr = e.file_name();
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let mut kind = kind_of(e.file_type());
            let mut size = None;
            let mut mtime = None;
            let need_type = e.file_type() == FileType::Unknown;
            if need_type || self.with_stat {
                let mut mask = StatxFlags::TYPE;
                if self.with_stat {
                    mask |= StatxFlags::SIZE | StatxFlags::MTIME;
                }
                // Through the directory fd, never following symlinks, and
                // without forcing a remote sync on network filesystems.
                match rustix::fs::statx(
                    &fd,
                    name,
                    AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC,
                    mask,
                ) {
                    Ok(sx) => {
                        if need_type {
                            kind = kind_of(FileType::from_raw_mode(sx.stx_mode.into()));
                        }
                        if self.with_stat {
                            size = Some(sx.stx_size);
                            mtime = Some(
                                sx.stx_mtime
                                    .tv_sec
                                    .saturating_mul(1_000_000_000)
                                    .saturating_add(i64::from(sx.stx_mtime.tv_nsec)),
                            );
                        }
                    }
                    // Vanished between getdents and statx: a race, not an error.
                    Err(_) => continue,
                }
            }
            out.push(RawEntry {
                name: bytes.into(),
                kind,
                size,
                mtime,
                native_id: Some(e.ino()),
                native_parent: None,
                attrs: EntryAttrs::default(),
            });
        }
        Ok(())
    }
}
