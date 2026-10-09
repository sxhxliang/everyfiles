//! Parallel tree assembly.
//!
//! N enumerator threads pull directories from a shared FIFO queue, call the
//! backend's `read_dir`, and send each directory's children to a single
//! assembler. The assembler is the only thread that allocates ids, appends to
//! the index, and pushes newly discovered directories back onto the queue.
//!
//! Two details matter:
//!
//! - **INV-1 by construction.** A directory's id is allocated by the assembler
//!   when its parent's batch arrives, strictly before any worker can enumerate
//!   it, so every child id is larger than its parent's.
//! - **FIFO order.** `crossbeam_deque::Injector` is a FIFO, so the second
//!   level of a shallow tree spreads across all cores immediately instead of
//!   one subtree being descended to its leaves while 999 siblings wait.
//!
//! Termination uses a pending counter that only the assembler decrements,
//! *after* it has pushed a directory's children, so the count can never hit
//! zero while work is still in flight.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded};
use crossbeam_deque::{Injector, Steal};

use super::{Kind, RawEntry, Scanner, os_str_from_bytes};
use crate::error::Result;
use crate::index::{Id, Index, ROOT, flags};
use crate::platform::VolumeConfig;

/// Options for a full scan.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    /// Enumerator threads; `None` means `available_parallelism()`.
    pub threads: Option<usize>,
    /// Do not recurse below this depth (root is depth 0). Entries at the limit
    /// are still recorded.
    pub max_depth: Option<u32>,
}

/// Counters from a scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Directories recorded, including the root.
    pub dirs: u64,
    pub files: u64,
    pub symlinks: u64,
    pub others: u64,
    /// Directories that could not be read (permission denied, vanished, ...).
    pub errors: u64,
    pub elapsed: Duration,
}

struct Work {
    path: PathBuf,
    id: Id,
    depth: u32,
}

struct Batch {
    id: Id,
    path: PathBuf,
    depth: u32,
    result: Result<Vec<RawEntry>>,
}

/// Scan `root` with `scanner` and build a fresh index.
pub fn build_index(
    scanner: &dyn Scanner,
    root: &Path,
    cfg: VolumeConfig,
    opts: &ScanOptions,
) -> Result<(Index, ScanStats)> {
    let start = Instant::now();
    // Absolute and lexically normalized (`.` components removed), so the
    // stored root and every reconstructed path are clean.
    let root: PathBuf = std::path::absolute(root)?;
    let root_mtime = scanner.root_info(&root)?.mtime;

    let mut index = Index::new(cfg, root.clone(), root_mtime);
    let mut stats = ScanStats {
        dirs: 1,
        ..ScanStats::default()
    };

    let injector: Injector<Work> = Injector::new();
    let pending = AtomicUsize::new(1);
    let abort = AtomicBool::new(false);
    injector.push(Work {
        path: root,
        id: ROOT,
        depth: 0,
    });

    let nthreads = opts
        .threads
        .unwrap_or_else(|| thread::available_parallelism().map_or(4, |n| n.get()))
        .max(1);
    let (tx, rx) = bounded::<Batch>(nthreads * 64);

    let outcome: Result<()> = thread::scope(|s| {
        for _ in 0..nthreads {
            let tx = tx.clone();
            let injector = &injector;
            let pending = &pending;
            let abort = &abort;
            s.spawn(move || worker(scanner, injector, pending, abort, tx));
        }
        drop(tx);

        let mut result = Ok(());
        for batch in rx {
            match batch.result {
                Ok(entries) => {
                    for e in &entries {
                        let id = match push_entry(&mut index, batch.id, e, &mut stats) {
                            Ok(id) => id,
                            Err(err) => {
                                result = Err(err);
                                abort.store(true, Ordering::SeqCst);
                                break;
                            }
                        };
                        let recurse =
                            e.kind == Kind::Dir && opts.max_depth.is_none_or(|d| batch.depth < d);
                        if recurse {
                            // SAFETY: `e.name` was produced by a backend via
                            // `name_bytes`, per the `RawEntry` contract.
                            let name = unsafe { os_str_from_bytes(&e.name) };
                            pending.fetch_add(1, Ordering::SeqCst);
                            injector.push(Work {
                                path: batch.path.join(name),
                                id,
                                depth: batch.depth + 1,
                            });
                        }
                    }
                }
                Err(_) => stats.errors += 1,
            }
            // Decrement only after this directory's children are queued.
            pending.fetch_sub(1, Ordering::SeqCst);
            if result.is_err() {
                break;
            }
        }
        // If we broke out early, make sure workers exit.
        abort.store(true, Ordering::SeqCst);
        result
    });
    outcome?;

    stats.elapsed = start.elapsed();
    Ok((index, stats))
}

