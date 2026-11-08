use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::path::{Path, PathBuf};

/// Scan system junk items matching CleanMyMac and Lemon Cleaner categories.
pub fn scan_system_junk() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. 系统缓存 (System Caches)
    let mut sys_cache_sub = Vec::new();
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
    // Apple native user caches
    let user_apple_caches = home.join("Library/Caches");
    if user_apple_caches.exists() {
        if let Ok(entries) = std::fs::read_dir(&user_apple_caches) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("com.apple.") {
                        let (sz, count) = dir_size_and_count(&p);
                        if sz > 1024 * 512 {
                            // > 512KB
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
    let (sz, count) = (
        sys_cache_sub.iter().map(|s| s.size_bytes).sum(),
        sys_cache_sub.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "sys_cache",
            CleanCategory::SystemJunk,
            "系统缓存",
            "系统应用产生的临时缓存文件",
            sys_caches,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(sys_cache_sub),
    );

    // 2. 系统日志 (System Logs & Diagnostic Reports)
    let mut log_subs = Vec::new();
    let log_dirs = [
        ("用户应用程序日志", home.join("Library/Logs")),
        (
            "系统崩溃报告与转储",
            home.join("Library/Logs/DiagnosticReports"),
        ),
        ("系统应用日志", PathBuf::from("/Library/Logs")),
    ];

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
    items.push(
        CleanItem::new(
            "sys_logs",
            CleanCategory::SystemJunk,
            "系统日志",
            "系统运行时产生的日志或报告，用于系统监控或排错",
            home.join("Library/Logs"),
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(log_subs),
    );

    // 3. iOS 照片缓存 (iOS Photo Cache)
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

    // 4. iOS 软件升级数据缓存 (iOS Software Update Cache)
    let update_cache = home.join("Library/iTunes/iPhone Software Updates");
    let uiagent_cache = home.join("Library/Group Containers/group.com.apple.coreservices.uiagent");
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

    // 5. iOS 设备备份 (iOS Device Backups) - Caution
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

    // 6. 下载目录 (Downloads) - Caution
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

    let user_trash = home.join(".Trash");
    let (trash_sz, trash_count) = if user_trash.exists() {
        dir_size_and_count(&user_trash)
    } else {
        (0, 0)
    };

    items.push(CleanItem::new(
        "trash_bin",
        CleanCategory::Trash,
        "废纸篓",
        "清理前请确认废纸篓中的文件都是可删除的",
        user_trash,
        trash_sz,
        trash_count,
        SafetyLevel::Caution,
    ));

    // Check external volume trashes
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

    items
}
