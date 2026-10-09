//! The in-memory index: struct-of-arrays records plus two name pools.
//!
//! Every record is one slot across eight parallel arrays, and its id is the
//! slot number. Paths are never stored; [`Index::resolve_path_into`] rebuilds
//! one by walking `parent` to the root. Two invariants make the whole design
//! work:
//!
//! - **INV-1:** for every record `r != ROOT`, `parent(r) < r`. The assembler
//!   allocates a directory's id before enumerating its children, so this holds
//!   by construction. It makes the tree provably acyclic, makes path
//!   reconstruction provably terminate, and makes compaction a single pass.
//! - **INV-2:** if a record is tombstoned, every descendant is tombstoned.
//!
//! Memory per record is 33 bytes of metadata plus the name bytes twice (raw
//! and case-folded, each NUL-terminated in its pool).

mod block;
mod children;
mod chunked;
mod pool;

pub use children::ChildIndex;
pub use chunked::{BLOCK, BLOCK_SHIFT, Chunked};
pub use pool::{NamePool, POOL_BLOCK, POOL_SHIFT};

use std::path::{Path, PathBuf};

use smallvec::SmallVec;

use crate::error::Result;
use crate::fold;
use crate::platform::VolumeConfig;
use crate::scan::{name_bytes, os_str_from_bytes};

/// Record identifier: the slot index across all arrays.
pub type Id = u32;

/// The root directory is always record 0 and is its own parent.
pub const ROOT: Id = 0;

/// Bit flags stored per record.
pub mod flags {
    pub const DIR: u8 = 1 << 0;
    pub const TOMBSTONE: u8 = 1 << 1;
    pub const SYMLINK: u8 = 1 << 2;
    pub const HIDDEN: u8 = 1 << 3;
    pub const SYSTEM: u8 = 1 << 4;
    pub const REPARSE: u8 = 1 << 5;
    /// `size` is meaningful (the backend supplied it).
    pub const HAS_SIZE: u8 = 1 << 6;
    /// `mtime` is meaningful (the backend supplied it).
    pub const HAS_MTIME: u8 = 1 << 7;
}

/// A pool together with the per-record offset and length arrays that index
/// into it. The query engine scans one of these.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoolView<'a> {
    pub pool: &'a NamePool,
    pub off: &'a Chunked<u32>,
    pub len: &'a Chunked<u16>,
}

/// The index. Readers get a consistent view; mutation is confined to the
/// engine-internal methods used by the assembler and the updater.
///
/// `Clone` is cheap and structural: blocks are shared, and a later write to a
/// shared block copies it first. Cloning is how a read snapshot is published.
#[derive(Debug, Clone)]
pub struct Index {
    pub(crate) cfg: VolumeConfig,
    pub(crate) root_path: PathBuf,
    pub(crate) name_pool: NamePool,
    pub(crate) fold_pool: NamePool,
    pub(crate) name_off: Chunked<u32>,
    pub(crate) name_len: Chunked<u16>,
    pub(crate) fold_off: Chunked<u32>,
    pub(crate) fold_len: Chunked<u16>,
    pub(crate) parent: Chunked<u32>,
    pub(crate) size: Chunked<u64>,
    pub(crate) mtime: Chunked<i64>,
    pub(crate) flags: Chunked<u8>,
    pub(crate) fold_scratch: Vec<u8>,
    pub(crate) tombstones: u64,
}

/// The storage pieces of an index, used by the store to rebuild one.
pub(crate) struct Parts {
    pub name_pool: NamePool,
    pub fold_pool: NamePool,
    pub name_off: Chunked<u32>,
    pub name_len: Chunked<u16>,
    pub fold_off: Chunked<u32>,
    pub fold_len: Chunked<u16>,
    pub parent: Chunked<u32>,
    pub size: Chunked<u64>,
    pub mtime: Chunked<i64>,
    pub flags: Chunked<u8>,
    pub tombstones: u64,
}

impl Index {
    /// Create an index whose record 0 is `root`.
    pub fn new(cfg: VolumeConfig, root: PathBuf, root_mtime: Option<i64>) -> Self {
        let mut idx = Self::empty(cfg, root);
        let name: Vec<u8> = idx
            .root_path
            .file_name()
            .map(name_bytes)
            .unwrap_or_else(|| name_bytes(idx.root_path.as_os_str()))
            .to_vec();
        let mut f = flags::DIR;
        if root_mtime.is_some() {
            f |= flags::HAS_MTIME;
        }
        idx.push_record(&name, ROOT, f, 0, root_mtime.unwrap_or(0))
            .expect("root record always fits");
        idx
    }

