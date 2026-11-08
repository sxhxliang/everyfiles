pub mod apps;
pub mod dev;
pub mod languages;
pub mod large_files;
pub mod launch_agents;
pub mod leftovers;
pub mod system;
pub mod web;

use std::fs;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::domain::{CleanCategory, CleanGroup, ScanSummary};

#[cfg(unix)]
fn file_allocated_size(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let allocated = meta.blocks().saturating_mul(512);
    if allocated > 0 {
        std::cmp::min(meta.len(), allocated)
    } else {
        meta.len()
    }
}

#[cfg(not(unix))]
fn file_allocated_size(meta: &fs::Metadata) -> u64 {
    meta.len()
}

/// Calculate size and file count of a file or directory tree.
pub fn dir_size_and_count(path: &Path) -> (u64, usize) {
    if !path.exists() {
        return (0, 0);
    }

    if let Ok(meta) = fs::metadata(path) {
        if !meta.is_dir() {
            return (file_allocated_size(&meta), 1);
        }
    } else {
        return (0, 0);
    }

    let mut total_size = 0u64;
    let mut file_count = 0usize;

    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total_size = total_size.saturating_add(file_allocated_size(&meta));
                file_count = file_count.saturating_add(1);
            }
        }
    }

    (total_size, file_count)
}

/// Calculate size of a file or directory tree in bytes.
pub fn dir_size(path: &Path) -> u64 {
    dir_size_and_count(path).0
}

/// Comprehensive scanner coordinator.
pub struct CleanupCoordinator;

impl CleanupCoordinator {
    /// Run full system scan across all categories in parallel.
    pub fn scan_all() -> ScanSummary {
        let start = Instant::now();

        // Run category scanners in parallel
        let categories = CleanCategory::all();
        let groups: Vec<CleanGroup> = categories
            .par_iter()
            .map(|cat| Self::scan_category(*cat))
            .collect();

        let elapsed = start.elapsed();
        ScanSummary::new(groups, elapsed)
    }

    /// Scan a single category.
    pub fn scan_category(category: CleanCategory) -> CleanGroup {
        let items = match category {
            CleanCategory::SystemJunk => {
                let mut list = system::scan_system_junk();
                list.extend(languages::scan_unneeded_languages());
                list.extend(launch_agents::scan_broken_login_items());
                list
            }
            CleanCategory::AppJunk => {
                let mut list = apps::scan_app_junk();
                list.extend(leftovers::scan_leftovers());
                list
            }
            CleanCategory::WebJunk => web::scan_web_junk(),
            CleanCategory::DevJunk => dev::scan_dev_junk(),
            CleanCategory::LargeOldFiles => large_files::scan_large_old_files(),
            CleanCategory::Trash => system::scan_trash_bins(),
        };

        CleanGroup::new(category, items)
    }
}
