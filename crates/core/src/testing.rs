//! In-memory test doubles, available to downstream crates via the `testing`
//! feature (re-exported by `everyfiles-testkit`).
//!
//! Living inside the core rather than in the testkit avoids a dev-dependency
//! cycle: a unit-test build of this crate is a distinct crate instance from
//! the one a separate testkit would link, so a trait defined here would not
//! match its own impl.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::platform::NameEncoding;
use crate::scan::{
    EntryAttrs, EntryStat, Kind, ProbeReport, RawEntry, RootInfo, ScanCaps, ScanMode, Scanner,
};
use crate::{Error, Result};

/// An in-memory directory tree that implements [`Scanner`].
///
/// Paths handed to [`SyntheticScanner::add_file`] / [`SyntheticScanner::add_dir`]
/// are relative to the synthetic root, use `/` as separator, and have their
/// intermediate directories created implicitly.
#[derive(Debug, Clone)]
pub struct SyntheticScanner {
    root: PathBuf,
    dirs: HashMap<PathBuf, Vec<RawEntry>>,
    root_mtime: i64,
}

impl SyntheticScanner {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        // Same form `build_index` compares against; see `GeneratedScanner::new`.
        let root = root.into();
        let root = std::path::absolute(&root).unwrap_or(root);
        let mut dirs = HashMap::new();
        dirs.insert(root.clone(), Vec::new());
        Self {
            root,
            dirs,
            root_mtime: 0,
        }
    }

    /// Build a tree from a list of relative paths. A trailing `/` marks a
    /// directory; everything else is a file with size and mtime derived from
    /// its position so tests can assert on them deterministically.
    pub fn from_paths(root: impl Into<PathBuf>, paths: &[&str]) -> Self {
        let mut s = Self::new(root);
        for (i, p) in paths.iter().enumerate() {
            if let Some(dir) = p.strip_suffix('/') {
                s.add_dir(dir);
            } else {
                s.add_file(
                    p,
                    (i as u64 + 1) * 100,
                    1_700_000_000_000_000_000 + i as i64,
                );
            }
        }
        s
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn add_dir(&mut self, rel: &str) {
        let full = self.ensure_parents(rel);
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let parent = full.parent().expect("has parent").to_path_buf();
        self.push_unique(
            &parent,
            RawEntry {
                name: name.as_bytes().into(),
                kind: Kind::Dir,
                size: Some(0),
                mtime: Some(0),
                native_id: None,
                native_parent: None,
                attrs: EntryAttrs::default(),
            },
        );
        self.dirs.entry(full).or_default();
    }

    pub fn add_file(&mut self, rel: &str, size: u64, mtime: i64) {
        let full = self.ensure_parents(rel);
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let parent = full.parent().expect("has parent").to_path_buf();
        self.push_unique(
            &parent,
            RawEntry {
                name: name.as_bytes().into(),
                kind: Kind::File,
                size: Some(size),
                mtime: Some(mtime),
                native_id: None,
                native_parent: None,
                attrs: EntryAttrs::default(),
            },
        );
    }

    /// Number of entries (files + dirs), not counting the root.
    pub fn entry_count(&self) -> usize {
        self.dirs.values().map(Vec::len).sum()
    }

    /// Drop the listing for `rel` while keeping its entry in the parent, so
    /// `read_dir` on it fails. Simulates a directory the process may not read.
    pub fn make_unreadable(&mut self, rel: &str) {
        let full = self.root.join(rel.trim_matches('/'));
        self.dirs.remove(&full);
    }

    /// Delete a file or a whole directory subtree.
    pub fn remove(&mut self, rel: &str) {
        let full = self.root.join(rel.trim_matches('/'));
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return;
        };
        let name = name.as_encoded_bytes();
        if let Some(v) = self.dirs.get_mut(parent) {
            v.retain(|e| e.name.as_slice() != name);
        }
        let prefix = full.clone();
        self.dirs
            .retain(|k, _| k != &prefix && !k.starts_with(&prefix));
    }

    /// Change a file's size and mtime.
    pub fn set_meta(&mut self, rel: &str, size: u64, mtime: i64) {
        let full = self.root.join(rel);
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return;
        };
        let name = name.as_encoded_bytes();
        if let Some(v) = self.dirs.get_mut(parent)
            && let Some(e) = v.iter_mut().find(|e| e.name.as_slice() == name)
        {
            e.size = Some(size);
            e.mtime = Some(mtime);
        }
    }

    /// Advance a directory's mtime (as the OS would when its entries change).
    /// `""` is the root.
    pub fn bump_dir_mtime(&mut self, rel: &str) {
        let full = if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel.trim_matches('/'))
        };
        if full == self.root {
            self.root_mtime += 1;
            return;
        }
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return;
        };
        let name = name.as_encoded_bytes();
        if let Some(v) = self.dirs.get_mut(parent)
            && let Some(e) = v.iter_mut().find(|e| e.name.as_slice() == name)
        {
            e.mtime = Some(e.mtime.unwrap_or(0) + 1);
        }
    }

    fn entry_at(&self, path: &Path) -> Option<&RawEntry> {
        let parent = path.parent()?;
        let name = path.file_name()?.as_encoded_bytes();
        self.dirs
            .get(parent)?
            .iter()
            .find(|e| e.name.as_slice() == name)
    }

    fn ensure_parents(&mut self, rel: &str) -> PathBuf {
        let mut cur = self.root.clone();
        let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        for comp in &comps[..comps.len().saturating_sub(1)] {
            let next = cur.join(comp);
            if !self.dirs.contains_key(&next) {
                self.push_unique(
                    &cur,
                    RawEntry {
                        name: comp.as_bytes().into(),
                        kind: Kind::Dir,
                        size: Some(0),
                        mtime: Some(0),
                        native_id: None,
                        native_parent: None,
                        attrs: EntryAttrs::default(),
                    },
                );
                self.dirs.insert(next.clone(), Vec::new());
            }
            cur = next;
        }
        cur.join(comps.last().copied().unwrap_or(""))
    }

    fn push_unique(&mut self, dir: &Path, e: RawEntry) {
        let v = self.dirs.entry(dir.to_path_buf()).or_default();
        if !v.iter().any(|x| x.name == e.name) {
            v.push(e);
        }
    }
}

