//! The pool scanner: one SIMD substring search per block of the name pool,
//! then a linear merge of hit offsets back to record ids.
//!
//! Why this beats one `memmem` call per name: on ~20-byte names the per-call
//! setup dominates. Scanning a 1 MiB block in one pass is memory-bandwidth
//! bound instead. Names are NUL-terminated in the pool and a query never
//! contains `0x00`, so a hit can never straddle two names, and because
//! offsets increase with record id the hit→id mapping is a merge, not a
//! binary search per hit.
//!
//! Blocks are split into `CHUNK`-sized tasks for parallelism. Each task scans
//! `needle.len() - 1` bytes past its end so a match straddling the boundary is
//! found by the task where it starts, and discards hits that start beyond its
//! end. A name straddling the boundary can be reported by both tasks, which is
//! why the caller deduplicates adjacent ids.

use memchr::memmem;
use rayon::prelude::*;

use crate::index::{Id, NamePool, PoolView, ROOT};

/// Bytes per parallel task.
const CHUNK: usize = 128 * 1024;

/// Ids (ascending, deduplicated, root excluded) of records whose name in
/// `view` contains `needle` and satisfy `pred`.
pub(crate) fn scan_pool<F>(view: &PoolView<'_>, needle: &[u8], pred: F) -> Vec<Id>
where
    F: Fn(Id) -> bool + Sync,
{
    debug_assert!(!needle.is_empty(), "empty needle is handled by the caller");
    let n = view.off.len();
    if n == 0 {
        return Vec::new();
    }
    let finder = memmem::Finder::new(needle);
    let mut tasks: Vec<(usize, usize)> = Vec::new();
    for b in 0..view.pool.num_blocks() {
        let blen = view.pool.block(b).len();
        let mut c = 0;
        while c < blen {
            tasks.push((b, c));
            c += CHUNK;
        }
    }
    let run = |&(b, c): &(usize, usize)| scan_task(view, &finder, needle.len(), b, c, &pred);
    let mut ids: Vec<Id> = if tasks.len() <= 1 {
        tasks.iter().flat_map(run).collect()
    } else {
        tasks.par_iter().map(run).flatten().collect()
    };
    ids.dedup();
    ids
}

fn scan_task<F>(
    view: &PoolView<'_>,
    finder: &memmem::Finder<'_>,
    nlen: usize,
    b: usize,
    c: usize,
    pred: &F,
) -> Vec<Id>
where
    F: Fn(Id) -> bool + Sync,
{
    let blk = view.pool.block(b);
    let base = NamePool::block_base(b) as usize;
    let end_excl = (c + CHUNK).min(blk.len());
    let scan_end = (end_excl + nlen - 1).min(blk.len());
    let hay = &blk[c..scan_end];
    let n = view.off.len();

    // The record owning the first byte of this task: largest offset <= base+c.
    let g0 = (base + c) as u32;
    let first_gt = partition_point(n, |k| view.off.get(k) <= g0);
    let mut cur = first_gt.saturating_sub(1);

    let mut out = Vec::new();
    let mut last = Id::MAX;
    for h in finder.find_iter(hay) {
        let g = base + c + h;
        if g >= base + end_excl {
            break; // belongs to the next task
        }
        while cur + 1 < n && (view.off.get(cur + 1) as usize) <= g {
            cur += 1;
        }
        let id = cur as Id;
        if id == last {
            continue;
        }
        let o = view.off.get(cur) as usize;
        let l = view.len.get(cur) as usize;
        debug_assert!(g >= o && g + nlen <= o + l, "hit outside its owning name");
        if g < o || g + nlen > o + l {
            continue;
        }
        last = id;
        if id != ROOT && pred(id) {
            out.push(id);
        }
    }
    out
}

/// First index in `0..n` for which `pred` is false, assuming `pred` is
/// true for a prefix and false afterwards.
fn partition_point(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Index, POOL_BLOCK};
    use crate::platform::VolumeConfig;
    use std::path::PathBuf;

    /// ~60k names: spans several CHUNKs and crosses a POOL_BLOCK boundary.
    fn big_index() -> Index {
        let mut idx = Index::new(VolumeConfig::unix(), PathBuf::from("/r"), None);
        let words = [
            "report", "Final", "IMG", "notes", "main", "lib", "Invoice", "photo", "data",
        ];
        let exts = ["rs", "txt", "JPG", "pdf", "md", "csv"];
        let mut i = 0u64;
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        while idx.fold_pool_bytes() < POOL_BLOCK + CHUNK * 3 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let w = words[(s % words.len() as u64) as usize];
            let e = exts[((s >> 8) % exts.len() as u64) as usize];
            let name = match (s >> 16) % 3 {
                0 => format!("{w}_{i}.{e}"),
                1 => format!("{w}{}-{w}.{e}", i % 97),
                _ => format!("{}.{e}", "x".repeat((s >> 24) as usize % 40 + 1)),
            };
            idx.push_record(name.as_bytes(), 0, 0, i, 0).unwrap();
            i += 1;
        }
        assert!(
            idx.fold_view().pool.num_blocks() >= 2,
            "test must cross a block"
        );
        idx
    }

    fn naive(idx: &Index, needle: &[u8]) -> Vec<Id> {
        (1..idx.len() as Id)
            .filter(|&id| {
                let h = idx.folded(id);
                h.windows(needle.len()).any(|w| w == needle)
            })
            .collect()
    }

    #[test]
    fn scan_pool_agrees_with_naive_search() {
        let idx = big_index();
        let view = idx.fold_view();
        for needle in [
            "a",
            "rs",
            "img",
            "report_",
            "final9",
            ".jpg",
            "xxxxxxxxxx",
            "zzzz-none",
            "9-",
        ] {
            let n = crate::fold::fold(needle.as_bytes());
            let got = scan_pool(&view, &n, |_| true);
            let want = naive(&idx, &n);
            assert_eq!(got, want, "needle {needle:?}");
            assert!(got.windows(2).all(|w| w[0] < w[1]), "ascending and unique");
        }
    }

    #[test]
    fn predicate_filters_hits() {
        let idx = big_index();
        let view = idx.fold_view();
        let even = scan_pool(&view, b"a", |id| id % 2 == 0);
        assert!(!even.is_empty());
        assert!(even.iter().all(|id| id % 2 == 0));
    }

    #[test]
    fn partition_point_basics() {
        let v = [1, 2, 4, 4, 9];
        assert_eq!(partition_point(v.len(), |k| v[k] <= 4), 4);
        assert_eq!(partition_point(v.len(), |k| v[k] <= 0), 0);
        assert_eq!(partition_point(v.len(), |k| v[k] <= 100), 5);
    }
}
