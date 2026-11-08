use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::fs;

/// Scan web junk matching Screenshot 1.
pub fn scan_web_junk() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. Mail 缓存
    let mail_downloads = home.join("Library/Containers/com.apple.mail/Data/Library/Mail Downloads");
    let mail_attachments = home.join("Library/Mail");
    let mut mail_subs = Vec::new();
    if mail_downloads.exists() {
        let (sz, count) = dir_size_and_count(&mail_downloads);
        if sz > 0 {
            mail_subs.push(SubItem {
                name: "Mail 临时下载附件".into(),
                path: mail_downloads.clone(),
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    // Scan Mail/V*/MailData/Attachments
    if mail_attachments.exists() {
        if let Ok(entries) = fs::read_dir(&mail_attachments) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('V') {
                        let att = p.join("MailData/Attachments");
                        if att.exists() {
                            let (sz, count) = dir_size_and_count(&att);
                            if sz > 0 {
                                mail_subs.push(SubItem {
                                    name: format!("Mail 历史缓存附件 ({name})"),
                                    path: att,
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
    let (sz, count) = (
        mail_subs.iter().map(|s| s.size_bytes).sum(),
        mail_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "web_mail",
            CleanCategory::WebJunk,
            "Mail 缓存",
            "收发邮件会下载大量的附件，可以将不会用到的缓存移除",
            mail_downloads,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(mail_subs),
    );

    // 2. Safari
    let safari_cache = home.join("Library/Containers/com.apple.Safari/Data/Library/Caches");
    let safari_local = home.join("Library/Safari/LocalStorage");
    let mut safari_subs = Vec::new();
    if safari_cache.exists() {
        let (sz, count) = dir_size_and_count(&safari_cache);
        if sz > 0 {
            safari_subs.push(SubItem {
                name: "Safari 网页资源缓存".into(),
                path: safari_cache.clone(),
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    if safari_local.exists() {
        let (sz, count) = dir_size_and_count(&safari_local);
        if sz > 0 {
            safari_subs.push(SubItem {
                name: "Safari 网页本地存储缓存".into(),
                path: safari_local,
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    let (sz, count) = (
        safari_subs.iter().map(|s| s.size_bytes).sum(),
        safari_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "web_safari",
            CleanCategory::WebJunk,
            "Safari",
            "Safari 加载页面时缓存的一些网页或资源",
            safari_cache,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(safari_subs),
    );

    // 3. Chrome
    let chrome_cache = home.join("Library/Caches/Google/Chrome/Default/Cache");
    let chrome_code_cache = home.join("Library/Caches/Google/Chrome/Default/Code Cache");
    let mut chrome_subs = Vec::new();
    if chrome_cache.exists() {
        let (sz, count) = dir_size_and_count(&chrome_cache);
        if sz > 0 {
            chrome_subs.push(SubItem {
                name: "Chrome 网页图片与资源缓存".into(),
                path: chrome_cache.clone(),
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    if chrome_code_cache.exists() {
        let (sz, count) = dir_size_and_count(&chrome_code_cache);
        if sz > 0 {
            chrome_subs.push(SubItem {
                name: "Chrome JS/V8 脚本编译缓存 (Code Cache)".into(),
                path: chrome_code_cache,
                size_bytes: sz,
                file_count: count,
                safety: SafetyLevel::Suggested,
                selected: true,
            });
        }
    }
    let (sz, count) = (
        chrome_subs.iter().map(|s| s.size_bytes).sum(),
        chrome_subs.iter().map(|s| s.file_count).sum(),
    );
    items.push(
        CleanItem::new(
            "web_chrome",
            CleanCategory::WebJunk,
            "Chrome",
            "Chrome 加载页面时缓存的一些网页或资源",
            chrome_cache,
            sz,
            count,
            SafetyLevel::Suggested,
        )
        .with_subitems(chrome_subs),
    );

    // 4. 夸克 (Quark)
    let quark_cache = home.join("Library/Caches/com.alibaba.Quark");
    let (quark_sz, quark_count) = if quark_cache.exists() {
        dir_size_and_count(&quark_cache)
    } else {
        (0, 0)
    };
    items.push(CleanItem::new(
        "web_quark",
        CleanCategory::WebJunk,
        "夸克",
        "夸克 加载页面时缓存的网页或资源",
        quark_cache,
        quark_sz,
        quark_count,
        SafetyLevel::Suggested,
    ));

    // 5. 下载日志 (Download Logs / Quarantine Events)
    let pref_dir = home.join("Library/Preferences");
    let mut dl_log_subs = Vec::new();
    let mut dl_log_size = 0u64;
    let mut dl_log_count = 0usize;

    if pref_dir.exists() {
        if let Ok(entries) = fs::read_dir(&pref_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("com.apple.LaunchServices.QuarantineEvents") {
                        let (sz, count) = dir_size_and_count(&p);
                        dl_log_size += sz;
                        dl_log_count += count;
                        dl_log_subs.push(SubItem {
                            name: format!("隔离事件记录: {name}"),
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

    let primary_quarantine =
        home.join("Library/Preferences/com.apple.LaunchServices.QuarantineEventsV2");
    items.push(
        CleanItem::new(
            "web_download_logs",
            CleanCategory::WebJunk,
            "下载日志",
            "不可信来源下载应用程序的日志及历史下载隔离数据库",
            primary_quarantine,
            dl_log_size,
            dl_log_count,
            SafetyLevel::Suggested,
        )
        .with_subitems(dl_log_subs),
    );

    items
}
