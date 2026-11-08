#![allow(unused_imports, dead_code)]

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
        let mut app_paths = Vec::new();

        #[cfg(target_os = "macos")]
        {
            let mut app_dirs = vec![
                PathBuf::from("/Applications"),
                PathBuf::from("/System/Applications"),
            ];
            if let Some(home) = dirs::home_dir() {
                app_dirs.push(home.join("Applications"));
            }
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
        }

        #[cfg(target_os = "linux")]
        {
            let mut app_dirs = vec![
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/usr/local/share/applications"),
            ];
            if let Some(home) = dirs::home_dir() {
                app_dirs.push(home.join(".local/share/applications"));
                // Flatpak apps
                let flatpak_dir = home.join(".var/app");
                if flatpak_dir.exists() {
                    if let Ok(entries) = fs::read_dir(&flatpak_dir) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.is_dir() {
                                app_paths.push(p);
                            }
                        }
                    }
                }
            }
            for dir in app_dirs {
                if !dir.exists() {
                    continue;
                }
                if let Ok(entries) = fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("desktop")
                        {
                            app_paths.push(p);
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let mut app_dirs = vec![
                PathBuf::from("C:\\Program Files"),
                PathBuf::from("C:\\Program Files (x86)"),
            ];
            if let Some(local) = dirs::data_local_dir() {
                app_dirs.push(local.join("Programs"));
            }
            for dir in app_dirs {
                if !dir.exists() {
                    continue;
                }
                if let Ok(entries) = fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_dir() {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            if name != "Common Files"
                                && name != "Windows NT"
                                && !name.starts_with('$')
                            {
                                app_paths.push(p);
                            }
                        }
                    }
                }
            }
        }

        let home = dirs::home_dir();

        let mut apps: Vec<InstalledApp> = app_paths
            .into_par_iter()
            .filter_map(|path| Self::inspect_app(&path, home.as_deref()))
            .collect();

        // Sort apps by total size descending
        apps.sort_by_key(|a| std::cmp::Reverse(a.total_size_bytes));
        apps
    }

    /// Fast search for an installed application matching query (name or bundle ID).
    pub fn find_app(query: &str) -> Vec<InstalledApp> {
        let q = query.trim().to_ascii_lowercase();
        let apps = Self::scan_all_apps();

        apps.into_iter()
            .filter(|app| {
                app.name.to_ascii_lowercase().contains(&q)
                    || app.display_name.to_ascii_lowercase().contains(&q)
                    || app.bundle_id.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }

    /// Inspect an application bundle/desktop-entry/install-folder and locate all related files and data.
    pub fn inspect_app(app_path: &Path, home: Option<&Path>) -> Option<InstalledApp> {
        if !app_path.exists() {
            return None;
        }

        #[cfg(target_os = "macos")]
        return Self::inspect_macos_app(app_path, home);

        #[cfg(target_os = "linux")]
        return Self::inspect_linux_app(app_path, home);

        #[cfg(target_os = "windows")]
        return Self::inspect_windows_app(app_path, home);

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        Self::inspect_macos_app(app_path, home)
    }

    #[cfg(target_os = "macos")]
    fn inspect_macos_app(bundle_path: &Path, home: Option<&Path>) -> Option<InstalledApp> {
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

        let total_size: u64 = associated.iter().map(|f| f.size_bytes).sum();

        Some(InstalledApp {
            name: raw_name,
            display_name: final_display_name,
            bundle_id,
            version,
            bundle_path: bundle_path.to_path_buf(),
            is_system,
            associated_files: associated,
            total_size_bytes: total_size,
        })
    }

    #[cfg(target_os = "linux")]
    fn inspect_linux_app(app_path: &Path, home: Option<&Path>) -> Option<InstalledApp> {
        let (raw_name, display_name, version, is_system) = if app_path.is_file() {
            let (id, name, ver) = parse_desktop_file(app_path);
            let is_sys = app_path.starts_with("/usr/share");
            (id, name, ver, is_sys)
        } else {
            let raw = app_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            (raw.clone(), raw, String::new(), false)
        };

        let mut associated = Vec::new();
        let (main_sz, main_count) = dir_size_and_count(app_path);
        associated.push(AppAssociatedFile {
            category_name: "桌面快捷方式 / 应用主目录".to_string(),
            path: app_path.to_path_buf(),
            size_bytes: main_sz,
            file_count: main_count,
            selected: !is_system,
        });

        if let Some(h) = home {
            let identifiers = [raw_name.as_str(), display_name.as_str()];
            for &id in &identifiers {
                if id.is_empty() {
                    continue;
                }
                for (cat_name, base_dir) in [
                    ("用户配置 (~/.config)", h.join(".config")),
                    ("用户缓存 (~/.cache)", h.join(".cache")),
                    ("应用数据 (~/.local/share)", h.join(".local/share")),
                    ("Flatpak 沙盒数据 (~/.var/app)", h.join(".var/app")),
                ] {
                    let candidate = base_dir.join(id);
                    if candidate.exists() && !associated.iter().any(|a| a.path == candidate) {
                        let (sz, count) = dir_size_and_count(&candidate);
                        associated.push(AppAssociatedFile {
                            category_name: cat_name.to_string(),
                            path: candidate,
                            size_bytes: sz,
                            file_count: count,
                            selected: true,
                        });
                    }
                }
            }
        }

        let total_size: u64 = associated.iter().map(|f| f.size_bytes).sum();
        Some(InstalledApp {
            name: raw_name.clone(),
            display_name,
            bundle_id: raw_name,
            version,
            bundle_path: app_path.to_path_buf(),
            is_system,
            associated_files: associated,
            total_size_bytes: total_size,
        })
    }

    #[cfg(target_os = "windows")]
    fn inspect_windows_app(app_path: &Path, home: Option<&Path>) -> Option<InstalledApp> {
        let raw_name = app_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let is_system = raw_name == "Windows Defender" || raw_name == "Internet Explorer";
        let mut associated = Vec::new();

        let (app_size, app_count) = dir_size_and_count(app_path);
        associated.push(AppAssociatedFile {
            category_name: "应用程序安装目录 (Program Files)".to_string(),
            path: app_path.to_path_buf(),
            size_bytes: app_size,
            file_count: app_count,
            selected: !is_system,
        });

        if let Some(h) = home {
            let appdata_roaming = h.join("AppData\\Roaming");
            let appdata_local = h.join("AppData\\Local");

            for (cat_name, base_dir) in [
                ("应用配置与漫游数据 (AppData\\Roaming)", appdata_roaming),
                ("应用本地数据与缓存 (AppData\\Local)", appdata_local),
            ] {
                if base_dir.exists() {
                    let candidate = base_dir.join(&raw_name);
                    if candidate.exists() && !associated.iter().any(|a| a.path == candidate) {
                        let (sz, count) = dir_size_and_count(&candidate);
                        associated.push(AppAssociatedFile {
                            category_name: cat_name.to_string(),
                            path: candidate,
                            size_bytes: sz,
                            file_count: count,
                            selected: true,
                        });
                    }
                }
            }
        }

        let total_size: u64 = associated.iter().map(|f| f.size_bytes).sum();
        Some(InstalledApp {
            name: raw_name.clone(),
            display_name: raw_name.clone(),
            bundle_id: raw_name,
            version: String::new(),
            bundle_path: app_path.to_path_buf(),
            is_system,
            associated_files: associated,
            total_size_bytes: total_size,
        })
    }

    /// Perform clean uninstallation of application and its associated files into Trash.
    pub fn uninstall(app: &InstalledApp, dry_run: bool, permanent: bool) -> CleanReport {
        let items: Vec<CleanItem> = app
            .associated_files
            .iter()
            .filter(|f| f.selected)
            .map(|f| {
                CleanItem::new(
                    format!("{}_{}", app.bundle_id, f.category_name),
                    crate::domain::CleanCategory::AppJunk,
                    &f.category_name,
                    format!("{} 关联数据", app.display_name),
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

/// Parse Linux .desktop file to extract name, version and id.
#[cfg(target_os = "linux")]
fn parse_desktop_file(path: &Path) -> (String, String, String) {
    let mut id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut name = String::new();
    let mut version = String::new();

    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(val) = trimmed.strip_prefix("Name=") {
                if name.is_empty() {
                    name = val.trim().to_string();
                }
            } else if let Some(val) = trimmed.strip_prefix("Version=") {
                if version.is_empty() {
                    version = val.trim().to_string();
                }
            } else if let Some(val) = trimmed.strip_prefix("StartupWMClass=") {
                id = val.trim().to_string();
            }
        }
    }
    if name.is_empty() {
        name = id.clone();
    }
    (id, name, version)
}

/// Parse Info.plist to extract bundle identifier, display name, and version on macOS.
#[cfg(any(target_os = "macos", test))]
fn parse_info_plist(bundle_path: &Path) -> (String, String, String) {
    let plist_path = bundle_path.join("Contents/Info.plist");
    if !plist_path.exists() {
        return (String::new(), String::new(), String::new());
    }

    let mut content = fs::read_to_string(&plist_path).unwrap_or_default();
    if content.is_empty() || content.starts_with("bplist") {
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

#[cfg(any(target_os = "macos", test))]
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
