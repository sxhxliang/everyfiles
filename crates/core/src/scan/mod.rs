//! The backend seam.
//!
//! A backend reports *observations*; the core owns identity, the tree, and the
//! pools. That split is what lets every platform contribute exactly the
//! metadata it gets for free without ever doing a per-file `stat` during a
//! scan: `getdents64` yields names and types only, `getattrlistbulk` yields
//! everything, the USN journal yields a volume-wide stream whose parents may
//! not have been seen yet. `Option` fields on [`RawEntry`] let a backend say
//! "I don't know" honestly.

mod assemble;

pub use assemble::{ScanOptions, ScanStats, build_index};

use std::ffi::OsStr;
use std::path::Path;
use std::time::SystemTime;

use smallvec::SmallVec;

use crate::error::{Error, Result};
use crate::platform::NameEncoding;

/// Backend-native identifier: inode number, NTFS file reference number, etc.
pub type NativeId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// Attribute bits a backend can supply for free.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryAttrs(pub u8);

impl EntryAttrs {
    pub const HIDDEN: u8 = 1 << 0;
    pub const SYSTEM: u8 = 1 << 1;
    pub const REPARSE: u8 = 1 << 2;

    #[inline]
    pub fn hidden(self) -> bool {
        self.0 & Self::HIDDEN != 0
    }
    #[inline]
    pub fn system(self) -> bool {
        self.0 & Self::SYSTEM != 0
    }
    #[inline]
    pub fn reparse(self) -> bool {
        self.0 & Self::REPARSE != 0
    }
}

/// One directory entry as observed by a backend, before the core assigns identity.
///
/// `name` is a single path component in the platform's `OsStr` encoding —
/// exactly the bytes of `OsStr::as_encoded_bytes()`, with no separator and no
/// NUL. Backends MUST produce it via [`name_bytes`]; the core relies on that
/// contract when it turns stored bytes back into an `OsStr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEntry {
    pub name: SmallVec<[u8; 64]>,
    pub kind: Kind,
    /// `Some` only when the backend reports it in the same bulk call.
    pub size: Option<u64>,
    /// Nanoseconds since the Unix epoch; `Some` only when reported in bulk.
    pub mtime: Option<i64>,
    pub native_id: Option<NativeId>,
    /// Native id of the containing directory, for volume-wide backends that
    /// report it independently of traversal. `None` for per-directory scans.
    pub native_parent: Option<NativeId>,
    pub attrs: EntryAttrs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    /// Enumerate one directory per call; the core drives the traversal.
    PerDirectory,
    /// One stream covering the whole volume.
    VolumeWide,
}

/// What a backend can do in general.
#[derive(Clone, Copy, Debug)]
pub struct ScanCaps {
    pub mode: ScanMode,
    pub provides_size: bool,
    pub provides_mtime: bool,
    pub provides_native_ids: bool,
    pub parallel_safe: bool,
    pub requires_privilege: bool,
    pub encoding: NameEncoding,
}

/// What a backend can do *for this root, right now* — the privilege probe.
#[derive(Clone, Copy, Debug)]
pub struct ProbeReport {
    pub granted: bool,
    /// e.g. `"getattrlistbulk"`, `"usn-journal"`, `"findex-walk"`, `"inotify"`.
    pub active: &'static str,
    /// Why the fast path is unavailable, if it is.
    pub reason: Option<&'static str>,
    pub kernel_queue_bytes: Option<u64>,
}

/// How a volume identity was derived. Different kinds never compare equal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum VolumeIdKind {
    /// No identity available; staleness relies on the spot-check alone.
    #[default]
    None = 0,
    /// macOS volume UUID (`ATTR_VOL_UUID`).
    MacUuid = 1,
    /// Linux heuristic: filesystem type, device, root inode.
    LinuxHeuristic = 2,
    /// Windows volume serial number.
    WinSerial = 3,
    /// Generic backend heuristic from `std` metadata.
    Generic = 4,
}

impl VolumeIdKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::MacUuid,
            2 => Self::LinuxHeuristic,
            3 => Self::WinSerial,
            4 => Self::Generic,
            _ => Self::None,
        }
    }
}

/// Identity of the volume an index was built from; persisted with the index
/// so a reformatted or swapped volume forces a rescan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeId {
    pub kind: VolumeIdKind,
    pub bytes: Vec<u8>,
}

impl VolumeId {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_none(&self) -> bool {
        self.kind == VolumeIdKind::None
    }
}

/// Metadata for one path, used for staleness spot-checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryStat {
    pub kind: Kind,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
}

/// What a backend reports about the root directory itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootInfo {
    /// Root mtime in ns since the epoch, if cheaply available.
    pub mtime: Option<i64>,
}

/// Push-based sink for volume-wide enumeration.
pub trait EntrySink {
    fn push(&mut self, dir_native_id: Option<NativeId>, entry: RawEntry) -> Result<()>;
}

/// A platform's fastest way to list what is on disk.
pub trait Scanner: Send + Sync {
    fn capabilities(&self) -> ScanCaps;

    /// Probe which fast path this process can actually use, without scanning.
    fn probe(&self, root: &Path) -> Result<ProbeReport>;

    /// Confirm `root` is a directory this scanner can enumerate and report its
    /// metadata. This is the only place the engine learns anything about the
    /// root; the core itself never touches the filesystem.
    fn root_info(&self, root: &Path) -> Result<RootInfo>;

    /// Identity of the volume containing `root`. The default reports none.
    fn volume_id(&self, _root: &Path) -> Result<VolumeId> {
        Ok(VolumeId::none())
    }

    /// Metadata for one path without following symlinks, for spot-checks.
    /// `Ok(None)` means this backend cannot check. The default cannot.
    fn stat_entry(&self, _path: &Path) -> Result<Option<EntryStat>> {
        Ok(None)
    }

    /// `ScanMode::PerDirectory`: enumerate exactly one directory into `out`.
    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()>;

    /// `ScanMode::VolumeWide`: enumerate everything reachable from `root`.
    fn enumerate(&self, _root: &Path, _sink: &mut dyn EntrySink) -> Result<()> {
        Err(Error::NotSupported("volume-wide enumeration"))
    }
}

/// The encoded bytes of an `OsStr`. This is the only sanctioned way to build
/// a [`RawEntry::name`].
#[inline]
pub fn name_bytes(s: &OsStr) -> &[u8] {
    s.as_encoded_bytes()
}

/// Reinterpret stored name bytes as an `OsStr`.
///
/// # Safety
///
/// `b` must consist of bytes obtained from [`name_bytes`] on this platform,
/// optionally concatenated with ASCII bytes (path separators). Arbitrary bytes
/// are not valid on Windows, where the encoding is WTF-8.
#[inline]
pub unsafe fn os_str_from_bytes(b: &[u8]) -> &OsStr {
    // SAFETY: forwarded to the caller per the contract above.
    unsafe { OsStr::from_encoded_bytes_unchecked(b) }
}

/// `SystemTime` to nanoseconds since the Unix epoch, saturating.
pub fn system_time_to_ns(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos())
            .map(|n| -n)
            .unwrap_or(i64::MIN),
    }
}
