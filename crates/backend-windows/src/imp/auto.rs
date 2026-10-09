//! Automatic tier selection: the USN journal when elevated and available,
//! `ReadDirectoryChangesW` otherwise. Decided at `start`, reported by `probe`.

use std::path::Path;
use std::time::Duration;

use everyfiles_core::Result;
use everyfiles_core::scan::ProbeReport;
use everyfiles_core::watch::{PollOutcome, ResumeToken, WatchCaps, WatchEvent, Watcher};

use super::usn::UsnWatcher;
use super::watcher::RdcwWatcher;

#[derive(Debug)]
enum Tier {
    Usn(UsnWatcher),
    Rdcw(RdcwWatcher),
}

/// The Windows watcher: USN journal (Administrator) with RDCW fallback.
#[derive(Debug, Default)]
pub struct WindowsWatcher {
    tier: Option<Tier>,
}

impl WindowsWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    fn pick(root: &Path) -> Tier {
        if UsnWatcher::availability(root).is_ok() {
            Tier::Usn(UsnWatcher::new())
        } else {
            Tier::Rdcw(RdcwWatcher::new())
        }
    }

    fn inner(&self) -> Option<&dyn Watcher> {
        match &self.tier {
            Some(Tier::Usn(w)) => Some(w),
            Some(Tier::Rdcw(w)) => Some(w),
            None => None,
        }
    }

    fn inner_mut(&mut self) -> Option<&mut dyn Watcher> {
        match &mut self.tier {
            Some(Tier::Usn(w)) => Some(w),
            Some(Tier::Rdcw(w)) => Some(w),
            None => None,
        }
    }
}

impl Watcher for WindowsWatcher {
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
        match Self::pick(root) {
            Tier::Usn(w) => w.probe(root),
            Tier::Rdcw(w) => {
                let mut r = w.probe(root)?;
                if let Err(u) = UsnWatcher::availability(root) {
                    r.reason = Some(u.reason());
                }
                Ok(r)
            }
        }
    }

    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()> {
        let mut tier = Self::pick(root);
        let r = match &mut tier {
            Tier::Usn(w) => w.start(root, since),
            Tier::Rdcw(w) => w.start(root, since),
        };
        let tier = match (r, tier) {
            (Ok(()), t) => t,
            // A stale token must surface so the caller sweeps; any other USN
            // failure degrades to RDCW.
            (Err(everyfiles_core::Error::TokenStale), t) => {
                self.tier = Some(t);
                return Err(everyfiles_core::Error::TokenStale);
            }
            (Err(_), Tier::Usn(_)) => {
                let mut w = RdcwWatcher::new();
                w.start(root, since)?;
                Tier::Rdcw(w)
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
