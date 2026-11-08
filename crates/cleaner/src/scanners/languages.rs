use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::path::{Path, PathBuf};

/// Unneeded localization (.lproj) scanner.
pub fn scan_unneeded_languages() -> Vec<CleanItem> {
    let mut items = Vec::new();

    let mut app_dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        app_dirs.push(home.join("Applications"));
    }

    let mut total_size = 0u64;
    let mut total_files = 0usize;
    let mut subitems = Vec::new();

    for base_dir in app_dirs {
        if !base_dir.exists() {
            continue;
        }

        if let Ok(entries) = std::fs::read_dir(&base_dir) {
            for entry in entries.flatten() {
                let app_path = entry.path();
                if app_path.is_dir() && app_path.extension().and_then(|s| s.to_str()) == Some("app")
                {
                    let app_name = app_path.file_name().unwrap_or_default().to_string_lossy();
                    if app_name == "Safari.app" || app_path.starts_with("/System") {
                        continue;
                    }
                    let resources_dir = app_path.join("Contents/Resources");
                    if resources_dir.exists() {
                        collect_unneeded_lproj(
                            &resources_dir,
                            &mut subitems,
                            &mut total_size,
                            &mut total_files,
                        );
                    }
                }
            }
        }
    }

    let item = CleanItem::new(
        "unneeded_languages",
        CleanCategory::SystemJunk,
        "无用语言文件",
        "一般会用到多语言文件中的1-2种，其它语言可清理（保留中英文）",
        PathBuf::from("/Applications"),
        total_size,
        total_files,
        SafetyLevel::Suggested,
    )
    .with_subitems(subitems);

    items.push(item);
    items
}

fn collect_unneeded_lproj(
    resources: &Path,
    subitems: &mut Vec<SubItem>,
    total_size: &mut u64,
    total_files: &mut usize,
) {
    if let Ok(entries) = std::fs::read_dir(resources) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if name.ends_with(".lproj") && !is_kept_language(name) {
                        let (sz, count) = dir_size_and_count(&path);
                        if sz > 0 {
                            *total_size += sz;
                            *total_files += count;
                            subitems.push(SubItem {
                                name: format!("语言包: {name} ({})", path.display()),
                                path,
                                size_bytes: sz,
                                file_count: count,
                                safety: SafetyLevel::Suggested,
                                selected: true,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Kept languages (Chinese variants, English variants, Base).
fn is_kept_language(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("zh")
        || lower.starts_with("en")
        || lower.starts_with("base")
        || lower == "c.lproj"
}
