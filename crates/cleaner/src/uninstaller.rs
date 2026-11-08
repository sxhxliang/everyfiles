use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::deleter::SafeDeleter;
use crate::domain::{CleanItem, CleanReport, SafetyLevel};
use crate::scanners::dir_size_and_count;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppAssociatedFile {
    pub category_name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_count: usize,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledApp {
    pub name: String,
    pub display_name: String,
    pub bundle_id: String,
    pub version: String,
    pub bundle_path: PathBuf,
    pub is_system: bool,
    pub associated_files: Vec<AppAssociatedFile>,
    pub total_size_bytes: u64,
}

pub struct AppUninstaller;

impl AppUninstaller {
    /// Scan and inventory all installed applications across system and user directories.
    pub fn scan_all_apps() -> Vec<InstalledApp> {
        let mut app_dirs = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
        ];

        if let Some(home) = dirs::home_dir() {
            app_dirs.push(home.join("Applications"));
        }

        let mut app_paths = Vec::new();
        for dir in app_dirs {
            if !dir.exists() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app") {
                        app_paths.push(p);
                    }
                }
            }
        }

        let home = dirs::home_dir();

        let mut apps: Vec<InstalledApp> = app_paths
            .into_par_iter()
            .filter_map(|bundle_path| Self::inspect_app(&bundle_path, home.as_deref()))
            .collect();

        // Sort apps by total size descending
        apps.sort_by_key(|a| std::cmp::Reverse(a.total_size_bytes));
        apps
    }

    /// Fast search for an installed application matching query (name or bundle ID).
    pub fn find_app(query: &str) -> Vec<InstalledApp> {
        let q = query.trim().to_ascii_lowercase();
        let home = dirs::home_dir();

        let mut app_dirs = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
        ];
        if let Some(ref h) = home {
            app_dirs.push(h.join("Applications"));
        }

        let mut matched_bundles = Vec::new();
        for dir in app_dirs {
            if !dir.exists() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app") {
                        let name = p
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if name.to_ascii_lowercase().contains(&q) {
                            matched_bundles.push(p);
                        } else {
                            let (bundle_id, display_name, _) = parse_info_plist(&p);
                            if bundle_id.to_ascii_lowercase().contains(&q)
                                || display_name.to_ascii_lowercase().contains(&q)
                            {
                                matched_bundles.push(p);
                            }
                        }
                    }
                }
            }
        }

        matched_bundles
            .into_par_iter()
            .filter_map(|bundle_path| Self::inspect_app(&bundle_path, home.as_deref()))
            .collect()
    }

    /// Inspect a single .app bundle and locate all related files and data.
    pub fn inspect_app(bundle_path: &Path, home: Option<&Path>) -> Option<InstalledApp> {
        if !bundle_path.exists() {
            return None;
        }

        let raw_name = bundle_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let is_system = bundle_path.starts_with("/System")
            || raw_name == "Safari"
            || raw_name == "Finder"
            || raw_name == "App Store";

        let (bundle_id, display_name, version) = parse_info_plist(bundle_path);

        let final_display_name = if !display_name.is_empty() {
            display_name
        } else {
            raw_name.clone()
        };

        let mut associated = Vec::new();

        // 1. App Bundle itself
        let (app_size, app_count) = dir_size_and_count(bundle_path);
        associated.push(AppAssociatedFile {
            category_name: "应用程序本体 (.app)".to_string(),
            path: bundle_path.to_path_buf(),
            size_bytes: app_size,
            file_count: app_count,
            selected: !is_system,
        });

        // 2. Discover related data in ~/Library if home is available
        if let Some(h) = home {
            let lib = h.join("Library");
            let identifiers = [
                bundle_id.as_str(),
                raw_name.as_str(),
                final_display_name.as_str(),
            ];

            let targets = [
                (
                    "应用支持数据 (Application Support)",
                    lib.join("Application Support"),
                ),
                ("应用临时缓存 (Caches)", lib.join("Caches")),
                ("沙盒容器 (Containers)", lib.join("Containers")),
                (
                    "保存状态 (Saved Application State)",
                    lib.join("Saved Application State"),
                ),
                ("WebKit 网页数据", lib.join("WebKit")),
                ("HTTP 存储", lib.join("HTTPStorages")),
                ("运行日志 (Logs)", lib.join("Logs")),
            ];

            for (cat_name, parent_dir) in targets {
                if !parent_dir.exists() {
                    continue;
                }

                for &id in &identifiers {
                    if id.is_empty() {
                        continue;
                    }
                    let candidate = parent_dir.join(id);
                    if candidate.exists() && !associated.iter().any(|a| a.path == candidate) {
                        let (sz, count) = dir_size_and_count(&candidate);
                        associated.push(AppAssociatedFile {
                            category_name: cat_name.to_string(),
                            path: candidate,
                            size_bytes: sz,
                            file_count: count,
                            selected: !is_system,
                        });
                    }

                    // Check .savedState suffix
                    if cat_name.contains("Saved Application State") {
                        let candidate_state = parent_dir.join(format!("{id}.savedState"));
                        if candidate_state.exists()
                            && !associated.iter().any(|a| a.path == candidate_state)
                        {
                            let (sz, count) = dir_size_and_count(&candidate_state);
                            associated.push(AppAssociatedFile {
                                category_name: cat_name.to_string(),
                                path: candidate_state,
                                size_bytes: sz,
                                file_count: count,
                                selected: !is_system,
                            });
                        }
                    }
                }
            }

            // Preferences plist
            if !bundle_id.is_empty() {
                let pref_file = lib.join("Preferences").join(format!("{bundle_id}.plist"));
                if pref_file.exists() && !associated.iter().any(|a| a.path == pref_file) {
                    let (sz, count) = dir_size_and_count(&pref_file);
                    associated.push(AppAssociatedFile {
                        category_name: "偏好设置文件 (Preferences plist)".to_string(),
                        path: pref_file,
                        size_bytes: sz,
                        file_count: count,
                        selected: !is_system,
                    });
                }
            }

            // LaunchAgents
            let launch_agent = lib.join("LaunchAgents");
            if launch_agent.exists() && !bundle_id.is_empty() {
                if let Ok(entries) = fs::read_dir(&launch_agent) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                            if fname.contains(&bundle_id) && !associated.iter().any(|a| a.path == p)
                            {
                                let (sz, count) = dir_size_and_count(&p);
                                associated.push(AppAssociatedFile {
                                    category_name: "开机后台启动项 (LaunchAgent)".to_string(),
                                    path: p,
                                    size_bytes: sz,
                                    file_count: count,
                                    selected: !is_system,
                                });
                            }
                        }
                    }
                }
            }
        }

        let total_size_bytes = associated.iter().map(|a| a.size_bytes).sum();

        Some(InstalledApp {
            name: raw_name,
            display_name: final_display_name,
            bundle_id,
            version,
            bundle_path: bundle_path.to_path_buf(),
            is_system,
            associated_files: associated,
            total_size_bytes,
        })
    }

    /// Completely uninstall an application, moving selected associated files to macOS Trash.
    pub fn uninstall(app: &InstalledApp, dry_run: bool, permanent: bool) -> CleanReport {
        let items: Vec<CleanItem> = app
            .associated_files
            .iter()
            .filter(|f| f.selected)
            .map(|f| {
                CleanItem::new(
                    f.path.to_string_lossy().to_string(),
                    crate::domain::CleanCategory::AppJunk,
                    &f.category_name,
                    f.path.display().to_string(),
                    f.path.clone(),
                    f.size_bytes,
                    f.file_count,
                    SafetyLevel::Suggested,
                )
            })
            .collect();

        SafeDeleter::execute(&items, dry_run, permanent, |_item, _cur, _tot| {})
    }
}

