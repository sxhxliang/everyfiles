//! Incremental updates: bring the index into agreement with the filesystem
//! one path at a time.
//!
//! The single primitive is [`Updater::reconcile_path`]: look at what the
//! index believes about a path, look at what the filesystem says, and fix
//! the difference. Every watcher event, and the catch-up sweep after a
//! restart, reduces to that. Deletions tombstone whole subtrees (INV-2) and
//! additions append (INV-1 by construction), so compaction stays a single
//! pass.

use std::path::{Path, PathBuf};

use crate::index::{ChildIndex, Id, Index, ROOT, flags};
use crate::scan::{EntryStat, Kind, RawEntry, Scanner, name_bytes, os_str_from_bytes};

/// Counters since the updater was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    pub added: u64,
    pub removed: u64,
    pub updated: u64,
    pub dirs_listed: u64,
    pub errors: u64,
}

/// Owns the writer's copy of the index and the child lookup structure.
pub struct Updater<'a> {
    index: Index,
    children: ChildIndex,
    scanner: &'a dyn Scanner,
    pub stats: UpdateStats,
}

impl std::fmt::Debug for Updater<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updater")
            .field("records", &self.index.len())
            .field("stats", &self.stats)
            .finish()
    }
}

enum Resolved {
    Found(Id),
    /// Every component up to `parent` exists; `first_missing` is the path of
    /// the first component that does not.
    Missing {
        parent: Id,
        first_missing: PathBuf,
    },
    Outside,
}

impl<'a> Updater<'a> {
    /// Takes ownership of the writer's index. Building the child lookup costs
    /// one sort of the live records.
    pub fn new(index: Index, scanner: &'a dyn Scanner) -> Self {
        let children = ChildIndex::build(&index);
        Self {
            index,
            children,
            scanner,
            stats: UpdateStats::default(),
        }
    }

    /// The writer's current index (not a snapshot; clone it to publish).
    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn into_index(self) -> Index {
        self.index
    }

    /// Clone for readers. Cheap: blocks are shared and copied on write.
    pub fn snapshot(&self) -> Index {
        self.index.clone()
    }

    fn resolve(&self, path: &Path) -> Resolved {
        let Ok(rel) = path.strip_prefix(self.index.root_path()) else {
            return Resolved::Outside;
        };
        let mut cur = ROOT;
        let mut walked = self.index.root_path().to_path_buf();
        for comp in rel.components() {
            let name = name_bytes(comp.as_os_str());
            walked.push(comp.as_os_str());
            match self.children.lookup(&self.index, cur, name) {
                Some(id) => cur = id,
                None => {
                    return Resolved::Missing {
                        parent: cur,
                        first_missing: walked,
                    };
                }
            }
        }
        Resolved::Found(cur)
    }

