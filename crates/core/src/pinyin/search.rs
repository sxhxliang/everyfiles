//! The index-level pinyin search: the SIMD substring scan answers every name
//! that has no non-ASCII byte, the DP answers the rest.
//!
//! A name with no byte >= 0x80 has nothing to read as pinyin, so it matches
//! exactly when the needle is a substring of it — which is the question the
//! ordinary pool scan already answers. The DP therefore only has to run on
//! names that can hold a Han character, and on most trees that is a small
//! fraction of them. Two passes over the pool find the two sets: `memmem` for
//! the substring hits, one byte-at-a-time SWAR scan for the names with a high
//! byte.

use rayon::prelude::*;

use super::Matcher;
use crate::index::{Id, Index, NamePool, PoolView};
use crate::query::matcher::{self, partition_point};
use crate::query::{PAR_THRESHOLD, Query, passes_filters};

/// Ids whose name matches the pinyin pattern, ascending.
pub(crate) fn search(index: &Index, q: &Query) -> Vec<Id> {
    let m = Matcher::new(q.needle.as_bytes());
    let n = index.len() as Id;
    if q.exact {
        // Whole-name matching is not a substring test, so the pool scan has
        // nothing to contribute; the DP runs over every record.
        let f = |id: Id| m.is_match_exact(index.folded(id)) && passes_filters(index, q, id);
        return if (n as usize) < PAR_THRESHOLD {
            (1..n).filter(|&id| f(id)).collect()
        } else {
            (1..n).into_par_iter().filter(|&id| f(id)).collect()
        };
    }

    let view = index.fold_view();
    let hits = matcher::scan_pool(&view, m.needle(), |id| passes_filters(index, q, id));
    let non_ascii = scan_non_ascii(&view);

    // `hits` is right for the names with no high byte and wrong for the ones
    // with a Han character in them, which the DP decides below.
    let mut out = Vec::with_capacity(hits.len());
    let mut j = 0;
    for &id in &hits {
        while j < non_ascii.len() && non_ascii[j] < id {
            j += 1;
        }
        if j == non_ascii.len() || non_ascii[j] != id {
            out.push(id);
        }
    }

    let f = |id: Id| m.is_match(index.folded(id)) && passes_filters(index, q, id);
    let dp: Vec<Id> = if non_ascii.len() < PAR_THRESHOLD {
        non_ascii.iter().filter(|&&id| f(id)).copied().collect()
    } else {
        non_ascii.par_iter().filter(|&&id| f(id)).copied().collect()
    };
    merge(&mut out, &dp);
    out
}

/// Merge two ascending, disjoint id lists into `out`.
fn merge(out: &mut Vec<Id>, other: &[Id]) {
    if other.is_empty() {
        return;
    }
    let mut merged = Vec::with_capacity(out.len() + other.len());
    let (mut i, mut j) = (0, 0);
    while i < out.len() && j < other.len() {
        if out[i] < other[j] {
            merged.push(out[i]);
            i += 1;
        } else {
            merged.push(other[j]);
            j += 1;
        }
    }
    merged.extend_from_slice(&out[i..]);
    merged.extend_from_slice(&other[j..]);
    *out = merged;
}

/// Ids whose folded name contains a byte >= 0x80, ascending.
fn scan_non_ascii(view: &PoolView<'_>) -> Vec<Id> {
    let blocks: Vec<Vec<Id>> = (0..view.pool.num_blocks())
        .into_par_iter()
        .map(|b| scan_block(view, b))
        .collect();
    blocks.concat()
}

/// One block: walk its high bytes and map each to the record that owns it.
/// Names never straddle a block and are NUL-terminated, so the owning record
/// only ever moves forward.
fn scan_block(view: &PoolView<'_>, b: usize) -> Vec<Id> {
    let blk = view.pool.block(b);
    let base = NamePool::block_base(b) as usize;
    let n = view.off.len();
    let first = partition_point(n, |k| (view.off.get(k) as usize) < base);
    let last = partition_point(n, |k| (view.off.get(k) as usize) < base + blk.len());
    let mut out = Vec::new();
    let mut cur = first;
    let mut pos = 0;
    while let Some(h) = find_high_byte(&blk[pos..]) {
        let g = pos + h;
        while cur < last && view.off.get(cur) as usize + view.len.get(cur) as usize <= g {
            cur += 1;
        }
        if cur < last && out.last() != Some(&(cur as Id)) {
            out.push(cur as Id);
        }
        pos = g + 1;
    }
    out
}

