//! Open an index: load it if it is fresh, otherwise rescan and save.
//!
//! Staleness rules, any of which forces a rescan:
//! 1. The file is missing, corrupt, or written by an incompatible build.
//! 2. The stored volume identity differs from the live one (when both known).
//! 3. The root is no longer a readable directory.
//! 4. A spot-check of random live records finds a size/mtime/kind mismatch
//!    or a vanished file. Sixteen `stat`s catch reformat, reinstall, and
//!    restore-from-backup, which is what makes a weak volume identity safe.
//! 5. The caller asked for it.
//!
//! Until incremental updates land, a failed spot-check means a full rescan;
//! that is the honest trade: a stale index must never serve wrong answers.

mod update;
mod watch_loop;

pub use update::{UpdateStats, Updater};
pub use watch_loop::{BatchReport, WatchOptions, watch_loop};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::index::{Id, Index, flags};
use crate::platform::VolumeConfig;
use crate::scan::{Kind, ScanOptions, ScanStats, Scanner, build_index};
use crate::store::{Loaded, SaveMeta, load, save};

/// How to open.
#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub force_rescan: bool,
    /// Hash the whole payload on load (~10 ms per 80 MB).
    pub verify_payload: bool,
    /// Random live records to `stat` against the index; 0 disables.
    pub spot_check: usize,
    pub scan: ScanOptions,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            force_rescan: false,
            verify_payload: true,
            spot_check: 16,
            scan: ScanOptions::default(),
        }
    }
}

/// Why a rescan happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RescanReason {
    NoIndex,
    Forced,
    Corrupt(String),
    Incompatible(String),
    PayloadHashMismatch,
    VolumeChanged,
    RootUnavailable,
    SpotCheckFailed(PathBuf),
}

impl std::fmt::Display for RescanReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoIndex => write!(f, "no index file"),
            Self::Forced => write!(f, "forced"),
            Self::Corrupt(m) => write!(f, "corrupt index: {m}"),
            Self::Incompatible(m) => write!(f, "incompatible index: {m}"),
            Self::PayloadHashMismatch => write!(f, "payload checksum mismatch"),
            Self::VolumeChanged => write!(f, "volume identity changed"),
            Self::RootUnavailable => write!(f, "root is not a readable directory"),
            Self::SpotCheckFailed(p) => write!(f, "spot-check failed at {}", p.display()),
        }
    }
}

/// What `open` did.
#[derive(Debug)]
pub enum OpenOutcome {
    Loaded {
        elapsed: Duration,
        file_len: u64,
        resume_token: Vec<u8>,
    },
    Rescanned {
        reason: RescanReason,
        stats: ScanStats,
        saved_bytes: u64,
        save_elapsed: Duration,
    },
}

/// Open the index for `root` stored at `index_path`.
pub fn open(
    scanner: &dyn Scanner,
    root: &Path,
    index_path: &Path,
    cfg: VolumeConfig,
    opts: &OpenOptions,
) -> Result<(Index, OpenOutcome)> {
    let root = std::path::absolute(root)?;
    let reason = if opts.force_rescan {
        RescanReason::Forced
    } else if !index_path.exists() {
        RescanReason::NoIndex
    } else {
        let t = Instant::now();
        match try_load(scanner, &root, index_path, cfg, opts) {
            Ok(l) => {
                return Ok((
                    l.index,
                    OpenOutcome::Loaded {
                        elapsed: t.elapsed(),
                        file_len: l.file_len,
                        resume_token: l.resume_token,
                    },
                ));
            }
            Err(r) => r,
        }
    };

    let (index, stats) = build_index(scanner, &root, cfg, &opts.scan)?;
    let vid = scanner.volume_id(&root)?;
    if let Some(parent) = index_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let st = save(
        &index,
        index_path,
        &SaveMeta {
            volume_id: &vid,
            resume_token: &[],
        },
    )?;
    Ok((
        index,
        OpenOutcome::Rescanned {
            reason,
            stats,
            saved_bytes: st.bytes,
            save_elapsed: st.elapsed,
        },
    ))
}

fn try_load(
    scanner: &dyn Scanner,
    root: &Path,
    index_path: &Path,
    cfg: VolumeConfig,
    opts: &OpenOptions,
) -> std::result::Result<Loaded, RescanReason> {
    let loaded = match load(index_path, cfg) {
        Ok(l) => l,
        Err(crate::Error::Corrupt(m)) => return Err(RescanReason::Corrupt(m)),
        Err(crate::Error::Incompatible(m)) => return Err(RescanReason::Incompatible(m)),
        Err(e) => return Err(RescanReason::Corrupt(e.to_string())),
    };
    if loaded.index.root_path() != root {
        return Err(RescanReason::Incompatible(format!(
            "index is for {}",
            loaded.index.root_path().display()
        )));
    }
    if opts.verify_payload && !loaded.verify_payload() {
        return Err(RescanReason::PayloadHashMismatch);
    }
    if scanner.root_info(root).is_err() {
        return Err(RescanReason::RootUnavailable);
    }
    if let Ok(live) = scanner.volume_id(root)
        && !live.is_none()
        && !loaded.volume_id.is_none()
        && live != loaded.volume_id
    {
        return Err(RescanReason::VolumeChanged);
    }
    if let Some(p) = spot_check(scanner, &loaded.index, opts.spot_check) {
        return Err(RescanReason::SpotCheckFailed(p));
    }
    Ok(loaded)
}

/// `Some(path)` of the first disagreement, `None` if all samples agree or
/// the backend cannot stat.
fn spot_check(scanner: &dyn Scanner, index: &Index, samples: usize) -> Option<PathBuf> {
    let n = index.len() as u64;
    if samples == 0 || n <= 1 {
        return None;
    }
    // Deterministic-enough LCG seeded from the clock; we only need spread.
    let mut s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9)
        | 1;
    let mut checked = 0;
    let mut attempts = 0;
    while checked < samples && attempts < samples * 8 {
        attempts += 1;
        s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let id = (1 + (s >> 33) % (n - 1)) as Id;
        if index.is_tombstone(id) {
            continue;
        }
        let path = index.resolve_path(id);
        match scanner.stat_entry(&path) {
            Ok(None) => return None, // backend cannot check; trust the index
            Err(_) => return Some(path),
            Ok(Some(st)) => {
                let f = index.flags(id);
                let is_dir = f & flags::DIR != 0;
                if is_dir != (st.kind == Kind::Dir) {
                    return Some(path);
                }
                if f & flags::HAS_SIZE != 0
                    && st.kind == Kind::File
                    && st.size.is_some_and(|sz| sz != index.size(id))
                {
                    return Some(path);
                }
                if f & flags::HAS_MTIME != 0 && st.mtime.is_some_and(|m| m != index.mtime(id)) {
                    return Some(path);
                }
                checked += 1;
            }
        }
    }
    None
}
