use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use xxhash_rust::xxh3::xxh3_64;

use crate::deleter::SafeDeleter;
use crate::guard::SafetyGuard;

const PARTIAL_READ_BYTES: usize = 16 * 1024; // 16 KB

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DuplicateFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified: Option<SystemTime>,
    pub is_original: bool,
    pub selected_for_removal: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub file_size: u64,
    pub hash: u64,
    pub files: Vec<DuplicateFile>,
    pub wasted_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub total_duplicate_files: usize,
    pub total_wasted_bytes: u64,
    pub scan_duration: Duration,
}

pub struct DuplicateFinder;

impl DuplicateFinder {
    /// Scan target directories for duplicate files using multi-tier size + xxHash3 comparison.
    pub fn find(roots: &[PathBuf], min_size: u64, max_depth: Option<usize>) -> DuplicateReport {
        let start = Instant::now();

        // 1. Collect all files grouped by exact byte size
        let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();

        for root in roots {
            if !root.exists() {
                continue;
            }
            let mut walker = WalkDir::new(root);
            if let Some(depth) = max_depth {
                walker = walker.max_depth(depth);
            }

            for entry in walker.into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let path = entry.into_path();
                    if let Ok(meta) = path.metadata() {
                        let size = meta.len();
                        if size >= min_size && SafetyGuard::is_safe(&path) {
                            size_map.entry(size).or_default().push(path);
                        }
                    }
                }
            }
        }

        // Filter: only keep sizes with 2 or more files
        let candidates: Vec<(u64, Vec<PathBuf>)> = size_map
            .into_iter()
            .filter(|(_, paths)| paths.len() >= 2)
            .collect();

        // 2. Parallel multi-tier hash comparison with xxHash3
        let matched_groups: Vec<DuplicateGroup> = candidates
            .into_par_iter()
            .flat_map(|(file_size, paths)| {
                // Tier 1: Partial hash (first 16KB)
                let mut partial_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
                for p in paths {
                    if let Some(hash) = hash_partial(&p) {
                        partial_map.entry(hash).or_default().push(p);
                    }
                }

                // Tier 2: Full hash for partial hash collisions
                let mut groups = Vec::new();
                for (_, part_paths) in partial_map {
                    if part_paths.len() < 2 {
                        continue;
                    }
                    if file_size <= PARTIAL_READ_BYTES as u64 {
                        // Entire file already covered by partial hash
                        groups.push(build_group(file_size, 0, part_paths));
                    } else {
                        let mut full_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
                        for p in part_paths {
                            if let Some(hash) = hash_full(&p) {
                                full_map.entry(hash).or_default().push(p);
                            }
                        }
                        for (full_hash, matching_paths) in full_map {
                            if matching_paths.len() >= 2 {
                                groups.push(build_group(file_size, full_hash, matching_paths));
                            }
                        }
                    }
                }
                groups
            })
            .collect();

        let mut report = DuplicateReport {
            groups: matched_groups,
            ..Default::default()
        };
        // Sort groups by wasted bytes descending
        report
            .groups
            .sort_by_key(|g| std::cmp::Reverse(g.wasted_bytes));

        report.total_duplicate_files = report
            .groups
            .iter()
            .map(|g| g.files.len().saturating_sub(1))
            .sum();
        report.total_wasted_bytes = report.groups.iter().map(|g| g.wasted_bytes).sum();
        report.scan_duration = start.elapsed();

        report
    }

    /// Clean duplicate files marked for removal.
    pub fn clean(groups: &[DuplicateGroup], dry_run: bool, permanent: bool) -> (u64, usize) {
        let mut reclaimed = 0u64;
        let mut count = 0usize;

        for g in groups {
            for f in &g.files {
                if f.selected_for_removal {
                    if let Ok(bytes) = SafeDeleter::clean_path(&f.path, dry_run, permanent) {
                        reclaimed += bytes;
                        count += 1;
                    }
                }
            }
        }

        (reclaimed, count)
    }
}

fn build_group(file_size: u64, hash: u64, paths: Vec<PathBuf>) -> DuplicateGroup {
    let mut files = Vec::new();
    for p in paths {
        let meta = p.metadata().ok();
        let modified = meta.and_then(|m| m.modified().ok());
        files.push(DuplicateFile {
            path: p,
            size_bytes: file_size,
            modified,
            is_original: false,
            selected_for_removal: false,
        });
    }

    // Sort by modification time ascending: oldest file is considered the original!
    files.sort_by(|a, b| match (a.modified, b.modified) {
        (Some(ta), Some(tb)) => ta.cmp(&tb),
        _ => a.path.cmp(&b.path),
    });

    // Mark oldest as original, all others as selected for removal
    for (i, f) in files.iter_mut().enumerate() {
        if i == 0 {
            f.is_original = true;
            f.selected_for_removal = false;
        } else {
            f.is_original = false;
            f.selected_for_removal = true;
        }
    }

    let wasted_bytes = file_size * (files.len().saturating_sub(1) as u64);

    DuplicateGroup {
        file_size,
        hash,
        files,
        wasted_bytes,
    }
}

/// Compute xxHash3 of the first 16KB of a file.
fn hash_partial(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; PARTIAL_READ_BYTES];
    let n = file.read(&mut buf).ok()?;
    Some(xxh3_64(&buf[..n]))
}

/// Compute xxHash3 of the entire file.
fn hash_full(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut hasher_input = Vec::new();
    file.read_to_end(&mut hasher_input).ok()?;
    Some(xxh3_64(&hasher_input))
}