fn worker(
    scanner: &dyn Scanner,
    injector: &Injector<Work>,
    pending: &AtomicUsize,
    abort: &AtomicBool,
    tx: Sender<Batch>,
) {
    let mut idle_spins = 0u32;
    loop {
        if abort.load(Ordering::Relaxed) {
            return;
        }
        match injector.steal() {
            Steal::Success(work) => {
                idle_spins = 0;
                let mut entries = Vec::with_capacity(128);
                let result = scanner.read_dir(&work.path, &mut entries).map(|()| entries);
                let batch = Batch {
                    id: work.id,
                    path: work.path,
                    depth: work.depth,
                    result,
                };
                if tx.send(batch).is_err() {
                    return;
                }
            }
            Steal::Retry => {}
            Steal::Empty => {
                if pending.load(Ordering::SeqCst) == 0 {
                    return;
                }
                idle_spins += 1;
                if idle_spins < 64 {
                    thread::yield_now();
                } else {
                    thread::sleep(Duration::from_micros(50));
                }
            }
        }
    }
}

fn push_entry(index: &mut Index, parent: Id, e: &RawEntry, stats: &mut ScanStats) -> Result<Id> {
    let mut f = 0u8;
    match e.kind {
        Kind::Dir => {
            f |= flags::DIR;
            stats.dirs += 1;
        }
        Kind::File => stats.files += 1,
        Kind::Symlink => {
            f |= flags::SYMLINK;
            stats.symlinks += 1;
        }
        Kind::Other => stats.others += 1,
    }
    if e.attrs.hidden() {
        f |= flags::HIDDEN;
    }
    if e.attrs.system() {
        f |= flags::SYSTEM;
    }
    if e.attrs.reparse() {
        f |= flags::REPARSE;
    }
    if e.size.is_some() {
        f |= flags::HAS_SIZE;
    }
    if e.mtime.is_some() {
        f |= flags::HAS_MTIME;
    }
    index.push_record(
        &e.name,
        parent,
        f,
        e.size.unwrap_or(0),
        e.mtime.unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::SyntheticScanner;
    use std::collections::BTreeSet;

    fn all_paths(index: &Index) -> BTreeSet<PathBuf> {
        (1..index.len() as Id)
            .filter(|&id| !index.is_tombstone(id))
            .map(|id| index.resolve_path(id))
            .collect()
    }

    #[test]
    fn assembles_a_small_tree() {
        let s = SyntheticScanner::from_paths(
            "/syn",
            &["a/b/c.txt", "a/d.rs", "e/", "f.md", "a/b/g/h.bin"],
        );
        // Expectations derive from the scanner's root, the absolutized form
        // `build_index` scans (on Windows `"/syn"` becomes `D:\syn`), so the
        // assertions hold for whatever prefix and separators the host uses.
        let root = s.root();
        let (idx, stats) =
            build_index(&s, root, VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        let got = all_paths(&idx);
        let want: BTreeSet<PathBuf> = [
            "a",
            "a/b",
            "a/b/c.txt",
            "a/b/g",
            "a/b/g/h.bin",
            "a/d.rs",
            "e",
            "f.md",
        ]
        .into_iter()
        .map(|rel| root.join(rel))
        .collect();
        assert_eq!(got, want);
        assert_eq!(stats.files, 4);
        assert_eq!(stats.dirs, 5); // root + a, b, g, e
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn inv1_holds_for_every_record() {
        let mut paths = Vec::new();
        for i in 0..300 {
            paths.push(format!("d{}/d{}/f{i}", i % 7, i % 13));
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let s = SyntheticScanner::from_paths("/syn", &refs);
        let (idx, _) = build_index(
            &s,
            s.root(),
            VolumeConfig::unix(),
            &ScanOptions {
                threads: Some(3),
                max_depth: None,
            },
        )
        .unwrap();
        for id in 1..idx.len() as Id {
            assert!(idx.parent(id) < id, "INV-1 violated at {id}");
        }
        assert_eq!(idx.len(), 1 + s.entry_count());
    }

    #[test]
    fn max_depth_records_but_does_not_recurse() {
        let s = SyntheticScanner::from_paths("/syn", &["a/b/c/d.txt"]);
        let root = s.root();
        let (idx, _) = build_index(
            &s,
            root,
            VolumeConfig::unix(),
            &ScanOptions {
                threads: Some(1),
                max_depth: Some(1),
            },
        )
        .unwrap();
        let got = all_paths(&idx);
        // depth 0 = root; depth 1 = a (recorded, and we recurse into it since 0 < 1);
        // b is recorded at depth 2 but not recursed into.
        let want: BTreeSet<PathBuf> = ["a", "a/b"].into_iter().map(|rel| root.join(rel)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn unreadable_directory_is_counted_not_fatal() {
        let mut s = SyntheticScanner::from_paths("/syn", &["ok/x.txt", "broken/inner.txt"]);
        s.make_unreadable("broken");
        let root = s.root();
        let (idx, stats) =
            build_index(&s, root, VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        let got = all_paths(&idx);
        // The directory itself is still recorded (its parent listed it); its
        // contents are not, and the failure is counted rather than fatal.
        assert!(got.contains(&root.join("broken")));
        assert!(!got.contains(&root.join("broken/inner.txt")));
        assert!(got.contains(&root.join("ok/x.txt")));
        assert_eq!(stats.errors, 1);
    }
}
