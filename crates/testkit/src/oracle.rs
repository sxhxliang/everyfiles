//! The naive reference scan every backend is compared against.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use everyfiles_core::scan::{Kind, system_time_to_ns};

/// What the oracle knows about one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleEntry {
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
}

/// Walk `root` with `walkdir` (not following symlinks) and `symlink_metadata`,
/// returning every entry below the root keyed by full path. The root itself is
/// excluded, matching what the index reports as searchable records.
///
/// Entries that cannot be read are skipped, not fatal: a system tree like
/// `/usr` contains directories the current user may not list, and the indexer
/// skips them too, so both sides must skip for the comparison to be meaningful.
pub fn naive_scan(root: &Path) -> std::io::Result<BTreeMap<PathBuf, OracleEntry>> {
    Ok(naive_scan_with_skips(root)?.0)
}

/// As [`naive_scan`], also returning how many entries were skipped.
pub fn naive_scan_with_skips(
    root: &Path,
) -> std::io::Result<(BTreeMap<PathBuf, OracleEntry>, usize)> {
    let mut out = BTreeMap::new();
    let mut skipped = 0usize;
    for entry in walkdir::WalkDir::new(root).follow_links(false).min_depth(1) {
        let Ok(entry) = entry else {
            // An unreadable directory; the entry itself was already yielded.
            skipped += 1;
            continue;
        };
        let Ok(md) = std::fs::symlink_metadata(entry.path()) else {
            skipped += 1;
            continue;
        };
        let ft = md.file_type();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else if ft.is_symlink() {
            Kind::Symlink
        } else {
            Kind::Other
        };
        out.insert(
            entry.path().to_path_buf(),
            OracleEntry {
                kind,
                size: md.len(),
                mtime: md.modified().map(system_time_to_ns).unwrap_or(0),
            },
        );
    }
    Ok((out, skipped))
}
