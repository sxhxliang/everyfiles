use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Top-level cleanup category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CleanCategory {
    /// 系统垃圾: 系统缓存、系统日志、无用语言包、损坏启动项、iOS升级/备份等
    SystemJunk,
    /// 应用垃圾: 微信、企业微信、VSCode、Cursor、网易云、腾讯视频、常用App缓存与卸载残留
    AppJunk,
    /// 上网垃圾: Mail缓存、Safari、Chrome、夸克、下载日志等
    WebJunk,
    /// 开发工具: Cargo、npm/yarn/pnpm、Python/pip/uv/conda、Xcode、Go、Gradle等
    DevJunk,
    /// 大文件与旧文件: 超大视频、归档、镜像、长期闲置文档
    LargeOldFiles,
    /// 废纸篓: 用户废纸篓与外接磁盘废纸篓
    Trash,
}

impl CleanCategory {
    pub fn title(&self) -> &'static str {
        match self {
            Self::SystemJunk => "系统垃圾",
            Self::AppJunk => "应用垃圾",
            Self::WebJunk => "上网垃圾",
            Self::DevJunk => "开发工具",
            Self::LargeOldFiles => "大文件与旧文件",
            Self::Trash => "废纸篓",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::SystemJunk => "⚙️",
            Self::AppJunk => "📦",
            Self::WebJunk => "🌐",
            Self::DevJunk => "💻",
            Self::LargeOldFiles => "📁",
            Self::Trash => "🗑️",
        }
    }

    pub fn all() -> &'static [CleanCategory] {
        &[
            Self::SystemJunk,
            Self::AppJunk,
            Self::WebJunk,
            Self::DevJunk,
            Self::LargeOldFiles,
            Self::Trash,
        ]
    }
}

impl fmt::Display for CleanCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.title())
    }
}

/// Cleaning recommendation safety indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SafetyLevel {
    /// 很干净 (0 字节或无可清理项)
    Clean,
    /// 建议清理 (临时缓存、运行日志，清理安全无风险)
    Suggested,
    /// 谨慎清理 (下载项、系统备份、用户代码构建产物等，需用户确认)
    Caution,
}

impl SafetyLevel {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Clean => "很干净",
            Self::Suggested => "建议清理",
            Self::Caution => "谨慎清理",
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            Self::Clean => "⚪",
            Self::Suggested => "🟢",
            Self::Caution => "🟡",
        }
    }
}

/// Individual item representing a sub-directory or sub-target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubItem {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_count: usize,
    pub safety: SafetyLevel,
    pub selected: bool,
}

/// Cleanable item row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanItem {
    pub id: String,
    pub category: CleanCategory,
    pub title: String,
    pub description: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_count: usize,
    pub safety: SafetyLevel,
    pub selected: bool,
    pub subitems: Vec<SubItem>,
}

impl CleanItem {
    pub fn new(
        id: impl Into<String>,
        category: CleanCategory,
        title: impl Into<String>,
        description: impl Into<String>,
        path: PathBuf,
        size_bytes: u64,
        file_count: usize,
        safety: SafetyLevel,
    ) -> Self {
        let is_clean = size_bytes == 0;
        let actual_safety = if is_clean { SafetyLevel::Clean } else { safety };
        // By default, Suggested items are checked, Caution items are unchecked, Clean items are unchecked
        let selected = actual_safety == SafetyLevel::Suggested;
        Self {
            id: id.into(),
            category,
            title: title.into(),
            description: description.into(),
            path,
            size_bytes,
            file_count,
            safety: actual_safety,
            selected,
            subitems: Vec::new(),
        }
    }

    pub fn with_subitems(mut self, subitems: Vec<SubItem>) -> Self {
        if !subitems.is_empty() {
            let total_size: u64 = subitems.iter().map(|s| s.size_bytes).sum();
            let total_files: usize = subitems.iter().map(|s| s.file_count).sum();
            self.size_bytes = total_size;
            self.file_count = total_files;
            if total_size == 0 {
                self.safety = SafetyLevel::Clean;
                self.selected = false;
            }
        }
        self.subitems = subitems;
        self
    }
}

/// Group of clean items belonging to one category.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanGroup {
    pub category: CleanCategory,
    pub title: String,
    pub items: Vec<CleanItem>,
    pub total_size: u64,
    pub selected_size: u64,
}

impl CleanGroup {
    pub fn new(category: CleanCategory, items: Vec<CleanItem>) -> Self {
        let total_size = items.iter().map(|it| it.size_bytes).sum();
        let selected_size = items
            .iter()
            .filter(|it| it.selected)
            .map(|it| it.size_bytes)
            .sum();
        Self {
            category,
            title: category.title().to_string(),
            items,
            total_size,
            selected_size,
        }
    }

    pub fn recalculate(&mut self) {
        self.total_size = self.items.iter().map(|it| it.size_bytes).sum();
        self.selected_size = self
            .items
            .iter()
            .filter(|it| it.selected)
            .map(|it| it.size_bytes)
            .sum();
    }
}

/// Overall scan summary across all categories.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanSummary {
    pub groups: Vec<CleanGroup>,
    pub total_size: u64,
    pub selected_size: u64,
    pub total_items: usize,
    pub selected_items: usize,
    pub scan_duration: Duration,
}

impl ScanSummary {
    pub fn new(groups: Vec<CleanGroup>, scan_duration: Duration) -> Self {
        let mut total_size = 0;
        let mut selected_size = 0;
        let mut total_items = 0;
        let mut selected_items = 0;

        for g in &groups {
            total_size += g.total_size;
            selected_size += g.selected_size;
            total_items += g.items.len();
            selected_items += g.items.iter().filter(|i| i.selected).count();
        }

        Self {
            groups,
            total_size,
            selected_size,
            total_items,
            selected_items,
            scan_duration,
        }
    }

    pub fn get_group(&self, cat: CleanCategory) -> Option<&CleanGroup> {
        self.groups.iter().find(|g| g.category == cat)
    }

    pub fn get_group_mut(&mut self, cat: CleanCategory) -> Option<&mut CleanGroup> {
        self.groups.iter_mut().find(|g| g.category == cat)
    }

    pub fn recalculate(&mut self) {
        self.total_size = 0;
        self.selected_size = 0;
        self.total_items = 0;
        self.selected_items = 0;

        for g in &mut self.groups {
            g.recalculate();
            self.total_size += g.total_size;
            self.selected_size += g.selected_size;
            self.total_items += g.items.len();
            self.selected_items += g.items.iter().filter(|i| i.selected).count();
        }
    }
}

/// Result report from a cleaning operation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CleanReport {
    pub dry_run: bool,
    pub permanent: bool,
    pub reclaimed_bytes: u64,
    pub cleaned_items: usize,
    pub failed_paths: Vec<(PathBuf, String)>,
    pub skipped_paths: Vec<PathBuf>,
    pub duration: Duration,
}

/// Helper function to format bytes in human-readable notation (B, KB, MB, GB, TB).
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    let b = bytes as f64;
    if b < KB {
        format!("{} B", bytes)
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.2} MB", b / MB)
    } else if b < TB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.2} TB", b / TB)
    }
}