    /// An index with no records at all. Only `new` and `compact` use this.
    fn empty(cfg: VolumeConfig, root: PathBuf) -> Self {
        Self {
            cfg,
            root_path: root,
            name_pool: NamePool::new(),
            fold_pool: NamePool::new(),
            name_off: Chunked::new(),
            name_len: Chunked::new(),
            fold_off: Chunked::new(),
            fold_len: Chunked::new(),
            parent: Chunked::new(),
            size: Chunked::new(),
            mtime: Chunked::new(),
            flags: Chunked::new(),
            fold_scratch: Vec::with_capacity(256),
            tombstones: 0,
        }
    }

    /// Reassemble an index from loaded storage.
    pub(crate) fn from_parts(cfg: VolumeConfig, root_path: PathBuf, p: Parts) -> Self {
        Self {
            cfg,
            root_path,
            name_pool: p.name_pool,
            fold_pool: p.fold_pool,
            name_off: p.name_off,
            name_len: p.name_len,
            fold_off: p.fold_off,
            fold_len: p.fold_len,
            parent: p.parent,
            size: p.size,
            mtime: p.mtime,
            flags: p.flags,
            fold_scratch: Vec::with_capacity(256),
            tombstones: p.tombstones,
        }
    }

    // ---- read API -------------------------------------------------------

    /// Total records, including tombstones and the root.
    #[inline]
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Records that are not tombstoned.
    pub fn live_count(&self) -> u64 {
        self.len() as u64 - self.tombstones
    }

    pub fn tombstone_count(&self) -> u64 {
        self.tombstones
    }

    pub fn config(&self) -> &VolumeConfig {
        &self.cfg
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Raw name bytes of a record (one path component).
    #[inline]
    pub fn name(&self, id: Id) -> &[u8] {
        let i = id as usize;
        self.name_pool
            .get(self.name_off.get(i), self.name_len.get(i))
    }

    /// Case-folded name bytes of a record.
    #[inline]
    pub fn folded(&self, id: Id) -> &[u8] {
        let i = id as usize;
        self.fold_pool
            .get(self.fold_off.get(i), self.fold_len.get(i))
    }

    #[inline]
    pub fn parent(&self, id: Id) -> Id {
        self.parent.get(id as usize)
    }

    #[inline]
    pub fn size(&self, id: Id) -> u64 {
        self.size.get(id as usize)
    }

    /// Nanoseconds since the Unix epoch.
    #[inline]
    pub fn mtime(&self, id: Id) -> i64 {
        self.mtime.get(id as usize)
    }

    #[inline]
    pub fn flags(&self, id: Id) -> u8 {
        self.flags.get(id as usize)
    }

    #[inline]
    pub fn is_dir(&self, id: Id) -> bool {
        self.flags(id) & flags::DIR != 0
    }

    #[inline]
    pub fn is_tombstone(&self, id: Id) -> bool {
        self.flags(id) & flags::TOMBSTONE != 0
    }

    /// Bytes used by the raw name pool, including block padding.
    pub fn name_pool_bytes(&self) -> usize {
        self.name_pool.bytes_used()
    }

    pub fn fold_pool_bytes(&self) -> usize {
        self.fold_pool.bytes_used()
    }

    /// Bytes served straight from a memory-mapped index file (zero-copy).
    pub fn mapped_bytes(&self) -> usize {
        self.name_pool.bytes_mapped()
            + self.fold_pool.bytes_mapped()
            + self.name_off.bytes_mapped()
            + self.name_len.bytes_mapped()
            + self.fold_off.bytes_mapped()
            + self.fold_len.bytes_mapped()
            + self.parent.bytes_mapped()
            + self.size.bytes_mapped()
            + self.mtime.bytes_mapped()
            + self.flags.bytes_mapped()
    }

    /// Approximate heap bytes of the index (excludes mapped blocks).
    pub fn approx_bytes(&self) -> usize {
        self.name_pool.bytes_allocated()
            + self.fold_pool.bytes_allocated()
            + self.name_off.bytes_allocated()
            + self.name_len.bytes_allocated()
            + self.fold_off.bytes_allocated()
            + self.fold_len.bytes_allocated()
            + self.parent.bytes_allocated()
            + self.size.bytes_allocated()
            + self.mtime.bytes_allocated()
            + self.flags.bytes_allocated()
    }

    pub(crate) fn name_view(&self) -> PoolView<'_> {
        PoolView {
            pool: &self.name_pool,
            off: &self.name_off,
            len: &self.name_len,
        }
    }