impl Scanner for SyntheticScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: true,
            provides_mtime: true,
            provides_native_ids: false,
            parallel_safe: true,
            requires_privilege: false,
            encoding: NameEncoding::Bytes,
        }
    }

    fn probe(&self, _root: &Path) -> Result<ProbeReport> {
        Ok(ProbeReport {
            granted: true,
            active: "synthetic",
            reason: None,
            kernel_queue_bytes: None,
        })
    }

    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>> {
        if path == self.root {
            return Ok(Some(EntryStat {
                kind: Kind::Dir,
                size: None,
                mtime: Some(self.root_mtime),
            }));
        }
        match self.entry_at(path) {
            Some(e) => Ok(Some(EntryStat {
                kind: e.kind,
                size: e.size,
                mtime: e.mtime,
            })),
            None => Err(Error::io(
                path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "synthetic entry not found"),
            )),
        }
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        if self.dirs.contains_key(root) {
            Ok(RootInfo {
                mtime: Some(self.root_mtime),
            })
        } else {
            Err(Error::io(
                root,
                std::io::Error::new(std::io::ErrorKind::NotFound, "synthetic root not found"),
            ))
        }
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        match self.dirs.get(dir) {
            Some(v) => {
                out.extend(v.iter().cloned());
                Ok(())
            }
            None => Err(Error::io(
                dir,
                std::io::Error::new(std::io::ErrorKind::NotFound, "synthetic dir not found"),
            )),
        }
    }
}

/// A procedurally generated tree of arbitrary size that costs no memory to
/// hold: every directory's children are derived deterministically from its
/// path. Used by `everyfiles bench` and the criterion benchmarks.
#[derive(Debug, Clone)]
pub struct GeneratedScanner {
    root: PathBuf,
    depth: u32,
    fanout: u32,
    files_per_dir: u32,
}

const WORDS: &[&str] = &[
    "report",
    "final",
    "draft",
    "notes",
    "main",
    "lib",
    "mod",
    "test",
    "index",
    "config",
    "invoice",
    "photo",
    "img",
    "screenshot",
    "backup",
    "archive",
    "data",
    "export",
    "import",
    "readme",
    "license",
    "build",
    "release",
    "debug",
    "cache",
    "session",
    "user",
    "admin",
    "project",
    "spec",
    "design",
    "sketch",
    "render",
    "video",
    "audio",
    "track",
    "album",
    "budget",
    "plan",
    "summary",
    "meeting",
    "agenda",
    "resume",
    "letter",
    "contract",
    "tax",
    "Report",
    "Final",
    "IMG",
    "DSC",
    "Screenshot",
    "Untitled",
    "New",
    "Copy",
    "Old",
    "Temp",
];
const EXTS: &[&str] = &[
    "rs", "txt", "md", "pdf", "jpg", "JPG", "png", "mp4", "mp3", "csv", "json", "toml", "html",
    "css", "js", "ts", "py", "zip", "tar.gz", "docx", "xlsx", "log", "o", "d", "lock",
];

impl GeneratedScanner {
    pub fn new(root: impl Into<PathBuf>, depth: u32, fanout: u32, files_per_dir: u32) -> Self {
        let root = root.into();
        // `build_index` and the engine call `std::path::absolute` on the scan
        // root before they reach `root_info`, so the scanner has to store that
        // same form or the two paths never compare equal. `absolute` is
        // lexical only, so this keeps core free of filesystem access.
        let root = std::path::absolute(&root).unwrap_or(root);
        Self {
            root,
            depth,
            fanout,
            files_per_dir,
        }
    }

    /// Pick a shape whose total entry count is close to `target`.
    pub fn with_approx_total(root: impl Into<PathBuf>, target: u64) -> Self {
        let (depth, fanout) = (4u32, 8u32);
        let dirs = Self::dir_count(depth, fanout);
        let files_per_dir = (target.saturating_sub(dirs) / dirs).clamp(1, 4096) as u32;
        Self::new(root, depth, fanout, files_per_dir)
    }

