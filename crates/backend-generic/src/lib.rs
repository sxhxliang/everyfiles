//! The portable reference backend.
//!
//! Deliberately the slow, obviously-correct implementation: `std::fs::read_dir`
//! plus one `symlink_metadata` per entry. It is the universal fallback, the
//! peer that every native backend is differentially tested against, and the
//! only scanner that behaves identically on all three platforms.
//!
//! Its watcher is a polling loop: every interval it asks the engine to sweep
//! directory mtimes, which is also the reference implementation of catch-up.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use everyfiles_core::watch::{PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};

use everyfiles_core::platform::NameEncoding;
use everyfiles_core::scan::{
    EntryAttrs, EntryStat, Kind, ProbeReport, RawEntry, RootInfo, ScanCaps, ScanMode, Scanner,
    VolumeId, name_bytes, system_time_to_ns,
};
use everyfiles_core::{Error, Result};

/// Portable scanner over `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericScanner;

impl GenericScanner {
    pub fn new() -> Self {
        Self
    }
}

impl Scanner for GenericScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: true,
            provides_mtime: true,
            provides_native_ids: false,
            parallel_safe: true,
            requires_privilege: false,
            encoding: if cfg!(windows) {
                NameEncoding::Wtf8
            } else {
                NameEncoding::Bytes
            },
        }
    }

    fn probe(&self, _root: &Path) -> Result<ProbeReport> {
        Ok(ProbeReport {
            granted: true,
            active: "std-read-dir",
            reason: None,
            kernel_queue_bytes: None,
        })
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        let md = std::fs::symlink_metadata(root).map_err(|e| Error::io(root, e))?;
        if !md.is_dir() {
            return Err(Error::NotADirectory(root.to_path_buf()));
        }
        Ok(RootInfo {
            mtime: md.modified().ok().map(system_time_to_ns),
        })
    }

    fn volume_id(&self, root: &Path) -> Result<VolumeId> {
        let md = std::fs::symlink_metadata(root).map_err(|e| Error::io(root, e))?;
        Ok(volume_id_from(&md))
    }

    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>> {
        let md = std::fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
        Ok(Some(EntryStat {
            kind: kind_of(&md.file_type()),
            size: Some(md.len()),
            mtime: md.modified().ok().map(system_time_to_ns),
        }))
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        let iter = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
        for entry in iter {
            // A directory entry that vanished between readdir and stat is a
            // race, not an error: skip it rather than failing the directory.
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            let kind = kind_of(&ft);
            // `DirEntry::metadata` does not follow symlinks, so this reports the
            // link itself for `Kind::Symlink`, which is what the index wants.
            //
            // On Windows it is served from the parent's directory listing,
            // which NTFS refreshes lazily: a subdirectory's last-write time
            // there can lag the directory's own record until that directory
            // is next opened. The `FindFirstFileW` remarks say so ("file
            // attribute information on NTFS file systems may not be current")
            // and name `GetFileInformationByHandle` as the authoritative call,
            // which is what `symlink_metadata` uses. The engine's spot-check
            // and sweep compare against that handle-based stat, so directories
            // are re-stat'ed through it here. Files are left alone: their copy
            // is refreshed on close, and re-stat'ing every file would turn the
            // listing into a per-entry open.
            let md = if cfg!(windows) && kind == Kind::Dir {
                std::fs::symlink_metadata(entry.path())
            } else {
                entry.metadata()
            };
            let (size, mtime) = match md {
                Ok(md) => (Some(md.len()), md.modified().ok().map(system_time_to_ns)),
                Err(_) => (None, None),
            };
            let name = entry.file_name();
            out.push(RawEntry {
                name: name_bytes(&name).into(),
                kind,
                size,
                mtime,
                native_id: None,
                native_parent: None,
                attrs: EntryAttrs::default(),
            });
        }
        Ok(())
    }
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

/// Device number plus root inode on Unix; nothing portable is available on
/// Windows through stable `std`, so the engine's spot-check carries the load.
#[cfg(unix)]
fn volume_id_from(md: &std::fs::Metadata) -> VolumeId {
    use everyfiles_core::scan::VolumeIdKind;
    use std::os::unix::fs::MetadataExt;
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&md.dev().to_le_bytes());
    bytes.extend_from_slice(&md.ino().to_le_bytes());
    VolumeId {
        kind: VolumeIdKind::Generic,
        bytes,
    }
}

#[cfg(not(unix))]
fn volume_id_from(_md: &std::fs::Metadata) -> VolumeId {
    VolumeId::none()
}

/// Periodic sweep. No kernel notification, no resume token; just correct.
#[derive(Debug)]
pub struct PollingWatcher {
    interval: Duration,
    root: Option<PathBuf>,
    next_tick: Instant,
    stopped: bool,
}

impl PollingWatcher {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval: interval.max(Duration::from_millis(10)),
            root: None,
            next_tick: Instant::now(),
            stopped: false,
        }
    }
}

impl Watcher for PollingWatcher {
    fn capabilities(&self) -> WatchCaps {
        WatchCaps {
            recursive: true,
            resumable: false,
            needs_privilege: false,
        }
    }

    fn probe(&self, _root: &Path) -> Result<ProbeReport> {
        Ok(ProbeReport {
            granted: true,
            active: "poll",
            reason: None,
            kernel_queue_bytes: None,
        })
    }

    fn start(&mut self, root: &Path, _since: Option<&ResumeToken>) -> Result<()> {
        self.root = Some(root.to_path_buf());
        self.next_tick = Instant::now() + self.interval;
        self.stopped = false;
        Ok(())
    }

    fn poll(&mut self, timeout: Duration, out: &mut Vec<WatchEvent>) -> Result<PollOutcome> {
        if self.stopped {
            return Ok(PollOutcome::Closed);
        }
        let now = Instant::now();
        if self.next_tick > now {
            let wait = (self.next_tick - now).min(timeout);
            std::thread::sleep(wait);
        }
        if Instant::now() >= self.next_tick {
            self.next_tick = Instant::now() + self.interval;
            out.push(WatchEvent::RescanNeeded);
            Ok(PollOutcome::Events(1))
        } else {
            Ok(PollOutcome::Timeout)
        }
    }

    fn resume_token(&self) -> ResumeToken {
        ResumeToken::default()
    }

    fn stop(&mut self) -> Result<()> {
        self.stopped = true;
        Ok(())
    }
}
