//! Public facade for the everyfiles engine.
//!
//! Re-exports the portable core and exposes the backend registry. The default
//! backend is chosen at compile time by `cfg(target_os)`; every backend that
//! was compiled in is also reachable by name at runtime via [`registry`], so a
//! macOS user can still exercise the generic fallback for differential testing.

use std::path::{Path, PathBuf};

pub use everyfiles_core as core;
pub use everyfiles_core::engine::{self, OpenOptions, OpenOutcome, RescanReason};
pub use everyfiles_core::index::{Id, Index, ROOT, flags};
#[cfg(feature = "pinyin")]
pub use everyfiles_core::pinyin;
pub use everyfiles_core::platform::VolumeConfig;
pub use everyfiles_core::query;
pub use everyfiles_core::scan::{self, ScanOptions, ScanStats, Scanner, build_index};
pub use everyfiles_core::store;
pub use everyfiles_core::watch::{self, ResumeToken, WatchEvent, Watcher};
pub use everyfiles_core::{Error, Result};

/// Whether a backend is the platform-native fast path or the portable fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Native,
    Generic,
}

/// Knobs a backend may honour when constructed. Backends ignore what does
/// not apply to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendOptions {
    /// Linux: run a `statx` pass so size and mtime are indexed. Off by
    /// default because `getdents64` alone is what keeps the first scan fast.
    pub with_stat: bool,
    /// Interval for backends that watch by polling.
    pub poll_interval: std::time::Duration,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            with_stat: false,
            poll_interval: std::time::Duration::from_secs(30),
        }
    }
}

/// One registered backend.
pub struct BackendDef {
    pub name: &'static str,
    pub tier: Tier,
    pub make_scanner: fn(&BackendOptions) -> Result<Box<dyn Scanner>>,
    pub make_watcher: fn(&BackendOptions) -> Result<Box<dyn Watcher>>,
}

fn make_generic(_: &BackendOptions) -> Result<Box<dyn Scanner>> {
    Ok(Box::new(everyfiles_backend_generic::GenericScanner::new()))
}

fn make_polling(o: &BackendOptions) -> Result<Box<dyn Watcher>> {
    Ok(Box::new(everyfiles_backend_generic::PollingWatcher::new(
        o.poll_interval,
    )))
}

const GENERIC: BackendDef = BackendDef {
    name: "generic",
    tier: Tier::Generic,
    make_scanner: make_generic,
    make_watcher: make_polling,
};

#[cfg(target_os = "macos")]
fn make_macos(_: &BackendOptions) -> Result<Box<dyn Scanner>> {
    Ok(Box::new(everyfiles_backend_macos::MacScanner::new()))
}

#[cfg(target_os = "linux")]
fn make_linux(o: &BackendOptions) -> Result<Box<dyn Scanner>> {
    Ok(Box::new(
        everyfiles_backend_linux::LinuxScanner::new().with_stat(o.with_stat),
    ))
}

#[cfg(target_os = "windows")]
fn make_windows(_: &BackendOptions) -> Result<Box<dyn Scanner>> {
    Ok(Box::new(everyfiles_backend_windows::WindowsScanner::new()))
}

#[cfg(target_os = "macos")]
fn make_fsevents(_: &BackendOptions) -> Result<Box<dyn Watcher>> {
    Ok(Box::new(everyfiles_backend_macos::FsEventsWatcher::new()))
}

#[cfg(target_os = "macos")]
static REGISTRY: &[BackendDef] = &[
    BackendDef {
        name: everyfiles_backend_macos::BACKEND_NAME,
        tier: Tier::Native,
        make_scanner: make_macos,
        make_watcher: make_fsevents,
    },
    GENERIC,
];

#[cfg(target_os = "linux")]
fn make_linux_watcher(_: &BackendOptions) -> Result<Box<dyn Watcher>> {
    // fanotify when root, inotify otherwise; decided at start, shown by probe.
    Ok(Box::new(everyfiles_backend_linux::LinuxWatcher::new()))
}

#[cfg(target_os = "linux")]
static REGISTRY: &[BackendDef] = &[
    BackendDef {
        name: everyfiles_backend_linux::BACKEND_NAME,
        tier: Tier::Native,
        make_scanner: make_linux,
        make_watcher: make_linux_watcher,
    },
    GENERIC,
];

#[cfg(target_os = "windows")]
fn make_windows_watcher(_: &BackendOptions) -> Result<Box<dyn Watcher>> {
    // USN journal when elevated and the volume has one, ReadDirectoryChangesW
    // otherwise; decided at start, shown by probe.
    Ok(Box::new(everyfiles_backend_windows::WindowsWatcher::new()))
}

#[cfg(target_os = "windows")]
static REGISTRY: &[BackendDef] = &[
    BackendDef {
        name: everyfiles_backend_windows::BACKEND_NAME,
        tier: Tier::Native,
        make_scanner: make_windows,
        make_watcher: make_windows_watcher,
    },
    GENERIC,
];

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
static REGISTRY: &[BackendDef] = &[GENERIC];

/// Every backend compiled into this build. The generic backend is always last.
pub fn registry() -> &'static [BackendDef] {
    REGISTRY
}

/// The compile-time default: the native backend for this host if one exists,
/// otherwise the generic fallback.
pub fn default_backend() -> &'static BackendDef {
    REGISTRY
        .iter()
        .find(|b| b.tier == Tier::Native)
        .unwrap_or_else(|| REGISTRY.last().expect("registry is never empty"))
}

/// Look a backend up by name. `"auto"` and `"native"` resolve to [`default_backend`].
pub fn find_backend(name: &str) -> Option<&'static BackendDef> {
    match name {
        "auto" | "native" => Some(default_backend()),
        other => REGISTRY.iter().find(|b| b.name == other),
    }
}

/// Volume configuration for the host platform. This is the one place the
/// facade injects a platform fact into the platform-free core.
pub fn host_volume_config() -> VolumeConfig {
    if cfg!(windows) {
        VolumeConfig::windows()
    } else {
        VolumeConfig::unix()
    }
}

/// Where the index for `root` lives by default: the OS cache directory,
/// keyed by a fingerprint of the absolute root path. Caches are purgeable by
/// design; losing one costs a rescan.
pub fn default_index_path(root: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(root).ok()?;
    let h = store::fingerprint(abs.as_os_str().as_encoded_bytes());
    Some(
        dirs::cache_dir()?
            .join("everyfiles")
            .join(format!("{h:016x}.idx")),
    )
}