    pub(crate) fn fold_view(&self) -> PoolView<'_> {
        PoolView {
            pool: &self.fold_pool,
            off: &self.fold_off,
            len: &self.fold_len,
        }
    }

    /// Rebuild the full path of `id` into `out` (cleared first). Only ever call
    /// this for the records you are about to show; it is O(depth).
    pub fn resolve_path_into(&self, id: Id, out: &mut Vec<u8>) {
        out.clear();
        let mut chain: SmallVec<[Id; 32]> = SmallVec::new();
        let mut cur = id;
        // INV-1 guarantees `parent(cur) < cur`, so this strictly decreases
        // and terminates.
        while cur != ROOT {
            chain.push(cur);
            cur = self.parent(cur);
        }
        out.extend_from_slice(name_bytes(self.root_path.as_os_str()));
        let sep = self.cfg.separator;
        for &c in chain.iter().rev() {
            if out.last().is_some_and(|&b| b != sep) {
                out.push(sep);
            }
            out.extend_from_slice(self.name(c));
        }
    }

    /// Convenience wrapper around [`Self::resolve_path_into`].
    pub fn resolve_path(&self, id: Id) -> PathBuf {
        let mut v = Vec::with_capacity(256);
        self.resolve_path_into(id, &mut v);
        // SAFETY: every byte in `v` is either the root path's own encoded
        // bytes, a record name that a backend produced with
        // `OsStr::as_encoded_bytes`, or an ASCII separator. The `OsStr`
        // documentation permits exactly this kind of concatenation.
        PathBuf::from(unsafe { os_str_from_bytes(&v) })
    }

    // ---- write API (engine-internal) -----------------------------------

    /// Append a record. `parent` must already exist (INV-1).
    pub(crate) fn push_record(
        &mut self,
        name: &[u8],
        parent: Id,
        flags: u8,
        size: u64,
        mtime: i64,
    ) -> Result<Id> {
        let id_usize = self.len();
        if id_usize >= u32::MAX as usize {
            return Err(crate::Error::IndexFull);
        }
        if name.len() > u16::MAX as usize {
            return Err(crate::Error::NameTooLong(name.len()));
        }
        let id = id_usize as Id;
        debug_assert!(
            parent < id || (id == ROOT && parent == ROOT),
            "INV-1 violated: parent {parent} >= id {id}"
        );

        let noff = self.name_pool.push(name)?;
        self.fold_scratch.clear();
        fold::fold_into(name, &mut self.fold_scratch);
        if self.fold_scratch.len() > u16::MAX as usize {
            return Err(crate::Error::NameTooLong(self.fold_scratch.len()));
        }
        let foff = self.fold_pool.push(&self.fold_scratch)?;

        self.name_off.push(noff);
        self.name_len.push(name.len() as u16);
        self.fold_off.push(foff);
        self.fold_len.push(self.fold_scratch.len() as u16);
        self.parent.push(parent);
        self.size.push(size);
        self.mtime.push(mtime);
        self.flags.push(flags);
        Ok(id)
    }

    /// Update size/mtime (and their validity flags) in place.
    pub(crate) fn set_meta(&mut self, id: Id, size: Option<u64>, mtime: Option<i64>) {
        let i = id as usize;
        let mut f = self.flags.get(i);
        if let Some(s) = size {
            self.size.set(i, s);
            f |= flags::HAS_SIZE;
        }
        if let Some(m) = mtime {
            self.mtime.set(i, m);
            f |= flags::HAS_MTIME;
        }
        self.flags.set(i, f);
    }

    /// Rebuild without tombstones. One forward pass: INV-1 guarantees a
    /// record's parent was already renumbered when the record is reached, and
    /// INV-2 guarantees that parent is live.
    pub fn compact(&self) -> Index {
        let n = self.len();
        let mut remap: Vec<Id> = vec![Id::MAX; n];
        let mut out = Self::empty(self.cfg, self.root_path.clone());
        for id in 0..n as Id {
            if self.is_tombstone(id) {
                continue;
            }
            let parent = remap[self.parent(id) as usize];
            debug_assert!(
                parent != Id::MAX || id == ROOT,
                "INV-2 violated: live record {id} under tombstoned parent"
            );
            let parent = if id == ROOT { ROOT } else { parent };
            let new_id = out
                .push_record(
                    self.name(id),
                    parent,
                    self.flags(id),
                    self.size(id),
                    self.mtime(id),
                )
                .expect("compacted index is no larger than the source");
            remap[id as usize] = new_id;
        }
        out
    }

    /// Mark a record deleted. The caller is responsible for INV-2: deleting a
    /// directory must tombstone its whole subtree.
    pub fn set_tombstone(&mut self, id: Id) {
        let i = id as usize;
        let f = self.flags.get(i);
        if f & flags::TOMBSTONE == 0 {
            self.flags.set(i, f | flags::TOMBSTONE);
            self.tombstones += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> Index {
        Index::new(VolumeConfig::unix(), PathBuf::from("/root"), None)
    }

    #[test]
    fn root_is_record_zero_and_its_own_parent() {
        let i = idx();
        assert_eq!(i.len(), 1);
        assert_eq!(i.parent(ROOT), ROOT);
        assert!(i.is_dir(ROOT));
        assert_eq!(i.name(ROOT), b"root");
        assert_eq!(i.resolve_path(ROOT), PathBuf::from("/root"));
    }

    #[test]
    fn paths_reconstruct_through_the_parent_chain() {
        let mut i = idx();
        let a = i.push_record(b"a", ROOT, flags::DIR, 0, 0).unwrap();
        let b = i.push_record(b"b", a, flags::DIR, 0, 0).unwrap();
        let f = i
            .push_record(b"File.TXT", b, flags::HAS_SIZE, 42, 7)
            .unwrap();
        assert_eq!(i.resolve_path(f), PathBuf::from("/root/a/b/File.TXT"));
        assert_eq!(i.folded(f), b"file.txt");
        assert_eq!(i.size(f), 42);
        assert_eq!(i.mtime(f), 7);
        assert_eq!(i.live_count(), 4);
    }

    #[test]
    fn root_at_filesystem_root_does_not_double_separator() {
        let mut i = Index::new(VolumeConfig::unix(), PathBuf::from("/"), None);
        let a = i.push_record(b"usr", ROOT, flags::DIR, 0, 0).unwrap();
        assert_eq!(i.resolve_path(a), PathBuf::from("/usr"));
    }

    #[test]
    fn tombstone_counts_once() {
        let mut i = idx();
        let a = i.push_record(b"a", ROOT, 0, 0, 0).unwrap();
        i.set_tombstone(a);
        i.set_tombstone(a);
        assert!(i.is_tombstone(a));
        assert_eq!(i.tombstone_count(), 1);
        assert_eq!(i.live_count(), 1);
    }

    #[test]
    fn compaction_drops_tombstones_and_preserves_paths() {
        let mut i = idx();
        let a = i.push_record(b"a", ROOT, flags::DIR, 0, 0).unwrap();
        let b = i.push_record(b"b", a, flags::DIR, 0, 0).unwrap();
        let _f1 = i.push_record(b"f1", b, flags::HAS_SIZE, 1, 0).unwrap();
        let gone = i.push_record(b"gone", ROOT, flags::DIR, 0, 0).unwrap();
        let gone_child = i.push_record(b"gc", gone, 0, 0, 0).unwrap();
        let f2 = i.push_record(b"f2", a, flags::HAS_SIZE, 2, 0).unwrap();
        i.set_tombstone(gone_child);
        i.set_tombstone(gone);
        let c = i.compact();
        assert_eq!(c.len(), 5);
        assert_eq!(c.tombstone_count(), 0);
        let paths: std::collections::BTreeSet<PathBuf> =
            (1..c.len() as Id).map(|id| c.resolve_path(id)).collect();
        let want: std::collections::BTreeSet<PathBuf> =
            ["/root/a", "/root/a/b", "/root/a/b/f1", "/root/a/f2"]
                .into_iter()
                .map(PathBuf::from)
                .collect();
        assert_eq!(paths, want);
        for id in 1..c.len() as Id {
            assert!(c.parent(id) < id, "INV-1 after compaction");
        }
        let _ = f2;
    }

    #[test]
    fn clone_is_a_stable_snapshot() {
        let mut i = idx();
        let a = i.push_record(b"a", ROOT, 0, 1, 0).unwrap();
        let snap = i.clone();
        i.set_tombstone(a);
        i.push_record(b"b", ROOT, 0, 2, 0).unwrap();
        assert!(!snap.is_tombstone(a));
        assert_eq!(snap.len(), 2);
        assert!(i.is_tombstone(a));
        assert_eq!(i.len(), 3);
    }

    #[test]
    fn many_records_cross_block_boundaries() {
        let mut i = idx();
        let n = BLOCK * 2 + 17;
        for k in 0..n {
            let name = format!("f{k}");
            i.push_record(name.as_bytes(), ROOT, 0, k as u64, 0)
                .unwrap();
        }
        assert_eq!(i.len(), n + 1);
        let last = n as Id;
        assert_eq!(i.name(last), format!("f{}", n - 1).as_bytes());
        assert_eq!(i.size(last), (n - 1) as u64);
        assert_eq!(i.name(BLOCK as Id), format!("f{}", BLOCK - 1).as_bytes());
    }
}