    /// Make the index agree with the filesystem at `path`. With `deep`, a
    /// directory's whole subtree is re-listed (for "may have missed events").
    pub fn reconcile_path(&mut self, path: &Path, deep: bool) {
        let path = match std::path::absolute(path) {
            Ok(p) => p,
            Err(_) => return,
        };
        match self.resolve(&path) {
            Resolved::Outside => {}
            Resolved::Found(id) => match self.scanner.stat_entry(&path) {
                Ok(Some(st)) => {
                    let was_dir = self.index.is_dir(id);
                    if was_dir != (st.kind == Kind::Dir) {
                        // Replaced by something of another kind: start over.
                        let parent = self.index.parent(id);
                        if id != ROOT {
                            self.remove_subtree(id);
                            self.add(parent, &path, st);
                        }
                    } else {
                        self.update_meta(id, st);
                        if st.kind == Kind::Dir {
                            self.reconcile_children(id, &path, deep);
                        }
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    if id != ROOT {
                        self.remove_subtree(id);
                    }
                }
            },
            Resolved::Missing {
                parent,
                first_missing,
            } => {
                if let Ok(Some(st)) = self.scanner.stat_entry(&first_missing) {
                    self.add(parent, &first_missing, st);
                }
            }
        }
    }

    /// Catch-up sweep from the root: re-list every directory whose mtime no
    /// longer matches (or is unknown), recursing into subdirectories either
    /// way. O(directories) stats; cannot see a size change to a file inside
    /// an otherwise untouched directory, which the live watcher covers.
    pub fn sweep(&mut self) {
        let root = self.index.root_path().to_path_buf();
        self.sweep_dir(ROOT, &root);
    }

    fn sweep_dir(&mut self, id: Id, path: &Path) {
        let known_mtime =
            (self.index.flags(id) & flags::HAS_MTIME != 0).then(|| self.index.mtime(id));
        let changed = match self.scanner.stat_entry(path) {
            Ok(Some(st)) if st.kind == Kind::Dir => {
                let changed = st.mtime.is_none() || known_mtime != st.mtime;
                if changed {
                    self.update_meta(id, st);
                }
                changed
            }
            Ok(Some(_)) | Err(_) => {
                if id != ROOT {
                    self.remove_subtree(id);
                }
                return;
            }
            Ok(None) => true,
        };
        if changed {
            self.reconcile_children(id, path, false);
        }
        let kids = self.children.children(&self.index, id);
        for k in kids {
            if self.index.is_dir(k) {
                let p = path.join(self.name_os(k));
                self.sweep_dir(k, &p);
            }
        }
    }

    /// Re-list `dir` and fix additions, removals, and metadata of its direct
    /// children; with `deep`, recurse into subdirectories.
    fn reconcile_children(&mut self, id: Id, dir: &Path, deep: bool) {
        let mut listed = Vec::with_capacity(64);
        if self.scanner.read_dir(dir, &mut listed).is_err() {
            self.stats.errors += 1;
            return;
        }
        self.stats.dirs_listed += 1;
        let mut seen: std::collections::HashSet<&[u8]> =
            std::collections::HashSet::with_capacity(listed.len());
        for e in &listed {
            seen.insert(&e.name);
            match self.children.lookup(&self.index, id, &e.name) {
                Some(child) => {
                    if self.index.is_dir(child) != (e.kind == Kind::Dir) {
                        self.remove_subtree(child);
                        self.add_entry(id, dir, e, deep);
                    } else {
                        self.update_meta(
                            child,
                            EntryStat {
                                kind: e.kind,
                                size: e.size,
                                mtime: e.mtime,
                            },
                        );
                        if deep && e.kind == Kind::Dir {
                            let p = dir.join(self.name_os(child));
                            self.reconcile_children(child, &p, true);
                        }
                    }
                }
                None => {
                    self.add_entry(id, dir, e, true);
                }
            }
        }
        let existing = self.children.children(&self.index, id);
        for child in existing {
            if !seen.contains(self.index.name(child)) {
                self.remove_subtree(child);
            }
        }
    }

    fn name_os(&self, id: Id) -> std::ffi::OsString {
        // SAFETY: stored names came from `name_bytes` on this platform.
        unsafe { os_str_from_bytes(self.index.name(id)) }.to_os_string()
    }

    /// Append a record for `path` (a child of `parent`); directories are
    /// listed recursively.
    fn add(&mut self, parent: Id, path: &Path, st: EntryStat) -> Option<Id> {
        let name = path.file_name()?;
        let e = RawEntry {
            name: name_bytes(name).into(),
            kind: st.kind,
            size: st.size,
            mtime: st.mtime,
            native_id: None,
            native_parent: None,
            attrs: Default::default(),
        };
        let dir = path.parent()?;
        self.add_entry(parent, dir, &e, true)
    }

    fn add_entry(&mut self, parent: Id, dir: &Path, e: &RawEntry, recurse: bool) -> Option<Id> {
        let mut f = 0u8;
        match e.kind {
            Kind::Dir => f |= flags::DIR,
            Kind::Symlink => f |= flags::SYMLINK,
            _ => {}
        }
        if e.size.is_some() {
            f |= flags::HAS_SIZE;
        }
        if e.mtime.is_some() {
            f |= flags::HAS_MTIME;
        }
        let id = self
            .index
            .push_record(
                &e.name,
                parent,
                f,
                e.size.unwrap_or(0),
                e.mtime.unwrap_or(0),
            )
            .ok()?;
        self.children.insert(parent, &e.name, id);
        self.stats.added += 1;
        if recurse && e.kind == Kind::Dir {
            // SAFETY: `e.name` was produced via `name_bytes` per the contract.
            let os = unsafe { os_str_from_bytes(&e.name) };
            let sub = dir.join(os);
            let mut listed = Vec::with_capacity(64);
            if self.scanner.read_dir(&sub, &mut listed).is_ok() {
                self.stats.dirs_listed += 1;
                for child in &listed {
                    self.add_entry(id, &sub, child, true);
                }
            } else {
                self.stats.errors += 1;
            }
        }
        Some(id)
    }

    fn update_meta(&mut self, id: Id, st: EntryStat) {
        let f = self.index.flags(id);
        let size_differs = st
            .size
            .is_some_and(|s| f & flags::HAS_SIZE == 0 || s != self.index.size(id));
        let mtime_differs = st
            .mtime
            .is_some_and(|m| f & flags::HAS_MTIME == 0 || m != self.index.mtime(id));
        if size_differs || mtime_differs {
            self.index.set_meta(id, st.size, st.mtime);
            self.stats.updated += 1;
        }
    }

    /// Tombstone `id` and every descendant (INV-2).
    fn remove_subtree(&mut self, id: Id) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if self.index.is_tombstone(cur) {
                continue;
            }
            if self.index.is_dir(cur) {
                stack.extend(self.children.children(&self.index, cur));
            }
            self.index.set_tombstone(cur);
            self.stats.removed += 1;
        }
    }

    /// Compact when tombstones exceed `max(min_tombstones, len/8)`.
    pub fn maybe_compact(&mut self, min_tombstones: u64) -> bool {
        let t = self.index.tombstone_count();
        if t == 0 || t < min_tombstones.max(self.index.len() as u64 / 8) {
            return false;
        }
        self.compact();
        true
    }

    pub fn compact(&mut self) {
        self.index = self.index.compact();
        self.children = ChildIndex::build(&self.index);
    }

    /// Fraction of the child lookup that lives in the unsorted delta.
    pub fn delta_len(&self) -> usize {
        self.children.delta_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::VolumeConfig;
    use crate::scan::{ScanOptions, build_index};
    use crate::testing::SyntheticScanner;
    use std::collections::BTreeSet;

    fn live(idx: &Index) -> BTreeSet<PathBuf> {
        (1..idx.len() as Id)
            .filter(|&i| !idx.is_tombstone(i))
            .map(|i| idx.resolve_path(i))
            .collect()
    }

    fn fresh(s: &SyntheticScanner) -> BTreeSet<PathBuf> {
        let (idx, _) =
            build_index(s, s.root(), VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        live(&idx)
    }

    fn setup() -> (SyntheticScanner, Index) {
        let s =
            SyntheticScanner::from_paths("/syn", &["a/one.txt", "a/b/two.txt", "c/three.md", "d/"]);
        let (idx, _) =
            build_index(&s, s.root(), VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        (s, idx)
    }

    #[test]
    fn create_remove_modify_converge_to_a_fresh_scan() {
        let (s0, idx) = setup();

        // Create a file and a whole new subtree; only the two "event" paths
        // are reconciled, and the subtree is picked up recursively.
        let mut s1 = s0.clone();
        s1.add_file("a/new.rs", 10, 10);
        s1.add_file("e/f/g/deep.txt", 5, 5);
        let mut u = Updater::new(idx, &s1);
        u.reconcile_path(Path::new("/syn/a/new.rs"), false);
        u.reconcile_path(Path::new("/syn/e"), false);
        assert_eq!(live(u.index()), fresh(&s1));
        assert_eq!(u.stats.added, 1 + 4, "new.rs + e, f, g, deep.txt");

        // Modify metadata.
        let mut s2 = s1.clone();
        s2.set_meta("a/one.txt", 999, 123);
        let mut u = Updater::new(u.into_index(), &s2);
        u.reconcile_path(Path::new("/syn/a/one.txt"), false);
        let a = u.children.lookup(u.index(), ROOT, b"a").unwrap();
        let one = u.children.lookup(u.index(), a, b"one.txt").unwrap();
        assert_eq!(u.index().size(one), 999);
        assert_eq!(u.index().mtime(one), 123);
        assert_eq!(u.stats.updated, 1);

        // Remove a subtree.
        let mut s3 = s2.clone();
        s3.remove("a");
        let mut u = Updater::new(u.into_index(), &s3);
        u.reconcile_path(Path::new("/syn/a"), false);
        assert_eq!(live(u.index()), fresh(&s3));
        assert_eq!(u.stats.removed, 5, "a, one.txt, new.rs, b, two.txt");
        assert!(u.index().tombstone_count() > 0);

        // Compaction keeps the same live set and resets tombstones.
        u.compact();
        assert_eq!(live(u.index()), fresh(&s3));
        assert_eq!(u.index().tombstone_count(), 0);
    }

    #[test]
    fn rename_is_remove_plus_create_and_deep_rescan_fixes_everything() {
        let (mut s, idx) = setup();
        s.remove("c/three.md");
        s.add_file("c/renamed.md", 300, 3);
        s.add_file("d/added_without_event.txt", 1, 1);
        let s = s;
        let mut u = Updater::new(idx, &s);
        u.reconcile_path(Path::new("/syn/c/three.md"), false);
        u.reconcile_path(Path::new("/syn/c/renamed.md"), false);
        // d changed without a per-file event: a deep reconcile of d catches it.
        u.reconcile_path(Path::new("/syn/d"), true);
        assert_eq!(live(u.index()), fresh(&s));
    }

    #[test]
    fn sweep_catches_up_after_offline_changes() {
        let (mut s, idx) = setup();
        s.add_file("a/b/offline.txt", 1, 1);
        s.remove("c");
        s.add_file("new_top/x.txt", 2, 2);
        s.bump_dir_mtime("a/b");
        s.bump_dir_mtime("");
        let s = s;
        let mut u = Updater::new(idx, &s);
        u.sweep();
        assert_eq!(live(u.index()), fresh(&s));
    }

    #[test]
    fn paths_outside_the_root_are_ignored() {
        let (s, idx) = setup();
        let mut u = Updater::new(idx, &s);
        u.reconcile_path(Path::new("/elsewhere/file"), false);
        assert_eq!(u.stats, UpdateStats::default());
    }
}