/// Parse Info.plist to extract bundle identifier, display name, and version.
fn parse_info_plist(bundle_path: &Path) -> (String, String, String) {
    let plist_path = bundle_path.join("Contents/Info.plist");
    if !plist_path.exists() {
        return (String::new(), String::new(), String::new());
    }

    let mut content = fs::read_to_string(&plist_path).unwrap_or_default();
    if content.is_empty() || content.starts_with("bplist") {
        // Fallback: convert binary plist using plutil
        if let Ok(out) = Command::new("plutil")
            .args([
                "-convert",
                "xml1",
                "-o",
                "-",
                plist_path.to_str().unwrap_or_default(),
            ])
            .output()
        {
            if out.status.success() {
                content = String::from_utf8_lossy(&out.stdout).to_string();
            }
        }
    }

    let bundle_id = extract_key_string(&content, "CFBundleIdentifier");
    let display_name = {
        let d = extract_key_string(&content, "CFBundleDisplayName");
        if !d.is_empty() {
            d
        } else {
            extract_key_string(&content, "CFBundleName")
        }
    };
    let version = extract_key_string(&content, "CFBundleShortVersionString");

    (bundle_id, display_name, version)
}

fn extract_key_string(xml: &str, key: &str) -> String {
    let key_tag = format!("<key>{key}</key>");
    if let Some(idx) = xml.find(&key_tag) {
        let rem = &xml[idx + key_tag.len()..];
        if let Some(s) = rem.find("<string>") {
            if let Some(e) = rem[s + 8..].find("</string>") {
                return rem[s + 8..s + 8 + e].trim().to_string();
            }
        }
    }
    String::new()
}