    fn dir_count(depth: u32, fanout: u32) -> u64 {
        (0..=depth).map(|d| (fanout as u64).pow(d)).sum()
    }

    pub fn expected_dirs(&self) -> u64 {
        Self::dir_count(self.depth, self.fanout)
    }

    /// Expected records including the root.
    pub fn expected_total(&self) -> u64 {
        let dirs = self.expected_dirs();
        dirs + dirs * self.files_per_dir as u64
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn depth_of(&self, dir: &Path) -> Option<u32> {
        let rel = dir.strip_prefix(&self.root).ok()?;
        Some(rel.components().count() as u32)
    }

    fn seed_of(dir: &Path) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in crate::scan::name_bytes(dir.as_os_str()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h | 1
    }

    fn file_name(mut s: u64, i: u32) -> String {
        s ^= (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let w1 = WORDS[(next() % WORDS.len() as u64) as usize];
        let w2 = WORDS[(next() % WORDS.len() as u64) as usize];
        let ext = EXTS[(next() % EXTS.len() as u64) as usize];
        match next() % 5 {
            0 => format!("{w1}_{w2}.{ext}"),
            1 => format!("{w1}-{w2}-{}.{ext}", next() % 1000),
            2 => format!("{w1}{}.{ext}", next() % 10_000),
            3 => format!("{w1} {w2} ({}).{ext}", next() % 20),
            _ => format!("{w1}.{ext}"),
        }
    }
}

impl Scanner for GeneratedScanner {
    fn capabilities(&self) -> ScanCaps {
        ScanCaps {
            mode: ScanMode::PerDirectory,
            provides_size: true,
            provides_mtime: true,
            provides_native_ids: false,
            parallel_safe: true,
            requires_privilege: false,
            encoding: NameEncoding::Bytes,
        }
    }

    fn probe(&self, _root: &Path) -> Result<ProbeReport> {
        Ok(ProbeReport {
            granted: true,
            active: "generated",
            reason: None,
            kernel_queue_bytes: None,
        })
    }

    fn root_info(&self, root: &Path) -> Result<RootInfo> {
        if root == self.root {
            Ok(RootInfo { mtime: Some(0) })
        } else {
            Err(Error::io(
                root,
                std::io::Error::new(std::io::ErrorKind::NotFound, "generated root mismatch"),
            ))
        }
    }

    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()> {
        let Some(depth) = self.depth_of(dir) else {
            return Err(Error::io(
                dir,
                std::io::Error::new(std::io::ErrorKind::NotFound, "outside generated root"),
            ));
        };
        let seed = Self::seed_of(dir);
        if depth < self.depth {
            for k in 0..self.fanout {
                out.push(RawEntry {
                    name: format!("d{k:02}").into_bytes().into(),
                    kind: Kind::Dir,
                    size: Some(0),
                    mtime: Some(0),
                    native_id: None,
                    native_parent: None,
                    attrs: EntryAttrs::default(),
                });
            }
        }
        for i in 0..self.files_per_dir {
            let name = Self::file_name(seed, i);
            let sz = seed.rotate_left(i % 63) % 50_000_000;
            out.push(RawEntry {
                name: name.into_bytes().into(),
                kind: Kind::File,
                size: Some(sz),
                mtime: Some(1_600_000_000_000_000_000 + (sz as i64) * 1_000),
                native_id: None,
                native_parent: None,
                attrs: EntryAttrs::default(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod generated_tests {
    use super::*;
    use crate::platform::VolumeConfig;
    use crate::scan::{ScanOptions, build_index};

    #[test]
    fn generated_tree_has_the_expected_shape() {
        let g = GeneratedScanner::new("/gen", 2, 3, 5);
        assert_eq!(g.expected_dirs(), 1 + 3 + 9);
        assert_eq!(g.expected_total(), 13 + 13 * 5);
        let (idx, stats) =
            build_index(&g, g.root(), VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        assert_eq!(idx.len() as u64, g.expected_total());
        assert_eq!(stats.dirs, 13);
        assert_eq!(stats.files, 65);
    }

    /// The scan and engine layers absolutize the root before `root_info`; a
    /// scanner keeping the raw `"/bench"` failed on Windows, where
    /// `std::path::absolute("/bench")` is `D:\bench`.
    #[test]
    fn generated_root_matches_the_absolutized_scan_root() {
        let g = GeneratedScanner::new("/gen", 1, 2, 3);
        let abs = std::path::absolute("/gen").unwrap();
        assert_eq!(g.root(), abs);
        build_index(&g, &abs, VolumeConfig::unix(), &ScanOptions::default()).unwrap();
    }

    #[test]
    fn generated_names_are_deterministic() {
        let g = GeneratedScanner::new("/gen", 1, 2, 4);
        let dir = g.root().join("d01");
        let mut a = Vec::new();
        let mut b = Vec::new();
        g.read_dir(&dir, &mut a).unwrap();
        g.read_dir(&dir, &mut b).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
    }
}
