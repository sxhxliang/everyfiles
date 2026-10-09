//! Automatic tier selection: fanotify when the process may use it, inotify
//! otherwise. The choice is made at `start`, reported by `probe`.

use std::path::Path;
use std::time::Duration;

use everyfiles_core::Result;
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};

use super::fanotify::FanotifyWatcher;
use super::inotify::InotifyWatcher;

#[derive(Debug)]
enum Tier {
    Fanotify(FanotifyWatcher),
    Inotify(InotifyWatcher),
}

/// The Linux watcher: fanotify (root) with inotify fallback.
#[derive(Debug)]
pub struct LinuxWatcher {
    tier: Option<Tier>,
    prefer_inotify: bool,
}

impl Default for LinuxWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxWatcher {
    pub fn new() -> Self {
        Self {
            tier: None,
            prefer_inotify: false,
        }
    }

    /// Never try fanotify, even as root.
    pub fn inotify_only(mut self) -> Self {
        self.prefer_inotify = true;
        self
    }

    fn pick(&self, root: &Path) -> Tier {
        if !self.prefer_inotify && FanotifyWatcher::availability(root).is_ok() {
            Tier::Fanotify(FanotifyWatcher::new())
        } else {
            Tier::Inotify(InotifyWatcher::new())
        }
    }

    fn inner(&self) -> Option<&dyn Watcher> {
        match &self.tier {
            Some(Tier::Fanotify(w)) => Some(w),
            Some(Tier::Inotify(w)) => Some(w),
            None => None,
        }
    }

    fn inner_mut(&mut self) -> Option<&mut dyn Watcher> {
        match &mut self.tier {
            Some(Tier::Fanotify(w)) => Some(w),
            Some(Tier::Inotify(w)) => Some(w),
            None => None,
        }
    }
}

impl Watcher for LinuxWatcher {
    fn capabilities(&self) -> WatchCaps {
        match self.inner() {
            Some(w) => w.capabilities(),
            None => WatchCaps {
                recursive: true,
                resumable: false,
                needs_privilege: false,
            },
        }
    }

    fn probe(&self, root: &Path) -> Result<ProbeReport> {
        match self.pick(root) {
            Tier::Fanotify(w) => w.probe(root),
            Tier::Inotify(w) => {
                let mut r = w.probe(root)?;
                if !self.prefer_inotify {
                    // Say why the faster tier was not chosen.
                    if let Err(u) = FanotifyWatcher::availability(root) {
                        r.reason = Some(u.reason());
                    }
                }
                Ok(r)
            }
        }
    }

    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()> {
        let mut tier = self.pick(root);
        let r = match &mut tier {
            Tier::Fanotify(w) => w.start(root, since),
            Tier::Inotify(w) => w.start(root, since),
        };
        // If fanotify was chosen but failed at start (race, policy), degrade.
        let tier = match (r, tier) {
            (Ok(()), t) => t,
            (Err(_), Tier::Fanotify(_)) => {
                let mut w = InotifyWatcher::new();
                w.start(root, since)?;
                Tier::Inotify(w)
            }
            (Err(e), _) => return Err(e),
        };
        self.tier = Some(tier);
        Ok(())
    }

    fn poll(&mut self, timeout: Duration, out: &mut Vec<WatchEvent>) -> Result<PollOutcome> {
        match self.inner_mut() {
            Some(w) => w.poll(timeout, out),
            None => Ok(PollOutcome::Closed),
        }
    }

    fn resume_token(&self) -> ResumeToken {
        self.inner().map(Watcher::resume_token).unwrap_or_default()
    }

    fn stop(&mut self) -> Result<()> {
        match self.inner_mut() {
            Some(w) => w.stop(),
            None => Ok(()),
        }
    }
}
