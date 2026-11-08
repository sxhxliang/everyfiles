use std::path::Path;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};

/// Scan large and old files in user home directories.
pub fn scan_large_old_files() -> Vec<CleanItem> {
    scan_large_old_files_with_criteria(1024 * 1024 * 100, Duration::from_secs(86400 * 60))
}

pub fn scan_large_old_files_with_criteria(min_size: u64, min_age: Duration) -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    let target_dirs = [
        home.join("Downloads"),
        home.join("Documents"),
        home.join("Desktop"),
        home.join("Movies"),
        home.join("Music"),
    ];

    let now = SystemTime::now();
    let mut subs = Vec::new();

    for dir in &target_dirs {
        if !dir.exists() {
            continue;
        }

        for entry in WalkDir::new(dir)
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            if let Ok(meta) = entry.metadata() {
                let size = meta.len();
                if size >= min_size {
                    let is_old = meta
                        .modified()
                        .ok()
                        .and_then(|m| now.duration_since(m).ok())
                        .map(|age| age >= min_age)
                        .unwrap_or(false);

                    if is_old {
                        let path = entry.into_path();
                        let filename = path.file_name().unwrap_or_default().to_string_lossy();
                        let kind = classify_file(&path);
                        subs.push(SubItem {
                            name: format!("[{kind}] {filename}"),
                            path,
                            size_bytes: size,
                            file_count: 1,
                            safety: SafetyLevel::Caution,
                            selected: false,
                        });
                    }
                }
            }
        }
    }

    // Sort by size descending
    subs.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
    // Limit to top 100 files
    subs.truncate(100);

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    items.push(
        CleanItem::new(
            "large_old_files",
            CleanCategory::LargeOldFiles,
            "大文件与旧文件",
            "大于 100MB 且超过 60 天未修改的大型文件，按体积降序排列",
            home.join("Downloads"),
            total_sz,
            total_count,
            SafetyLevel::Caution,
        )
        .with_subitems(subs),
    );

    items
}

fn classify_file(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4" | "mkv" | "mov" | "avi" | "wmv" | "flv") => "视频",
        Some("dmg" | "iso" | "pkg") => "镜像/安装包",
        Some("zip" | "tar" | "gz" | "7z" | "rar" | "xz") => "压缩包",
        Some("mp3" | "flac" | "wav" | "aac" | "m4a") => "音频",
        Some("pdf" | "psd" | "ai" | "doc" | "docx" | "xlsx" | "pptx") => "文档/工程",
        _ => "其它文件",
    }
}
