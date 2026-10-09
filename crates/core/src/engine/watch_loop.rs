//! Drive a watcher: drain events, reconcile, publish a snapshot, compact and
//! persist on a cadence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::error::Result;
use crate::index::Index;
use crate::scan::VolumeId;
use crate::store::{SaveMeta, save};
use crate::watch::{PollOutcome, WatchEvent, Watcher};

use super::Updater;

#[derive(Clone, Debug)]
pub struct WatchOptions {
    /// Coalesce events for this long before applying a batch.
    pub debounce: Duration,
    /// Persist the index (with the resume token) at least this often.
    pub save_every: Duration,
    /// Compact when tombstones exceed this many (and 1/8 of records).
    pub compact_min_tombstones: u64,
    /// Apply one batch, then return. For tests and scripts.
    pub once: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(500),
            save_every: Duration::from_secs(30),
            compact_min_tombstones: 10_000,
            once: false,
        }
    }
}

/// What one applied batch did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchReport {
    pub events: usize,
    pub overflow: bool,
    pub added: u64,
    pub removed: u64,
    pub updated: u64,
    pub compacted: bool,
    pub saved: bool,
    pub records: usize,
}

/// Run until the watcher closes (or after one batch with `opts.once`).
/// `published` always holds the latest consistent snapshot for readers.
pub fn watch_loop(
    updater: &mut Updater<'_>,
    watcher: &mut dyn Watcher,
    published: &ArcSwap<Index>,
    index_path: Option<&std::path::Path>,
    volume_id: &VolumeId,
    opts: &WatchOptions,
    mut on_batch: impl FnMut(&BatchReport),
) -> Result<()> {
    let mut events: Vec<WatchEvent> = Vec::new();
    let mut last_save = Instant::now();
    loop {
        // Wait for the first event, then keep draining for the debounce window.
        let first = watcher.poll(Duration::from_secs(1), &mut events)?;
        if first == PollOutcome::Closed {
            break;
        }
        if events.is_empty() {
            if last_save.elapsed() >= opts.save_every
                && let Some(p) = index_path
            {
                persist(updater, watcher, p, volume_id)?;
                last_save = Instant::now();
            }
            continue;
        }
        let deadline = Instant::now() + opts.debounce;
        while Instant::now() < deadline {
            match watcher.poll(
                deadline.saturating_duration_since(Instant::now()),
                &mut events,
            )? {
                PollOutcome::Events(_) => {}
                PollOutcome::Timeout | PollOutcome::Closed => break,
            }
        }

        let before = updater.stats;
        let mut report = BatchReport {
            events: events.len(),
            ..BatchReport::default()
        };
        for ev in events.drain(..) {
            match ev {
                WatchEvent::Path { path, deep, .. } => updater.reconcile_path(&path, deep),
                WatchEvent::Overflow | WatchEvent::RescanNeeded => {
                    report.overflow = true;
                    updater.sweep();
                }
            }
        }
        let after = updater.stats;
        report.added = after.added - before.added;
        report.removed = after.removed - before.removed;
        report.updated = after.updated - before.updated;
        report.compacted = updater.maybe_compact(opts.compact_min_tombstones);
        published.store(Arc::new(updater.snapshot()));
        report.records = updater.index().len();

        if let Some(p) = index_path
            && (last_save.elapsed() >= opts.save_every || report.compacted)
        {
            persist(updater, watcher, p, volume_id)?;
            last_save = Instant::now();
            report.saved = true;
        }
        on_batch(&report);
        if opts.once {
            break;
        }
    }
    if let Some(p) = index_path {
        persist(updater, watcher, p, volume_id)?;
    }
    Ok(())
}

fn persist(
    updater: &Updater<'_>,
    watcher: &dyn Watcher,
    path: &std::path::Path,
    volume_id: &VolumeId,
) -> Result<()> {
    let token = watcher.resume_token();
    save(
        updater.index(),
        path,
        &SaveMeta {
            volume_id,
            resume_token: &token.0,
        },
    )?;
    Ok(())
}