/// Offset of the first byte >= 0x80, if any. Eight bytes per step; on a tree
/// with no Chinese names this is the whole cost of a pinyin query.
#[inline]
fn find_high_byte(hay: &[u8]) -> Option<usize> {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let (chunks, tail) = hay.as_chunks::<8>();
    for (i, c) in chunks.iter().enumerate() {
        if u64::from_le_bytes(*c) & HIGH != 0 {
            let at = c.iter().position(|&b| b >= 0x80).expect("mask saw one");
            return Some(i * 8 + at);
        }
    }
    tail.iter()
        .position(|&b| b >= 0x80)
        .map(|at| chunks.len() * 8 + at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::VolumeConfig;
    use crate::query::{KindFilter, search as query_search};
    use crate::scan::{ScanOptions, build_index};
    use crate::testing::SyntheticScanner;

    fn fixture() -> Index {
        let s = SyntheticScanner::from_paths(
            "/syn",
            &[
                "src/main.rs",
                "docs/README.md",
                "拼音搜索Everything.txt",
                "我的搜索工具/",
                "我的搜索工具/报告2024.docx",
                "重庆火锅.jpg",
                "银行账单.pdf",
                "女装/",
                "报告📄最终.txt",
                "café.txt",
                "downloads/",
            ],
        );
        build_index(&s, s.root(), VolumeConfig::unix(), &ScanOptions::default())
            .unwrap()
            .0
    }

    /// The predicate the DP path applies, run over every record.
    fn reference(idx: &Index, q: &Query) -> Vec<Id> {
        let m = Matcher::new(q.needle.as_bytes());
        (1..idx.len() as Id)
            .filter(|&id| {
                let hit = if q.exact {
                    m.is_match_exact(idx.folded(id))
                } else {
                    m.is_match(idx.folded(id))
                };
                hit && !idx.is_tombstone(id)
            })
            .collect()
    }

    fn names(idx: &Index, ids: &[Id]) -> Vec<String> {
        ids.iter()
            .map(|&i| String::from_utf8_lossy(idx.name(i)).into_owned())
            .collect()
    }

    fn query(needle: &str, exact: bool) -> Query {
        Query {
            needle: needle.into(),
            exact,
            pinyin: true,
            ..Query::default()
        }
    }

    /// The dispatch (pool scan for ASCII names, DP for the rest, merge) has to
    /// agree with the DP alone on every record.
    #[test]
    fn the_two_paths_agree() {
        let idx = fixture();
        for needle in [
            "py",
            "pysousuo",
            "pysousuoeve",
            "pysousuoeverything",
            "sousuo",
            "ss",
            "bg",
            "gj",
            "报告",
            "café",
            "eve",
            "zzz",
            "hs",
            "chongqing",
            "zhongqing",
            "nv",
            "nu",
            "📄",
            "a",
            "rs",
            "main",
        ] {
            for exact in [false, true] {
                let q = query(needle, exact);
                let got = query_search(&idx, &q).unwrap().ids;
                assert_eq!(got, reference(&idx, &q), "needle {needle:?} exact {exact}");
            }
        }
    }

    #[test]
    fn finds_han_names_by_pinyin() {
        let idx = fixture();
        let got = names(
            &idx,
            &query_search(&idx, &query("pysousuo", false)).unwrap().ids,
        );
        assert_eq!(got, ["拼音搜索Everything.txt"]);

        let got = names(
            &idx,
            &query_search(&idx, &query("wdssgj", false)).unwrap().ids,
        );
        assert_eq!(got, ["我的搜索工具"]);

        let mut got = names(&idx, &query_search(&idx, &query("bg", false)).unwrap().ids);
        got.sort();
        assert_eq!(got, ["报告2024.docx", "报告📄最终.txt"]);

        let got = names(&idx, &query_search(&idx, &query("cq", false)).unwrap().ids);
        assert_eq!(got, ["重庆火锅.jpg"]);

        let got = names(&idx, &query_search(&idx, &query("nz", false)).unwrap().ids);
        assert_eq!(got, ["女装"]);
    }

    #[test]
    fn exact_pins_the_whole_name() {
        let idx = fixture();
        let all = query_search(&idx, &query("pysousuoeverything.txt", true)).unwrap();
        assert_eq!(names(&idx, &all.ids), ["拼音搜索Everything.txt"]);
        assert!(
            query_search(&idx, &query("pysousuoeve", true))
                .unwrap()
                .ids
                .is_empty()
        );
    }

    #[test]
    fn filters_still_apply() {
        let idx = fixture();
        let mut q = query("bg", false);
        q.kind = KindFilter::DirsOnly;
        assert!(query_search(&idx, &q).unwrap().ids.is_empty());
        let mut q = query("bg", false);
        q.min_size = Some(1);
        q.kind = KindFilter::FilesOnly;
        let got = names(&idx, &query_search(&idx, &q).unwrap().ids);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn empty_needle_matches_everything_but_root() {
        let idx = fixture();
        let m = query_search(&idx, &query("", false)).unwrap();
        assert_eq!(m.ids.len(), idx.len() - 1);
    }

    #[test]
    fn wildcards_and_case_sensitivity_are_rejected() {
        let idx = fixture();
        let mut q = query("*.txt", false);
        q.needle = "*.txt".into();
        assert!(matches!(
            query_search(&idx, &q),
            Err(crate::Error::Query(_))
        ));
        let mut q = query("py", false);
        q.case_sensitive = true;
        assert!(matches!(
            query_search(&idx, &q),
            Err(crate::Error::Query(_))
        ));
    }

    #[test]
    fn high_byte_scan_finds_every_owner() {
        let idx = fixture();
        let view = idx.fold_view();
        let mut want: Vec<Id> = (0..idx.len() as Id)
            .filter(|&id| !idx.folded(id).is_ascii())
            .collect();
        want.sort_unstable();
        assert_eq!(scan_non_ascii(&view), want);
    }

    #[test]
    fn find_high_byte_matches_the_naive_scan() {
        for hay in [
            &b""[..],
            b"abcdefgh",
            b"abcdefg",
            b"abc\xffdef",
            b"\xff",
            b"abcdefg\xff",
            b"abcdefghijklmnopqrstuvwxyz\xe4\xb8\xad",
        ] {
            assert_eq!(
                find_high_byte(hay),
                hay.iter().position(|&b| b >= 0x80),
                "{hay:?}"
            );
        }
    }
}
