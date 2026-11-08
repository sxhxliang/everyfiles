use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Scan application uninstaller leftovers: folders remaining from previously uninstalled applications.
pub fn scan_leftovers() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    let installed_ids = collect_installed_app_identifiers(&home);

    let mut leftover_subs = Vec::new();
    let mut total_size = 0u64;
    let mut total_count = 0usize;

    // Scan ~/Library/Application Support
    let app_support = home.join("Library/Application Support");
    check_orphans(
        &app_support,
        &installed_ids,
        &mut leftover_subs,
        &mut total_size,
        &mut total_count,
    );

    // Scan ~/Library/Saved Application State
    let saved_state = home.join("Library/Saved Application State");
    check_orphans(
        &saved_state,
        &installed_ids,
        &mut leftover_subs,
        &mut total_size,
        &mut total_count,
    );

    // Scan ~/Library/WebKit
    let webkit_dir = home.join("Library/WebKit");
    check_orphans(
        &webkit_dir,
        &installed_ids,
        &mut leftover_subs,
        &mut total_size,
        &mut total_count,
    );

    if !leftover_subs.is_empty() {
        items.push(
            CleanItem::new(
                "app_leftovers",
                CleanCategory::AppJunk,
                "应用卸载残留",
                "已移除软件在系统中遗留的无用配置文件与数据支持碎片",
                app_support,
                total_size,
                total_count,
                SafetyLevel::Suggested,
            )
            .with_subitems(leftover_subs),
        );
    }

    items
}

fn check_orphans(
    parent: &Path,
    installed: &HashSet<String>,
    subs: &mut Vec<SubItem>,
    total_size: &mut u64,
    total_count: &mut usize,
) {
    if !parent.exists() {
        return;
    }

    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                // Ignore macOS system and common developer runtime tools
                if name.starts_with("com.apple.")
                    || name.starts_with("Apple")
                    || name == "CrashReporter"
                    || name == "MobileSync"
                    || name == "Code"
                    || name == "Cursor"
                    || name == "CleanMyMac"
                    || name == "Google"
                    || name == "everyfiles"
                {
                    continue;
                }

                // Strip .savedState suffix if present
                let clean_name = name.strip_suffix(".savedState").unwrap_or(name);

                // If not found in installed apps identifiers or names
                let lower = clean_name.to_ascii_lowercase();
                let matched = installed
                    .iter()
                    .any(|inst| inst == &lower || inst.contains(&lower) || lower.contains(inst));

                if !matched {
                    let (sz, count) = dir_size_and_count(&p);
                    if sz > 1024 * 50 {
                        // > 50KB
                        *total_size += sz;
                        *total_count += count;
                        subs.push(SubItem {
                            name: format!("孤立残留: {name}"),
                            path: p,
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

/// Collect names and CFBundleIdentifiers of installed apps in /Applications and ~/Applications.
fn collect_installed_app_identifiers(home: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    let dirs = [
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        home.join("Applications"),
    ];

    for base in &dirs {
        if !base.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.extension().and_then(|s| s.to_str()) == Some("app") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        set.insert(stem.to_ascii_lowercase());
                    }
                    // Try to read CFBundleIdentifier from Info.plist
                    let plist_path = p.join("Contents/Info.plist");
                    if let Ok(content) = fs::read_to_string(&plist_path) {
                        if let Some(idx) = content.find("<key>CFBundleIdentifier</key>") {
                            let rem = &content[idx + 29..];
                            if let Some(s) = rem.find("<string>") {
                                if let Some(e) = rem[s + 8..].find("</string>") {
                                    let id = &rem[s + 8..s + 8 + e];
                                    set.insert(id.trim().to_ascii_lowercase());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    set
}
