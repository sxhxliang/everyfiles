use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::domain::{CleanItem, CleanReport};
use crate::guard::SafetyGuard;

/// Clean execution engine providing safe macOS Trash operations and dry-run validation.
pub struct SafeDeleter;

impl SafeDeleter {
    /// Delete or trash an individual path.
    /// Returns reclaimed size in bytes on success.
    pub fn clean_path(path: &Path, dry_run: bool, permanent: bool) -> Result<u64, String> {
        if !path.exists() {
            return Ok(0);
        }

        // 1. Safety validation
        if let Err(reason) = SafetyGuard::validate(path) {
            return Err(format!("Blocked by safety guard: {reason} ({path:?})"));
        }

        // Calculate size before deletion
        let size = crate::scanners::dir_size(path);

        if dry_run {
            return Ok(size);
        }

        // 2. Perform deletion
        if permanent {
            if path.is_dir() {
                fs::remove_dir_all(path)
                    .map_err(|e| format!("Failed to delete directory {path:?}: {e}"))?;
            } else {
                fs::remove_file(path)
                    .map_err(|e| format!("Failed to delete file {path:?}: {e}"))?;
            }
        } else {
            // Move to macOS Trash (~/.Trash)
            trash::delete(path).map_err(|e| format!("Failed to move to Trash {path:?}: {e}"))?;
        }

        Ok(size)
    }

    /// Empty directory contents while keeping the directory itself.
    pub fn clean_dir_contents(dir: &Path, dry_run: bool, permanent: bool) -> Result<u64, String> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(0);
        }

        if let Err(reason) = SafetyGuard::validate(dir) {
            return Err(format!("Blocked by safety guard: {reason} ({dir:?})"));
        }

        let mut reclaimed = 0;
        let entries = fs::read_dir(dir).map_err(|e| format!("Cannot read dir {dir:?}: {e}"))?;

        for entry in entries.flatten() {
            let child_path = entry.path();
            if SafetyGuard::is_safe(&child_path) {
                let bytes = Self::clean_path(&child_path, dry_run, permanent)?;
                reclaimed += bytes;
            }
        }

        Ok(reclaimed)
    }

    /// Execute cleaning on a list of selected CleanItems.
    pub fn execute<F>(
        items: &[CleanItem],
        dry_run: bool,
        permanent: bool,
        mut progress: F,
    ) -> CleanReport
    where
        F: FnMut(&CleanItem, usize, usize),
    {
        let start = Instant::now();
        let mut report = CleanReport {
            dry_run,
            permanent,
            duration: Duration::ZERO,
            ..Default::default()
        };

        let selected_items: Vec<&CleanItem> = items.iter().filter(|i| i.selected).collect();
        let total = selected_items.len();

        for (idx, item) in selected_items.iter().enumerate() {
            progress(item, idx + 1, total);

            if !item.subitems.is_empty() {
                let mut item_reclaimed = 0;
                for sub in &item.subitems {
                    if sub.selected {
                        match Self::clean_path(&sub.path, dry_run, permanent) {
                            Ok(bytes) => {
                                item_reclaimed += bytes;
                            }
                            Err(e) => {
                                report.failed_paths.push((sub.path.clone(), e));
                            }
                        }
                    }
                }
                report.reclaimed_bytes += item_reclaimed;
                report.cleaned_items += 1;
            } else {
                match Self::clean_path(&item.path, dry_run, permanent) {
                    Ok(bytes) => {
                        report.reclaimed_bytes += bytes;
                        report.cleaned_items += 1;
                    }
                    Err(e) => {
                        report.failed_paths.push((item.path.clone(), e));
                    }
                }
            }
        }

        report.duration = start.elapsed();
        report
    }
}
