//! Query: match, filter, sort, page.
//!
//! Matching is a case-insensitive substring search over the folded names.
//! Filters compile into one per-record predicate ordered cheapest-first, so
//! there is never a second pass over memory. Sorting works on the matched id
//! list, and callers reconstruct paths only for the page they display.
//!
//! Substring matching runs one SIMD scan per block of the folded name pool
//! and merges hit offsets back to ids (see [`matcher`]). Small indexes are
//! scanned on the calling thread so a tiny query never pays for a thread pool.
//! A needle containing `*` or `?` is matched as a whole-name glob instead
//! (see [`glob`]), prefiltered by the longest literal run in the pattern.
//! With the `pinyin` feature, [`Query::pinyin`] reads the needle as a pinyin
//! pattern over Han characters instead (see [`crate::pinyin`]).

mod glob;
pub(crate) mod matcher;

use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::fold;
use crate::index::{Id, Index, flags};

/// Restrict results by record kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    Any,
    FilesOnly,
    DirsOnly,
}

/// Sort order for results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Mtime,
    /// Full path. Requires reconstructing every matched path; fine for
    /// pages, expensive for very large result sets.
    Path,
}

/// A search request.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub needle: String,
    pub case_sensitive: bool,
    /// Whole-name match instead of substring.
    pub exact: bool,
    /// Read the needle as a pinyin pattern (feature `pinyin`): each ASCII
    /// letter matches an ASCII character of the name or part of a Han
    /// character's reading. Always case-insensitive, and never a glob.
    #[cfg(feature = "pinyin")]
    pub pinyin: bool,
    pub kind: KindFilter,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Inclusive lower bound on mtime (ns since epoch).
    pub after: Option<i64>,
    /// Inclusive upper bound on mtime (ns since epoch).
    pub before: Option<i64>,
    pub include_hidden: bool,
}

impl Query {
    pub fn new(needle: impl Into<String>) -> Self {
        Self {
            needle: needle.into(),
            ..Self::default()
        }
    }

