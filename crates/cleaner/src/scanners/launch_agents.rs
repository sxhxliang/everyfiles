use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::fs;
use std::path::{Path, PathBuf};

/// Scan broken login items and launch agents whose target executables no longer exist.
pub fn scan_broken_login_items() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let mut search_dirs = vec![
        PathBuf::from("/Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchDaemons"),
    ];

    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join("Library/LaunchAgents"));
    }

    let mut broken_subs = Vec::new();
    let mut total_size = 0u64;
    let mut total_count = 0usize;

    for dir in &search_dirs {
        if !dir.exists() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("plist") {
                    if let Some(broken_target) = check_broken_plist(&path) {
                        let (sz, count) = dir_size_and_count(&path);
                        total_size += sz;
                        total_count += count;
                        let filename = path.file_name().unwrap_or_default().to_string_lossy();
                        broken_subs.push(SubItem {
                            name: format!("{filename} -> 缺失目标: {broken_target}"),
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

    let primary_path = search_dirs
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("/Library/LaunchAgents"));
    items.push(
        CleanItem::new(
            "broken_launch_agents",
            CleanCategory::SystemJunk,
            "损坏的启动项",
            "应用程序或服务被移除后，对应启动配置的残留文件",
            primary_path,
            total_size,
            total_count,
            SafetyLevel::Suggested,
        )
        .with_subitems(broken_subs),
    );

    items
}

/// Checks if a plist references a binary that does not exist.
fn check_broken_plist(plist_path: &Path) -> Option<String> {
    let content = fs::read(plist_path).ok()?;
    // Plist files can be XML or Apple binary format. We parse common Program string patterns.
    let text = String::from_utf8_lossy(&content);

    // Look for <key>Program</key><string>/path/to/bin</string>
    if let Some(idx) = text.find("<key>Program</key>") {
        let remainder = &text[idx + 18..];
        if let Some(start) = remainder.find("<string>") {
            if let Some(end) = remainder[start + 8..].find("</string>") {
                let target_path = &remainder[start + 8..start + 8 + end];
                if !target_path.is_empty() && !Path::new(target_path).exists() {
                    return Some(target_path.to_string());
                }
            }
        }
    }

    // Look for <key>ProgramArguments</key> ... <string>/path/to/bin</string>
    if let Some(idx) = text.find("<key>ProgramArguments</key>") {
        let remainder = &text[idx + 27..];
        if let Some(start) = remainder.find("<string>") {
            if let Some(end) = remainder[start + 8..].find("</string>") {
                let target_path = &remainder[start + 8..start + 8 + end];
                if target_path.starts_with('/') && !Path::new(target_path).exists() {
                    return Some(target_path.to_string());
                }
            }
        }
    }

    None
}
