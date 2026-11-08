use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::fs;
use std::path::Path;

/// Scan application junk matching Screenshot 2.
pub fn scan_app_junk() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. Xcode 开发垃圾
    let xcode_item = scan_xcode_junk(&home);
    items.push(xcode_item);

    // 2. Visual Studio Code
    let vscode_cache = home.join("Library/Application Support/Code/Cache");
    let vscode_cached_data = home.join("Library/Application Support/Code/CachedData");
    let vscode_logs = home.join("Library/Application Support/Code/logs");
    let mut vscode_subs = Vec::new();
    for (name, path) in [
        ("VS Code 网页资源缓存", vscode_cache.clone()),
        ("VS Code 编译与语言中间件 (CachedData)", vscode_cached_data),
        ("VS Code 运行日志", vscode_logs),
    ] {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                vscode_subs.push(SubItem {
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
        vscode_subs.iter().map(|s| s.size_bytes).sum(),
        vscode_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "app_vscode",
            CleanCategory::AppJunk,
            "Visual Studio Code",
            "默认清理 VSCode 缓存及日志文件，展开查看具体可清理项",
            vscode_cache,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(vscode_subs),
    );

    // 3. Cursor
    let cursor_cache = home.join("Library/Application Support/Cursor/Cache");
    let cursor_cached_data = home.join("Library/Application Support/Cursor/CachedData");
    let cursor_logs = home.join("Library/Application Support/Cursor/logs");
    let mut cursor_subs = Vec::new();
    for (name, path) in [
        ("Cursor 临时资源缓存", cursor_cache.clone()),
        ("Cursor 编译中间件 (CachedData)", cursor_cached_data),
        ("Cursor 运行日志", cursor_logs),
    ] {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                cursor_subs.push(SubItem {
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
        cursor_subs.iter().map(|s| s.size_bytes).sum(),
        cursor_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "app_cursor",
            CleanCategory::AppJunk,
            "Cursor",
            "默认清理 Cursor 缓存及日志文件，展开查看具体可清理项",
            cursor_cache,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(cursor_subs),
    );

    // 4. 微信 (WeChat) - 仅清理临时缓存、WebKit 与日志，绝对保护聊天记录
    let wechat_caches = home.join("Library/Containers/com.tencent.xinWeChat/Data/Library/Caches");
    let wechat_webkit = home.join("Library/Containers/com.tencent.xinWeChat/Data/Library/WebKit");
    let mut wechat_subs = Vec::new();
    if wechat_caches.exists() {
        let (sz, count) = dir_size_and_count(&wechat_caches);
        if sz > 0 {
            wechat_subs.push(SubItem {
                name: "微信临时图片/资源缓存".into(),
                path: wechat_caches.clone(),
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    if wechat_webkit.exists() {
        let (sz, count) = dir_size_and_count(&wechat_webkit);
        if sz > 0 {
            wechat_subs.push(SubItem {
                name: "微信公众号及内置网页缓存".into(),
                path: wechat_webkit,
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    let (sz, count) = (
        wechat_subs.iter().map(|s| s.size_bytes).sum(),
        wechat_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "app_wechat",
            CleanCategory::AppJunk,
            "微信",
            "默认清理微信缓存及日志文件，展开查看具体可清理项 (聊天记录已受保护)",
            wechat_caches,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(wechat_subs),
    );

    // 5. 企业微信 (WeCom)
    let wework_caches = home.join("Library/Containers/com.tencent.WeWorkMac/Data/Library/Caches");
    let (wework_sz, wework_count) = if wework_caches.exists() {
        dir_size_and_count(&wework_caches)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "app_wework",
        CleanCategory::AppJunk,
        "企业微信",
        "默认清理企业微信缓存及日志文件，展开查看具体可清理项",
        wework_caches,
        wework_sz,
        wework_count,
        SafetyLevel::Suggested,
    ));

    // 6. 网易云音乐 (Netease Cloud Music)
    let netease_caches = home.join("Library/Containers/com.netease.163music/Data/Library/Caches");
    let (netease_sz, netease_count) = if netease_caches.exists() {
        dir_size_and_count(&netease_caches)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "app_netease_music",
        CleanCategory::AppJunk,
        "网易云音乐",
        "默认清理网易云音乐缓存及日志文件，展开查看具体可清理项",
        netease_caches,
        netease_sz,
        netease_count,
        SafetyLevel::Suggested,
    ));

    // 7. 腾讯视频 (Tencent Video)
    let tenvideo_caches = home.join("Library/Containers/com.tencent.tenvideo/Data/Library/Caches");
    let (tenvideo_sz, tenvideo_count) = if tenvideo_caches.exists() {
        dir_size_and_count(&tenvideo_caches)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "app_tenvideo",
        CleanCategory::AppJunk,
        "腾讯视频",
        "默认清理腾讯视频缓存及日志文件，展开查看具体可清理项",
        tenvideo_caches,
        tenvideo_sz,
        tenvideo_count,
        SafetyLevel::Suggested,
    ));

    // 8. Quotio
    let quotio_caches = home.join("Library/Caches/Quotio");
    let (quotio_sz, quotio_count) = if quotio_caches.exists() {
        dir_size_and_count(&quotio_caches)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "app_quotio",
        CleanCategory::AppJunk,
        "Quotio",
        "清理 Quotio 缓存文件及日志文件",
        quotio_caches,
        quotio_sz,
        quotio_count,
        SafetyLevel::Suggested,
    ));

    // 9. 其它应用 (Other Apps in ~/Library/Caches)
    let other_item = scan_other_apps(&home);
    items.push(other_item);

    // 10. 安装包 (Installation Packages in ~/Downloads) - 谨慎清理
    let install_pkg_item = scan_install_packages(&home);
    items.push(install_pkg_item);

    items
}

fn scan_xcode_junk(home: &Path) -> CleanItem {
    let mut subs = Vec::new();
    let dev_root = home.join("Library/Developer/Xcode");

    for (name, rel) in [
        ("DerivedData 构建缓存", "DerivedData"),
        ("Archives 打包历史归档", "Archives"),
        ("iOS DeviceSupport 符号文件", "iOS DeviceSupport"),
        ("CoreSimulator 模拟器缓存", "../CoreSimulator/Caches"),
    ] {
        let p = dev_root.join(rel);
        if p.exists() {
            let (sz, count) = dir_size_and_count(&p);
            if sz > 0 {
                subs.push(SubItem {
                    name: name.into(),
                    path: p,
                    size_bytes: sz,
                    file_count: count,
                    safety: SafetyLevel::Suggested,
                    selected: true,
                });
            }
        }
    }

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "xcode_dev_junk",
        CleanCategory::AppJunk,
        "Xcode 开发垃圾",
        "默认清理 Xcode 缓存文件，展开查看具体可清理项",
        dev_root,
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_other_apps(home: &Path) -> CleanItem {
    let caches_dir = home.join("Library/Caches");
    let mut subs = Vec::new();
    let known_prefixes = [
        "com.apple.",
        "Google",
        "com.google.",
        "Quotio",
        "com.microsoft.",
        "Homebrew",
        "pip",
        "Yarn",
        "everyfiles",
    ];

    if caches_dir.exists() {
        if let Ok(entries) = fs::read_dir(&caches_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if known_prefixes.iter().any(|k| name.starts_with(k)) {
                        continue;
                    }
                    let (sz, count) = dir_size_and_count(&p);
                    if sz > 1024 * 100 {
                        // > 100KB
                        subs.push(SubItem {
                            name: format!("{name} 缓存"),
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "other_apps_cache",
        CleanCategory::AppJunk,
        "其它应用",
        "暂未适配清理规则的 App，清理缓存及日志文件",
        caches_dir,
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_install_packages(home: &Path) -> CleanItem {
    let dl_dir = home.join("Downloads");
    let mut subs = Vec::new();

    if dl_dir.exists() {
        if let Ok(entries) = fs::read_dir(&dl_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        let lower = ext.to_ascii_lowercase();
                        if matches!(lower.as_str(), "dmg" | "pkg" | "iso") {
                            let (sz, count) = dir_size_and_count(&p);
                            let filename = p.file_name().unwrap_or_default().to_string_lossy();
                            subs.push(SubItem {
                                name: filename.to_string(),
                                path: p,
                                size_bytes: sz,
                                file_count: count,
                                safety: SafetyLevel::Caution,
                                selected: false,
                            });
                        }
                    }
                }
            }
        }
    }

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "install_packages",
        CleanCategory::AppJunk,
        "安装包",
        "清理不用的安装包 (.dmg, .pkg, .iso)",
        dl_dir,
        total_sz,
        total_count,
        SafetyLevel::Caution,
    )
    .with_subitems(subs)
}