    /// Reject queries this version cannot answer correctly rather than
    /// silently answering something else.
    pub fn validate(&self) -> Result<()> {
        if let (Some(lo), Some(hi)) = (self.min_size, self.max_size)
            && lo > hi
        {
            return Err(Error::Query("--min-size is greater than --max-size".into()));
        }
        #[cfg(feature = "pinyin")]
        if self.pinyin {
            if self.case_sensitive {
                return Err(Error::Query(
                    "--pinyin matches case-insensitively; drop --case-sensitive".into(),
                ));
            }
            if glob::has_wildcards(self.needle.as_bytes()) {
                return Err(Error::Query(
                    "--pinyin cannot be combined with * or ?".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Result of [`search`]: matched ids in ascending id order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matches {
    pub ids: Vec<Id>,
    pub elapsed: Duration,
}

/// Below this many records, scan sequentially rather than spin up rayon.
pub(crate) const PAR_THRESHOLD: usize = 32 * 1024;

/// Run a query, returning every matching record id (the root is never a result).
pub fn search(index: &Index, q: &Query) -> Result<Matches> {
    q.validate()?;
    let start = Instant::now();
    let ids = match_ids(index, q);
    Ok(Matches {
        ids,
        elapsed: start.elapsed(),
    })
}

/// The match itself, once the query is known to be answerable.
fn match_ids(index: &Index, q: &Query) -> Vec<Id> {
    let needle: Vec<u8> = if q.case_sensitive {
        q.needle.as_bytes().to_vec()
    } else {
        fold::fold(q.needle.as_bytes())
    };
    let pred = |id: Id| passes_filters(index, q, id);
    let n = index.len() as Id;

    if needle.is_empty() {
        return filter_all(n, &pred);
    }
    #[cfg(feature = "pinyin")]
    if q.pinyin {
        return crate::pinyin::search(index, q);
    }
    if glob::has_wildcards(&needle) {
        // A whole-name glob. `exact` is implied and ignored. A literal run of
        // two or more bytes in the pattern must appear verbatim in any match,
        // so it is a sound prefilter; without one, every record is tested.
        let literal = glob::longest_literal_run(&needle);
        if literal.len() >= 2 {
            let view = if q.case_sensitive {
                index.name_view()
            } else {
                index.fold_view()
            };
            let pat = needle.clone();
            matcher::scan_pool(&view, literal, move |id| {
                let hay = if q.case_sensitive {
                    index.name(id)
                } else {
                    index.folded(id)
                };
                glob::glob_match(&pat, hay) && pred(id)
            })
        } else {
            let pat = needle.clone();
            let f = move |id: Id| {
                let hay = if q.case_sensitive {
                    index.name(id)
                } else {
                    index.folded(id)
                };
                glob::glob_match(&pat, hay) && pred(id)
            };
            filter_all(n, &f)
        }
    } else if q.exact {
        let hay = |id: Id| {
            if q.case_sensitive {
                index.name(id)
            } else {
                index.folded(id)
            }
        };
        let f = |id: Id| {
            let h = hay(id);
            h.len() == needle.len() && h == needle.as_slice() && pred(id)
        };
        filter_all(n, &f)
    } else {
        let view = if q.case_sensitive {
            index.name_view()
        } else {
            index.fold_view()
        };
        matcher::scan_pool(&view, &needle, pred)
    }
}

fn filter_all<F: Fn(Id) -> bool + Sync>(n: Id, f: &F) -> Vec<Id> {
    if (n as usize) < PAR_THRESHOLD {
        (1..n).filter(|&id| f(id)).collect()
    } else {
        (1..n).into_par_iter().filter(|&id| f(id)).collect()
    }
}

/// Every filter except the name match, cheapest first.
#[inline]
pub(crate) fn passes_filters(index: &Index, q: &Query, id: Id) -> bool {
    let f = index.flags(id);
    if f & flags::TOMBSTONE != 0 {
        return false;
    }
    if !q.include_hidden && f & flags::HIDDEN != 0 {
        return false;
    }
    let is_dir = f & flags::DIR != 0;
    match q.kind {
        KindFilter::FilesOnly if is_dir => return false,
        KindFilter::DirsOnly if !is_dir => return false,
        _ => {}
    }
    if q.min_size.is_some() || q.max_size.is_some() {
        if f & flags::HAS_SIZE == 0 {
            return false;
        }
        let s = index.size(id);
        if q.min_size.is_some_and(|m| s < m) || q.max_size.is_some_and(|m| s > m) {
            return false;
        }
    }
    if q.after.is_some() || q.before.is_some() {
        if f & flags::HAS_MTIME == 0 {
            return false;
        }
        let t = index.mtime(id);
        if q.after.is_some_and(|a| t < a) || q.before.is_some_and(|b| t > b) {
            return false;
        }
    }
    true
}

/// Keep only the `k` ids that would come first under `key`/`desc`, in
/// arbitrary order; follow with [`sort_ids`] to order them. O(n) selection,
/// so paging through a huge result set never sorts all of it.
pub fn truncate_top(index: &Index, ids: &mut Vec<Id>, key: SortKey, desc: bool, k: usize) {
    if k == 0 || ids.len() <= k {
        return;
    }
    match key {
        SortKey::Path => {
            // Path order needs every path anyway; a full sort is the honest cost.
            sort_ids(index, ids, key, desc);
            ids.truncate(k);
        }
        SortKey::Name => {
            let mut keyed = name_keyed(index, ids);
            keyed.select_nth_unstable_by(k - 1, |a, b| name_keyed_cmp(index, desc, a, b));
            keyed.truncate(k);
            ids.clear();
            ids.extend(keyed.into_iter().map(|(_, id)| id));
        }
        SortKey::Size | SortKey::Mtime => {
            let cmp = |a: &Id, b: &Id| {
                let o = compare(index, key, *a, *b);
                if desc { o.reverse() } else { o }
            };
            ids.select_nth_unstable_by(k - 1, cmp);
            ids.truncate(k);
        }
    }
}

/// The first eight folded-name bytes as a big-endian integer: byte-wise
/// lexicographic order on names is integer order on these keys, and names
/// never contain `0x00`, so zero padding sorts a shorter name first exactly
/// as the full comparison would. Comparing integers instead of two random
/// pool reads per step is what keeps ordering millions of hits cheap; the
/// full name is consulted only when prefixes tie.
#[inline]
fn name_prefix(index: &Index, id: Id) -> u64 {
    let n = index.folded(id);
    let mut b = [0u8; 8];
    let l = n.len().min(8);
    b[..l].copy_from_slice(&n[..l]);
    u64::from_be_bytes(b)
}

fn name_keyed(index: &Index, ids: &[Id]) -> Vec<(u64, Id)> {
    if ids.len() < PAR_THRESHOLD {
        ids.iter().map(|&id| (name_prefix(index, id), id)).collect()
    } else {
        ids.par_iter()
            .map(|&id| (name_prefix(index, id), id))
            .collect()
    }
}

#[inline]
fn name_keyed_cmp(index: &Index, desc: bool, a: &(u64, Id), b: &(u64, Id)) -> std::cmp::Ordering {
    let o =
        a.0.cmp(&b.0)
            .then_with(|| compare(index, SortKey::Name, a.1, b.1));
    if desc { o.reverse() } else { o }
}

fn compare(index: &Index, key: SortKey, a: Id, b: Id) -> std::cmp::Ordering {
    match key {
        SortKey::Name => index
            .folded(a)
            .cmp(index.folded(b))
            .then_with(|| index.name(a).cmp(index.name(b)))
            .then(a.cmp(&b)),
        SortKey::Size => (index.size(a), a).cmp(&(index.size(b), b)),
        SortKey::Mtime => (index.mtime(a), a).cmp(&(index.mtime(b), b)),
        SortKey::Path => unreachable!("path comparison is handled with precomputed keys"),
    }
}

/// Sort matched ids in place.
pub fn sort_ids(index: &Index, ids: &mut [Id], key: SortKey, desc: bool) {
    match key {
        SortKey::Name => {
            let mut keyed = name_keyed(index, ids);
            if keyed.len() < PAR_THRESHOLD {
                keyed.sort_unstable_by(|a, b| name_keyed_cmp(index, false, a, b));
            } else {
                keyed.par_sort_unstable_by(|a, b| name_keyed_cmp(index, false, a, b));
            }
            for (slot, (_, id)) in ids.iter_mut().zip(keyed) {
                *slot = id;
            }
        }
        SortKey::Size | SortKey::Mtime => {
            if ids.len() < PAR_THRESHOLD {
                ids.sort_unstable_by(|&a, &b| compare(index, key, a, b));
            } else {
                ids.par_sort_unstable_by(|&a, &b| compare(index, key, a, b));
            }
        }
        SortKey::Path => {
            let mut keyed: Vec<(Vec<u8>, Id)> = ids
                .par_iter()
                .map(|&a| {
                    let mut v = Vec::with_capacity(128);
                    index.resolve_path_into(a, &mut v);
                    (v, a)
                })
                .collect();
            keyed.par_sort_unstable();
            for (slot, (_, id)) in ids.iter_mut().zip(keyed) {
                *slot = id;
            }
        }
    }
    if desc {
        ids.reverse();
    }
}

/// The `[offset, offset+limit)` window of `ids`; `limit == 0` means all.
pub fn page(ids: &[Id], offset: usize, limit: usize) -> &[Id] {
    let s = offset.min(ids.len());
    let e = if limit == 0 {
        ids.len()
    } else {
        s.saturating_add(limit).min(ids.len())
    };
    &ids[s..e]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::VolumeConfig;
    use crate::scan::{ScanOptions, build_index};
    use crate::testing::SyntheticScanner;
    use std::path::PathBuf;

    fn fixture() -> Index {
        let s = SyntheticScanner::from_paths(
            "/syn",
            &[
                "src/main.rs",
                "src/lib.rs",
                "src/Util.RS",
                "docs/README.md",
                "docs/notes/",
                "Cargo.toml",
                "target/debug/app",
                "日本語/ファイル.txt",
            ],
        );
        build_index(&s, s.root(), VolumeConfig::unix(), &ScanOptions::default())
            .unwrap()
            .0
    }

    fn names(idx: &Index, ids: &[Id]) -> Vec<String> {
        ids.iter()
            .map(|&i| String::from_utf8_lossy(idx.name(i)).into_owned())
            .collect()
    }

    #[test]
    fn substring_is_case_insensitive_by_default() {
        let idx = fixture();
        let mut m = search(&idx, &Query::new(".rs")).unwrap();
        sort_ids(&idx, &mut m.ids, SortKey::Name, false);
        assert_eq!(names(&idx, &m.ids), ["lib.rs", "main.rs", "Util.RS"]);
    }

    #[test]
    fn case_sensitive_respects_case() {
        let idx = fixture();
        let m = search(
            &idx,
            &Query {
                needle: ".RS".into(),
                case_sensitive: true,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(names(&idx, &m.ids), ["Util.RS"]);
    }

    #[test]
    fn empty_needle_matches_everything_but_root() {
        let idx = fixture();
        let m = search(&idx, &Query::new("")).unwrap();
        assert_eq!(m.ids.len(), idx.len() - 1);
        assert!(!m.ids.contains(&0));
    }

    #[test]
    fn kind_filters() {
        let idx = fixture();
        let dirs = search(
            &idx,
            &Query {
                kind: KindFilter::DirsOnly,
                ..Query::default()
            },
        )
        .unwrap();
        let mut d = names(&idx, &dirs.ids);
        d.sort();
        assert_eq!(d, ["debug", "docs", "notes", "src", "target", "日本語"]);
        let files = search(
            &idx,
            &Query {
                kind: KindFilter::FilesOnly,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(files.ids.len(), 7);
    }

    #[test]
    fn exact_matches_whole_name() {
        let idx = fixture();
        let m = search(
            &idx,
            &Query {
                needle: "lib.rs".into(),
                exact: true,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(names(&idx, &m.ids), ["lib.rs"]);
        let none = search(
            &idx,
            &Query {
                needle: "lib".into(),
                exact: true,
                ..Query::default()
            },
        )
        .unwrap();
        assert!(none.ids.is_empty());
    }

    #[test]
    fn non_ascii_search_folds() {
        let idx = fixture();
        let m = search(&idx, &Query::new("ファイル")).unwrap();
        assert_eq!(names(&idx, &m.ids), ["ファイル.txt"]);
    }

    #[test]
    fn size_filter_uses_has_size() {
        let idx = fixture();
        // from_paths assigns size (i+1)*100 to files in order.
        let m = search(
            &idx,
            &Query {
                min_size: Some(500),
                max_size: Some(700),
                ..Query::default()
            },
        )
        .unwrap();
        let mut got = names(&idx, &m.ids);
        got.sort();
        // files are: main.rs(100) lib.rs(200) Util.RS(300) README.md(400)
        // Cargo.toml(600) app(700) ファイル.txt(800)  -> 600, 700
        assert_eq!(got, ["Cargo.toml", "app"]);
    }

    #[test]
    fn glob_queries_match_whole_names() {
        let idx = fixture();
        let mut m = search(&idx, &Query::new("*.rs")).unwrap();
        sort_ids(&idx, &mut m.ids, SortKey::Name, false);
        assert_eq!(names(&idx, &m.ids), ["lib.rs", "main.rs", "Util.RS"]);

        // `?` is one character, not one byte.
        let m = search(&idx, &Query::new("????.txt")).unwrap();
        assert_eq!(names(&idx, &m.ids), ["ファイル.txt"]);
        assert!(search(&idx, &Query::new("?.txt")).unwrap().ids.is_empty());

        // Only wildcards: everything but the root.
        let all = search(&idx, &Query::new("*")).unwrap();
        assert_eq!(all.ids.len(), idx.len() - 1);
        assert!(search(&idx, &Query::new("*.zzz")).unwrap().ids.is_empty());

        // Literal run too short for the prefilter: slow path, same answers.
        let m = search(&idx, &Query::new("*a*a*")).unwrap();
        let mut expect: Vec<String> = (1..idx.len() as Id)
            .filter(|&i| idx.folded(i).iter().filter(|&&b| b == b'a').count() >= 2)
            .map(|i| String::from_utf8_lossy(idx.name(i)).into_owned())
            .collect();
        expect.sort();
        let mut got = names(&idx, &m.ids);
        got.sort();
        assert_eq!(got, expect);
    }

    #[test]
    fn glob_respects_case_sensitivity() {
        let idx = fixture();
        let m = search(
            &idx,
            &Query {
                needle: "*.RS".into(),
                case_sensitive: true,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(names(&idx, &m.ids), ["Util.RS"]);
        let mut m = search(&idx, &Query::new("*.RS")).unwrap();
        sort_ids(&idx, &mut m.ids, SortKey::Name, false);
        assert_eq!(names(&idx, &m.ids), ["lib.rs", "main.rs", "Util.RS"]);
    }

    #[test]
    fn sort_by_path_and_paging() {
        let idx = fixture();
        let mut m = search(&idx, &Query::new("")).unwrap();
        sort_ids(&idx, &mut m.ids, SortKey::Path, false);
        let paths: Vec<PathBuf> = m.ids.iter().map(|&i| idx.resolve_path(i)).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        assert_eq!(page(&m.ids, 0, 3).len(), 3);
        assert_eq!(page(&m.ids, 2, 3), &m.ids[2..5]);
        assert_eq!(page(&m.ids, 100, 3).len(), 0);
        assert_eq!(page(&m.ids, 0, 0).len(), m.ids.len());
    }

    #[test]
    fn name_order_with_shared_prefixes_matches_full_comparison() {
        use crate::platform::VolumeConfig;
        let mut idx = Index::new(VolumeConfig::unix(), PathBuf::from("/r"), None);
        let names = [
            "prefix_shared_zzz.txt",
            "prefix_shared_aaa.txt",
            "prefix_s",
            "prefix_shared",
            "PREFIX_SHARED_mmm.txt",
            "b",
            "a",
            "ab",
            "prefix_shared_aaa.txt~",
        ];
        for n in names {
            idx.push_record(n.as_bytes(), 0, 0, 0, 0).unwrap();
        }
        let mut ids: Vec<Id> = (1..idx.len() as Id).collect();
        sort_ids(&idx, &mut ids, SortKey::Name, false);
        let got = names_of(&idx, &ids);
        let mut want: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        want.sort_by(|a, b| {
            a.to_lowercase()
                .as_bytes()
                .cmp(b.to_lowercase().as_bytes())
                .then_with(|| a.as_bytes().cmp(b.as_bytes()))
        });
        assert_eq!(got, want);
        // top-k agrees with the full sort for several k, both directions
        for k in [1, 2, 4, 8] {
            for desc in [false, true] {
                let mut full: Vec<Id> = (1..idx.len() as Id).collect();
                sort_ids(&idx, &mut full, SortKey::Name, desc);
                let mut top: Vec<Id> = (1..idx.len() as Id).collect();
                truncate_top(&idx, &mut top, SortKey::Name, desc, k);
                sort_ids(&idx, &mut top, SortKey::Name, desc);
                assert_eq!(top, full[..k], "k={k} desc={desc}");
            }
        }
    }

    fn names_of(idx: &Index, ids: &[Id]) -> Vec<String> {
        ids.iter()
            .map(|&i| String::from_utf8_lossy(idx.name(i)).into_owned())
            .collect()
    }

    #[test]
    fn truncate_top_keeps_the_right_k() {
        let idx = fixture();
        let m = search(
            &idx,
            &Query {
                kind: KindFilter::FilesOnly,
                ..Query::default()
            },
        )
        .unwrap();
        let mut full = m.ids.clone();
        sort_ids(&idx, &mut full, SortKey::Size, true);
        let mut top = m.ids.clone();
        truncate_top(&idx, &mut top, SortKey::Size, true, 3);
        sort_ids(&idx, &mut top, SortKey::Size, true);
        assert_eq!(top, full[..3]);
        let mut all = m.ids.clone();
        truncate_top(&idx, &mut all, SortKey::Name, false, 100);
        assert_eq!(all.len(), m.ids.len());
    }

    #[test]
    fn sort_by_size_desc() {
        let idx = fixture();
        let mut m = search(
            &idx,
            &Query {
                kind: KindFilter::FilesOnly,
                ..Query::default()
            },
        )
        .unwrap();
        sort_ids(&idx, &mut m.ids, SortKey::Size, true);
        let sizes: Vec<u64> = m.ids.iter().map(|&i| idx.size(i)).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
    }
}
