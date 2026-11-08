use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};

/// Scan system junk items matching CleanMyMac and Lemon Cleaner categories.
pub fn scan_system_junk() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. 系统缓存 (System & User Caches)
    let mut sys_cache_sub = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let sys_caches = PathBuf::from("/Library/Caches");
        if sys_caches.exists() {
            let (sz, count) = dir_size_and_count(&sys_caches);
            if sz > 0 {
                sys_cache_sub.push(SubItem {
                    name: "全局系统缓存 (/Library/Caches)".into(),
                    path: sys_caches.clone(),
                    size_bytes: sz,
                    file_count: count,
                    safety: SafetyLevel::Suggested,
                    selected: true,
                });
            }
        }
        let user_apple_caches = home.join("Library/Caches");
        if user_apple_caches.exists() {
            if let Ok(entries) = std::fs::read_dir(&user_apple_caches) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("com.apple.") {
                            let (sz, count) = dir_size_and_count(&p);
                            if sz > 1024 * 512 {
                                sys_cache_sub.push(SubItem {
                                    name: format!("原生系统组件缓存 ({name})"),
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
    }

    #[cfg(target_os = "linux")]
    {
        let user_cache = home.join(".cache");
        if user_cache.exists() {
            let thumb = user_cache.join("thumbnails");
            if thumb.exists() {
                let (sz, count) = dir_size_and_count(&thumb);
                if sz > 0 {
                    sys_cache_sub.push(SubItem {
                        name: "桌面与文件缩略图缓存 (thumbnails)".into(),
                        path: thumb,
                        size_bytes: sz,
                        file_count: count,
                        safety: SafetyLevel::Suggested,
                        selected: true,
                    });
                }
            }
            let fontconfig = user_cache.join("fontconfig");
            if fontconfig.exists() {
                let (sz, count) = dir_size_and_count(&fontconfig);
                if sz > 0 {
                    sys_cache_sub.push(SubItem {
                        name: "系统字体索引缓存 (fontconfig)".into(),
                        path: fontconfig,
                        size_bytes: sz,
                        file_count: count,
                        safety: SafetyLevel::Suggested,
                        selected: true,
                    });
                }
            }
        }

        // Package manager caches (if readable)
        for (pkg_name, p) in [
            ("APT 离线归档缓存", PathBuf::from("/var/cache/apt/archives")),
            ("Pacman 包缓存", PathBuf::from("/var/cache/pacman/pkg")),
            ("DNF/YUM 缓存", PathBuf::from("/var/cache/dnf")),
        ] {
            if p.exists() {
                let (sz, count) = dir_size_and_count(&p);
                if sz > 0 {
                    sys_cache_sub.push(SubItem {
                        name: pkg_name.into(),
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

    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = dirs::data_local_dir() {
            let temp_dir = local_app_data.join("Temp");
            if temp_dir.exists() {
                let (sz, count) = dir_size_and_count(&temp_dir);
                if sz > 0 {
                    sys_cache_sub.push(SubItem {
                        name: "用户临时缓存目录 (%LOCALAPPDATA%\\Temp)".into(),
                        path: temp_dir,
                        size_bytes: sz,
                        file_count: count,
                        safety: SafetyLevel::Suggested,
                        selected: true,
                    });
                }
            }
            let explorer_thumbs = local_app_data.join("Microsoft/Windows/Explorer");
            if explorer_thumbs.exists() {
                let (sz, count) = dir_size_and_count(&explorer_thumbs);
                if sz > 0 {
                    sys_cache_sub.push(SubItem {
                        name: "Windows 资源管理器缩略图缓存".into(),
                        path: explorer_thumbs,
                        size_bytes: sz,
                        file_count: count,
                        safety: SafetyLevel::Suggested,
                        selected: true,
                    });
                }
            }
        }

        let win_temp = PathBuf::from("C:\\Windows\\Temp");
        if win_temp.exists() {
            let (sz, count) = dir_size_and_count(&win_temp);
            if sz > 0 {
                sys_cache_sub.push(SubItem {
                    name: "系统临时缓存 (C:\\Windows\\Temp)".into(),
                    path: win_temp,
                    size_bytes: sz,
                    file_count: count,
                    safety: SafetyLevel::Suggested,
                    selected: true,
                });
            }
        }

        let win_update = PathBuf::from("C:\\Windows\\SoftwareDistribution\\Download");
        if win_update.exists() {
            let (sz, count) = dir_size_and_count(&win_update);
            if sz > 0 {
                sys_cache_sub.push(SubItem {
                    name: "Windows 更新下载暂存 (SoftwareDistribution)".into(),
                    path: win_update,
                    size_bytes: sz,
                    file_count: count,
                    safety: SafetyLevel::Suggested,
                    selected: true,
                });
            }
        }
    }

    let (sz, count) = (
        sys_cache_sub.iter().map(|s| s.size_bytes).sum(),
        sys_cache_sub.iter().map(|s| s.file_count).sum(),
    );
    let target_cache_path = sys_cache_sub
        .first()
        .map(|s| s.path.clone())
        .unwrap_or_else(|| home.join(".cache"));

    items.push(
        CleanItem::new(
            "sys_cache",
            CleanCategory::SystemJunk,
            "系统缓存",
            "操作系统及系统应用产生的临时缓存文件",
            target_cache_path,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(sys_cache_sub),
    );

    // 2. 系统日志与崩溃报告 (System Logs & Diagnostic Reports)
    let mut log_subs = Vec::new();

    #[cfg(target_os = "macos")]
    let log_dirs = [
        ("用户应用程序日志", home.join("Library/Logs")),
        (
            "系统崩溃报告与转储",
            home.join("Library/Logs/DiagnosticReports"),
        ),
        ("系统应用日志", PathBuf::from("/Library/Logs")),
    ];

    #[cfg(target_os = "linux")]
    let log_dirs = [
        ("系统全局日志 (/var/log)", PathBuf::from("/var/log")),
        ("用户崩溃转储 (/var/crash)", PathBuf::from("/var/crash")),
        ("X11/Wayland 会话日志", home.join(".local/share/xorg")),
    ];

    #[cfg(target_os = "windows")]
    let log_dirs = [
        (
            "用户应用程序崩溃转储",
            dirs::data_local_dir()
                .map(|p| p.join("CrashDumps"))
                .unwrap_or_else(|| home.join("AppData/Local/CrashDumps")),
        ),
        (
            "Windows 错误报告 (WER 归档)",
            PathBuf::from("C:\\ProgramData\\Microsoft\\Windows\\WER"),
        ),
        (
            "Windows 内存转储与日志",
            PathBuf::from("C:\\Windows\\Minidump"),
        ),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let log_dirs: [(&str, PathBuf); 0] = [];

    for (name, path) in log_dirs {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                log_subs.push(SubItem {
                    name: name.into(),
                    path,
                    size_bytes: sz,
                    file_count: count,
                    safety: SafetyLevel::Suggested,
                    selected: true,
                });
            }
        }
    }
    let (sz, count) = (
        log_subs.iter().map(|s| s.size_bytes).sum(),
        log_subs.iter().map(|s| s.file_count).sum(),
    );
    let target_log_path = log_subs
        .first()
        .map(|s| s.path.clone())
        .unwrap_or_else(|| home.clone());

    items.push(
        CleanItem::new(
            "sys_logs",
            CleanCategory::SystemJunk,
            "系统日志",
            "系统运行时产生的日志或报告，用于系统监控或排错",
            target_log_path,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(log_subs),
    );

    // 3. iOS 照片与同步缓存 (Apple 生态，macOS 独有)
    #[cfg(target_os = "macos")]
    {
        let photo_deriv = home.join("Pictures/Photos Library.photoslibrary/resources/derivatives");
        let iphoto_thumb = home.join("Pictures/iPhoto Library/Thumbnails");
        let mut photo_size = 0;
        let mut photo_count = 0;
        let mut target_photo_path = photo_deriv.clone();

        if photo_deriv.exists() {
            let (sz, c) = dir_size_and_count(&photo_deriv);
            photo_size += sz;
            photo_count += c;
        }
        if iphoto_thumb.exists() {
            let (sz, c) = dir_size_and_count(&iphoto_thumb);
            photo_size += sz;
            photo_count += c;
            if !photo_deriv.exists() {
                target_photo_path = iphoto_thumb;
            }
        }
        items.push(CleanItem::new(
            "ios_photos_cache",
            CleanCategory::SystemJunk,
            "iOS 照片缓存",
            "iOS 设置同步时产生的照片缓存及缩略图",
            target_photo_path,
            photo_size,
            photo_count,
            SafetyLevel::Suggested,
        ));

        // 4. iOS 软件升级数据缓存
        let update_cache = home.join("Library/iTunes/iPhone Software Updates");
        let uiagent_cache =
            home.join("Library/Group Containers/group.com.apple.coreservices.uiagent");
        let mut update_size = 0;
        let mut update_count = 0;
        let mut update_path = update_cache.clone();

        if update_cache.exists() {
            let (sz, c) = dir_size_and_count(&update_cache);
            update_size += sz;
            update_count += c;
        }
        if uiagent_cache.exists() {
            let (sz, c) = dir_size_and_count(&uiagent_cache);
            update_size += sz;
            update_count += c;
            if !update_cache.exists() {
                update_path = uiagent_cache;
            }
        }
        items.push(CleanItem::new(
            "ios_update_cache",
            CleanCategory::SystemJunk,
            "iOS 软件升级数据缓存",
            "iOS 设置同步时，iOS 软件升级导致的数据固件缓存",
            update_path,
            update_size,
            update_count,
            SafetyLevel::Suggested,
        ));

        // 5. iOS 设备备份 - Caution
        let backup_dir = home.join("Library/Application Support/MobileSync/Backup");
        let (backup_sz, backup_count) = if backup_dir.exists() {
            dir_size_and_count(&backup_dir)
        } else {
            (0, 0)
        };
        items.push(CleanItem::new(
            "ios_backup",
            CleanCategory::SystemJunk,
            "iOS 设备备份",
            "历史本地整机备份，清理前请确认备份文件都是可删除的",
            backup_dir,
            backup_sz,
            backup_count,
            SafetyLevel::Caution,
        ));
    }

    // 6. 下载目录 (Downloads) - Caution (通用)
    let downloads_dir = home.join("Downloads");
    let (dl_sz, dl_count) = if downloads_dir.exists() {
        dir_size_and_count(&downloads_dir)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "downloads_dir",
        CleanCategory::SystemJunk,
        "下载",
        "清理前请确认下载中的文件都是可删除的",
        downloads_dir,
        dl_sz,
        dl_count,
        SafetyLevel::Caution,
    ));

    items
}

/// Scan Trash bins (user trash & external volumes).
pub fn scan_trash_bins() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. 用户废纸篓 (User Trash)
    #[cfg(target_os = "macos")]
    let user_trash = home.join(".Trash");

    #[cfg(target_os = "linux")]
    let user_trash = home.join(".local/share/Trash");

    #[cfg(target_os = "windows")]
    let user_trash = {
        let drive = home
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "C:".into());
        PathBuf::from(format!("{drive}\\$Recycle.Bin"))
    };

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let user_trash = home.join(".Trash");

    let (trash_sz, trash_count) = if user_trash.exists() {
        dir_size_and_count(&user_trash)
    } else {
        (0, 0)
    };

    items.push(CleanItem::new(
        "trash_bin",
        CleanCategory::Trash,
        "系统废纸篓 / 回收站",
        "清理前请确认废纸篓/回收站中的文件都是可删除的",
        user_trash,
        trash_sz,
        trash_count,
        SafetyLevel::Caution,
    ));

    // 2. 挂载外置存储废纸篓
    #[cfg(target_os = "macos")]
    {
        let volumes = Path::new("/Volumes");
        if volumes.exists() {
            if let Ok(entries) = std::fs::read_dir(volumes) {
                for entry in entries.flatten() {
                    let ext_trash = entry.path().join(".Trashes");
                    if ext_trash.exists() {
                        let (sz, count) = dir_size_and_count(&ext_trash);
                        if sz > 0 {
                            let name = entry.file_name().to_string_lossy().to_string();
                            items.push(CleanItem::new(
                                format!("ext_trash_{name}"),
                                CleanCategory::Trash,
                                format!("外置磁盘废纸篓 ({name})"),
                                "外接存储设备上的废纸篓文件",
                                ext_trash,
                                sz,
                                count,
                                SafetyLevel::Caution,
                            ));
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for mount_root in ["/media", "/mnt"] {
            let p = Path::new(mount_root);
            if p.exists() {
                if let Ok(entries) = std::fs::read_dir(p) {
                    for entry in entries.flatten() {
                        let sub = entry.path();
                        // FreeDesktop external trash spec: .Trash-$UID or .Trash
                        for trash_name in [".Trash", ".Trash-1000"] {
                            let ext_trash = sub.join(trash_name);
                            if ext_trash.exists() {
                                let (sz, count) = dir_size_and_count(&ext_trash);
                                if sz > 0 {
                                    let name = entry.file_name().to_string_lossy().to_string();
                                    items.push(CleanItem::new(
                                        format!("ext_trash_{name}"),
                                        CleanCategory::Trash,
                                        format!("挂载介质回收站 ({name})"),
                                        "可移动存储设备上的回收站数据",
                                        ext_trash,
                                        sz,
                                        count,
                                        SafetyLevel::Caution,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    items
}
