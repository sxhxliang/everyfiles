//! The change-notification seam.
//!
//! Every platform reports changes as paths (FSEvents, inotify,
//! `ReadDirectoryChangesW`), and every platform has a way of saying "I lost
//! events". The engine treats each path event as *reconcile this path against
//! the filesystem*, which is robust to coalescing and to the ordering games
//! renames play, and treats a loss signal as *reconcile this subtree*. The
//! event kinds are hints, not instructions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;
use crate::scan::ProbeReport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Removed,
    Modified,
    Renamed,
    /// Metadata-only change.
    Attrib,
    /// The backend does not know; reconcile and see.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something happened at `path`. `deep` means descendants may have
    /// changed too (FSEvents `MustScanSubDirs`, a moved-in directory).
    Path {
        path: PathBuf,
        kind: EventKind,
        deep: bool,
    },
    /// The backend dropped events for the whole root; sweep everything.
    Overflow,
    /// The backend's state is unusable (journal wrapped, identity changed).
    RescanNeeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchCaps {
    pub recursive: bool,
    /// Whether a persisted token can replay events across a restart.
    pub resumable: bool,
    pub needs_privilege: bool,
}

/// Opaque position persisted with the index; the backend owns the encoding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeToken(pub Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// `out` received this many events.
    Events(usize),
    Timeout,
    /// The watcher has stopped and will deliver nothing more.
    Closed,
}

/// A platform's change feed.
pub trait Watcher: Send {
    fn capabilities(&self) -> WatchCaps;

    fn probe(&self, root: &Path) -> Result<ProbeReport>;

    /// Begin watching `root`. `since` is a token from a previous run; a
    /// watcher that cannot honour it must return `Error::TokenStale` so the
    /// caller reconciles instead of trusting a partial replay.
    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()>;

    /// Wait up to `timeout` for events, appending them to `out`.
    fn poll(&mut self, timeout: Duration, out: &mut Vec<WatchEvent>) -> Result<PollOutcome>;

    /// Current position, safe to persist at any time.
    fn resume_token(&self) -> ResumeToken;

    fn stop(&mut self) -> Result<()>;
}
