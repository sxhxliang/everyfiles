//! Child lookup: "the record named `X` under directory `P`".
//!
//! The base is a permutation of live record ids sorted by `(parent, name)`,
//! so both *children of P* and *child of P named X* are binary searches.
//! Records added after the base was built live in a small per-parent map,
//! consulted first. A tombstoned base record is simply skipped on lookup, so
//! deletions never need to touch the permutation; compaction rebuilds it.

use std::collections::HashMap;

use rayon::prelude::*;

use super::{Id, Index};

#[derive(Debug, Default, Clone)]
pub struct ChildIndex {
    base: Vec<Id>,
    delta: HashMap<Id, HashMap<Vec<u8>, Id>>,
}

impl ChildIndex {
    /// Sort every live non-root record by `(parent, name)`.
    pub fn build(index: &Index) -> Self {
        let mut base: Vec<Id> = (1..index.len() as Id)
            .filter(|&id| !index.is_tombstone(id))
            .collect();
        let key = |id: Id| (index.parent(id), index.name(id));
        if base.len() < 32 * 1024 {
            base.sort_unstable_by(|&a, &b| key(a).cmp(&key(b)));
        } else {
            base.par_sort_unstable_by(|&a, &b| key(a).cmp(&key(b)));
        }
        Self {
            base,
            delta: HashMap::new(),
        }
    }

    /// Record a newly appended child.
    pub fn insert(&mut self, parent: Id, name: &[u8], id: Id) {
        self.delta
            .entry(parent)
            .or_default()
            .insert(name.to_vec(), id);
    }

    /// The live child of `parent` named exactly `name`, if any.
    pub fn lookup(&self, index: &Index, parent: Id, name: &[u8]) -> Option<Id> {
        if let Some(m) = self.delta.get(&parent)
            && let Some(&id) = m.get(name)
        {
            return (!index.is_tombstone(id)).then_some(id);
        }
        let (lo, hi) = self.range(index, parent);
        let slice = &self.base[lo..hi];
        let pos = slice.partition_point(|&id| index.name(id) < name);
        match slice.get(pos) {
            Some(&id) if index.name(id) == name && !index.is_tombstone(id) => Some(id),
            _ => None,
        }
    }

    /// All live children of `parent`.
    pub fn children(&self, index: &Index, parent: Id) -> Vec<Id> {
        let (lo, hi) = self.range(index, parent);
        let mut v: Vec<Id> = self.base[lo..hi]
            .iter()
            .copied()
            .filter(|&id| !index.is_tombstone(id))
            .collect();
        if let Some(m) = self.delta.get(&parent) {
            v.extend(m.values().copied().filter(|&id| !index.is_tombstone(id)));
        }
        v
    }

    /// Entries added since `build` (the part not in the sorted base).
    pub fn delta_len(&self) -> usize {
        self.delta.values().map(HashMap::len).sum()
    }

    fn range(&self, index: &Index, parent: Id) -> (usize, usize) {
        let lo = self.base.partition_point(|&id| index.parent(id) < parent);
        let hi = lo + self.base[lo..].partition_point(|&id| index.parent(id) == parent);
        (lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ROOT, flags};
    use crate::platform::VolumeConfig;
    use std::path::PathBuf;

    #[test]
    fn lookup_and_children_across_base_and_delta() {
        let mut i = Index::new(VolumeConfig::unix(), PathBuf::from("/r"), None);
        let a = i.push_record(b"a", ROOT, flags::DIR, 0, 0).unwrap();
        let x = i.push_record(b"x.txt", a, 0, 0, 0).unwrap();
        let y = i.push_record(b"y.txt", a, 0, 0, 0).unwrap();
        let z = i.push_record(b"z", ROOT, 0, 0, 0).unwrap();
        let mut c = ChildIndex::build(&i);
        assert_eq!(c.lookup(&i, ROOT, b"a"), Some(a));
        assert_eq!(c.lookup(&i, a, b"y.txt"), Some(y));
        assert_eq!(c.lookup(&i, a, b"nope"), None);
        assert_eq!(c.lookup(&i, ROOT, b"x.txt"), None, "wrong parent");
        let mut kids = c.children(&i, a);
        kids.sort();
        assert_eq!(kids, [x, y]);

        // Tombstoned base record disappears from lookups without a rebuild.
        i.set_tombstone(x);
        assert_eq!(c.lookup(&i, a, b"x.txt"), None);
        assert_eq!(c.children(&i, a), [y]);

        // Re-created under the same name: delta wins.
        let x2 = i.push_record(b"x.txt", a, 0, 0, 0).unwrap();
        c.insert(a, b"x.txt", x2);
        assert_eq!(c.lookup(&i, a, b"x.txt"), Some(x2));
        let mut kids = c.children(&i, a);
        kids.sort();
        assert_eq!(kids, [y, x2]);
        assert_eq!(c.children(&i, ROOT).len(), 2);
        let _ = z;
    }
}
