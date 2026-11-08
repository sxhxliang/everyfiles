use clap::{Parser, Subcommand};
use everyfiles_cleaner::{
    AppUninstaller, CleanCategory, CleanGroup, CleanItem, CleanupCoordinator, DeveloperManager,
    DuplicateFinder, SafeDeleter, SpaceLens, SystemMaintenance, format_size,
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
pub struct CleanArgs {
    #[command(subcommand)]
    pub action: Option<CleanAction>,

    /// Launch CleanMyMac-style interactive Terminal UI dashboard.
    #[arg(short, long)]
    pub interactive: bool,
}

#[derive(Subcommand, Debug)]
pub enum CleanAction {
    /// Perform full smart scan and display cleanup recommendations.
    Scan {
        /// Category to filter (system, app, web, dev, large, trash).
        #[arg(short, long)]
        category: Option<String>,
        /// Output report as JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Dedicated developer tools & environments scan and clean (Cargo, npm, Python, Xcode, etc.).
    Dev {
        /// Scan and clean dormant project dependencies (node_modules, target, .venv).
        #[arg(short, long)]
        projects: bool,
        /// Scan bloated Git repositories and run git gc.
        #[arg(short, long)]
        git: bool,
        /// Clean obsolete/unavailable iOS simulators (xcrun simctl).
        #[arg(long)]
        simulators: bool,
        /// Reclaim Docker Desktop disk space (docker system prune).
        #[arg(long)]
        docker: bool,
        /// Minimum idle days for dormant projects (default: 30 days).
        #[arg(long, default_value_t = 30)]
        idle_days: u64,
        /// Custom workspace directory to scan.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Clean developer caches directly.
        #[arg(long)]
        clean: bool,
        /// Dry-run mode (calculate space without deleting).
        #[arg(long)]
        dry_run: bool,
        /// Output report as JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Inspect installed applications and leftover files.
    Apps {
        /// List all installed applications sorted by total footprint (bundle + data).
        #[arg(short, long)]
        list: bool,
        /// Only show uninstalled app leftovers.
        #[arg(long)]
        leftovers: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Deep app uninstaller: cleanly remove application and all associated data.
    Uninstall {
        /// Application name or bundle identifier (e.g. "Zoom", "WeChat", "Alacritty").
        app: String,
        /// Perform dry-run without deleting files.
        #[arg(long)]
        dry_run: bool,
        /// Skip confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Permanently delete files instead of moving to macOS Trash.
        #[arg(long)]
        permanent: bool,
    },
    /// Inspect web junk (Mail, Safari, Chrome, Quark, download logs).
    Web {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect large and old files on disk.
    LargeFiles {
        /// Minimum file size (e.g. 100M, 500M, 1G).
        #[arg(long, default_value = "100M")]
        min_size: String,
        /// Minimum idle age (e.g. 30d, 60d, 90d).
        #[arg(long, default_value = "60d")]
        older_than: String,
    },
    /// Scan for duplicate files using xxHash3 and clean redundant copies.
    Duplicates {
        /// Directory path to scan (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Minimum file size threshold (e.g. 100K, 1M, 10M).
        #[arg(long, default_value = "100K")]
        min_size: String,
        /// Recursion depth limit.
        #[arg(long)]
        max_depth: Option<usize>,
        /// Directly move duplicate copies to macOS Trash (keeping original).
        #[arg(long)]
        clean: bool,
        /// Dry run mode (simulate space savings without moving files).
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// DaisyDisk-style visual disk space lens.
    Space {
        /// Directory path to explore (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output non-interactive summary table instead of launching visual TUI.
        #[arg(long)]
        summary: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Execute system maintenance tasks (RAM purge, DNS flush, Spotlight rebuild).
    Optimize {
        /// Purge inactive RAM disk cache.
        #[arg(long)]
        ram: bool,
        /// Flush system DNS cache.
        #[arg(long)]
        dns: bool,
        /// Rebuild Spotlight search index.
        #[arg(long)]
        spotlight: bool,
    },
    /// Execute cleaning on recommended or selected items.
    Apply {
        /// Category to clean (system, app, web, dev, large, trash). Default: all suggested items.
        #[arg(short, long)]
        category: Option<String>,
        /// Perform dry-run without deleting files.
        #[arg(long)]
        dry_run: bool,
        /// Permanently delete files instead of moving to macOS Trash.
        #[arg(long)]
        permanent: bool,
        /// Skip confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Launch interactive terminal dashboard.
    Ui,
}

pub fn run_clean(args: CleanArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.interactive {
        return launch_tui();
    }

    match args.action {
        None | Some(CleanAction::Ui) => launch_tui(),
        Some(CleanAction::Scan { category, json }) => run_scan(category, json),
        Some(CleanAction::Dev {
            projects,
            git,
            simulators,
            docker,
            idle_days,
            workspace,
            clean,
            dry_run,
            json,
        }) => run_dev(
            projects, git, simulators, docker, idle_days, workspace, clean, dry_run, json,
        ),
        Some(CleanAction::Apps {
            list,
            leftovers,
            json,
        }) => run_apps(list, leftovers, json),
        Some(CleanAction::Uninstall {
            app,
            dry_run,
            yes,
            permanent,
        }) => run_uninstall(app, dry_run, yes, permanent),
        Some(CleanAction::Web { json }) => run_web(json),
        Some(CleanAction::LargeFiles {
            min_size,
            older_than,
        }) => run_large_files(min_size, older_than),
        Some(CleanAction::Duplicates {
            path,
            min_size,
            max_depth,
            clean,
            dry_run,
            json,
        }) => run_duplicates(path, min_size, max_depth, clean, dry_run, json),
        Some(CleanAction::Space {
            path,
            summary,
            json,
        }) => run_space(path, summary, json),
        Some(CleanAction::Optimize {
            ram,
            dns,
            spotlight,
        }) => run_optimize(ram, dns, spotlight),
        Some(CleanAction::Apply {
            category,
            dry_run,
            permanent,
            yes,
        }) => run_apply(category, dry_run, permanent, yes),
    }
}

fn launch_tui() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("正在极速扫描系统中，请稍候...");
    let summary = CleanupCoordinator::scan_all();
    let mut app = everyfiles_cleaner::CleanerApp::new(summary);
    app.run()?;
    Ok(())
}

fn run_scan(category: Option<String>, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let summary = CleanupCoordinator::scan_all();

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    let filter_cat = category.as_deref().and_then(parse_category);

    println!("\n=======================================================");
    println!("        everyfiles Mac 清理管家 | 智能扫描报告");
    println!("=======================================================\n");

    for group in &summary.groups {
        if filter_cat.is_some_and(|fc| group.category != fc) {
            continue;
        }
        print_group(group);
    }

    println!("-------------------------------------------------------");
    println!(
        " 扫描完成 (耗时: {:.2}s)",
        summary.scan_duration.as_secs_f32()
    );
    println!(
        " 发现可清理项: {} 项 | 总大小: {}",
        summary.total_items,
        format_size(summary.total_size)
    );
    println!(
        " 推荐安全清理: {} 项 | 大小: {}",
        summary.selected_items,
        format_size(summary.selected_size)
    );
    println!("-------------------------------------------------------");
    println!(" 提示: ");
    println!("  - 运行 `everyfiles clean ui` 启动 CleanMyMac 终端交互式仪表盘");
    println!("  - 运行 `everyfiles clean apply` 立即将推荐项安全移至废纸篓");
    println!("  - 运行 `everyfiles clean dev` 专项检查开发工具缓存\n");

    Ok(())
}

fn resolve_workspaces(custom: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(p) = custom {
        return vec![p];
    }
    let mut dirs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(home) = dirs::home_dir() {
        for sub in &["Code", "Projects", "Workspace", "Developer", "Desktop"] {
            let p = home.join(sub);
            if p.is_dir() && !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    dirs
}

#[allow(clippy::too_many_arguments)]
fn run_dev(
    projects: bool,
    git: bool,
    simulators: bool,
    docker: bool,
    idle_days: u64,
    workspace: Option<PathBuf>,
    clean: bool,
    dry_run: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let ran_specific = projects || git || simulators || docker;

    if !ran_specific {
        // General Developer tool & environment caches scan (Cargo, npm, pip, go, brew, Xcode, etc.)
        let group = CleanupCoordinator::scan_category(CleanCategory::DevJunk);

        if json {
            println!("{}", serde_json::to_string_pretty(&group)?);
            return Ok(());
        }

        println!("\n💻 开发人员开发工具与环境专项报告");
        println!("=======================================================\n");
        print_group(&group);

        if clean {
            println!("正在清理选中的开发工具缓存...");
            let report = SafeDeleter::execute(&group.items, dry_run, false, |item, cur, tot| {
                println!("  [{cur}/{tot}] 清理: {}", item.title);
            });
            println!(
                "\n开发工具清理完成: 释放空间 {} (耗时: {:.2}s)",
                format_size(report.reclaimed_bytes),
                report.duration.as_secs_f32()
            );
        } else {
            println!(" 💡 开发套件专项功能推荐: ");
            println!("  - `everyfiles clean dev --clean`         : 立即清理上述开发工具缓存");
            println!(
                "  - `everyfiles clean dev --projects`      : 排查闲置工程依赖 (node_modules/target/.venv)"
            );
            println!(
                "  - `everyfiles clean dev --git`           : 排查膨胀 Git 仓库并执行 gc 压缩"
            );
            println!(
                "  - `everyfiles clean dev --simulators`    : 自动清理失效的 Xcode iOS 模拟器"
            );
            println!(
                "  - `everyfiles clean dev --docker`        : 回收 Docker 镜像、无用容器与卷缓存\n"
            );
        }
        return Ok(());
    }

    let workspaces = resolve_workspaces(workspace);

    // 1. Dormant Project Artifacts
    if projects {
        println!("正在扫描工作区闲置依赖产物 (闲置天数 >= {idle_days} 天)...");
        let artifacts =
            DeveloperManager::scan_dormant_artifacts(&workspaces, idle_days, 1024 * 1024);

        if json {
            println!("{}", serde_json::to_string_pretty(&artifacts)?);
        } else {
            println!(
                "\n📦 闲置工程依赖与构建产物排查报告 (阈值: {} 天)",
                idle_days
            );
            println!("=======================================================\n");

            if artifacts.is_empty() {
                println!(
                    "  ✨ 赞！未发现超过 {} 天未修改的闲置构建产物或依赖目录。\n",
                    idle_days
                );
            } else {
                let total_size: u64 = artifacts.iter().map(|a| a.size_bytes).sum();
                let total_files: usize = artifacts.iter().map(|a| a.file_count).sum();

                for (i, art) in artifacts.iter().enumerate() {
                    println!(
                        "{:>2}. [{}] 闲置: {:>3} 天 | 大小: {:>9} ({} 文件)",
                        i + 1,
                        art.project_name,
                        art.idle_days,
                        format_size(art.size_bytes),
                        art.file_count
                    );
                    println!(
                        "    类型: {} | 路径: {}",
                        art.artifact_kind.label(),
                        art.path.display()
                    );
                }

                println!("\n-------------------------------------------------------");
                println!(
                    " 发现闲置依赖/产物: {} 项 | 包含文件: {} 个 | 总占用: {}",
                    artifacts.len(),
                    total_files,
                    format_size(total_size)
                );
                println!("-------------------------------------------------------");

                if clean {
                    if dry_run {
                        println!(" [试运行] 预计可清理释放: {}", format_size(total_size));
                    } else {
                        println!("正在将闲置依赖安全移至 macOS 废纸篓...");
                        let (reclaimed, count) =
                            DeveloperManager::clean_artifacts(&artifacts, false, false);
                        println!(
                            " 清理完成: 已将 {} 个工程产物移至废纸篓，成功释放空间: {}",
                            count,
                            format_size(reclaimed)
                        );
                    }
                } else {
                    println!(
                        " 提示: 运行 `everyfiles clean dev --projects --clean` 立即安全清理上述闲置依赖。\n"
                    );
                }
            }
        }
    }

    // 2. Git Repositories Optimization
    if git {
        println!("正在排查本地 Git 仓库存储与膨胀情况...");
        let repos = DeveloperManager::scan_git_repositories(&workspaces);

        if json {
            println!("{}", serde_json::to_string_pretty(&repos)?);
        } else {
            println!("\n🐙 本地 Git 仓库存储排查报告");
            println!("=======================================================\n");

            if repos.is_empty() {
                println!("  未在工作区中检索到 Git 仓库。\n");
            } else {
                let total_git_size: u64 = repos.iter().map(|r| r.git_dir_size).sum();

                for (i, repo) in repos.iter().enumerate() {
                    let bloat_tag = if repo.is_bloated {
                        " ⚠️ [膨胀建议gc]"
                    } else {
                        " [正常]"
                    };
                    println!(
                        "{:>2}. {:<24} .git 大小: {:>9} (松散对象: {}){}",
                        i + 1,
                        repo.name,
                        format_size(repo.git_dir_size),
                        format_size(repo.loose_objects_size),
                        bloat_tag
                    );
                    println!("    路径: {}", repo.path.display());
                }

                println!("\n-------------------------------------------------------");
                println!(
                    " 检索到 Git 仓库: {} 个 | .git 目录总占用: {}",
                    repos.len(),
                    format_size(total_git_size)
                );
                println!("-------------------------------------------------------");

                if clean {
                    println!("正在对膨胀仓库执行 git gc --prune=now --aggressive 压缩...");
                    let mut total_reclaimed = 0u64;
                    for repo in repos.iter().filter(|r| r.is_bloated) {
                        match DeveloperManager::optimize_git_repo(&repo.path) {
                            Ok(reclaimed) => {
                                println!(
                                    "  [优化完成] {}: 压缩释放 {}",
                                    repo.name,
                                    format_size(reclaimed)
                                );
                                total_reclaimed += reclaimed;
                            }
                            Err(e) => {
                                eprintln!("  [优化失败] {}: {e}", repo.name);
                            }
                        }
                    }
                    println!(
                        "\nGit 仓库压缩完成: 共释放空间: {}",
                        format_size(total_reclaimed)
                    );
                } else {
                    println!(
                        " 提示: 运行 `everyfiles clean dev --git --clean` 立即对膨胀仓库执行压缩。\n"
                    );
                }
            }
        }
    }

    // 3. iOS Simulators Cleanup
    if simulators {
        println!("\n📱 排查并清理不可用的 Xcode iOS 模拟器...");
        match DeveloperManager::clean_unavailable_simulators() {
            Ok(msg) => println!("  ✅ {msg}"),
            Err(e) => eprintln!("  ❌ 清理失败: {e}"),
        }
    }

    // 4. Docker Reclaim
    if docker {
        println!("\n🐳 正在排查并执行 Docker / 容器环境空间回收...");
        match DeveloperManager::docker_system_prune(true, true) {
            Ok(msg) => {
                println!("  ✅ Docker 回收完成:\n{msg}");
            }
            Err(e) => eprintln!("  ⚠️  {e}"),
        }
    }

    Ok(())
}

fn run_apps(
    list: bool,
    leftovers_only: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if list {
        println!("正在扫描系统中已安装的应用程序及其全套数据占用...");
        let apps = AppUninstaller::scan_all_apps();

        if json {
            println!("{}", serde_json::to_string_pretty(&apps)?);
            return Ok(());
        }

        println!("\n=======================================================");
        println!("      macOS 已安装应用程序资产全景 (按全套占用降序)");
        println!("=======================================================\n");

        for (i, app) in apps.iter().take(30).enumerate() {
            let sys_tag = if app.is_system { " [系统保护]" } else { "" };
            let app_size = app
                .associated_files
                .first()
                .map(|f| format_size(f.size_bytes))
                .unwrap_or_else(|| "0 B".into());

            println!(
                "{:>2}. {:<24} (版本: {:<8}) {sys_tag}",
                i + 1,
                app.display_name,
                if app.version.is_empty() {
                    "未知"
                } else {
                    &app.version
                }
            );
            println!(
                "    全套占用: {:>10}  (本体: {:>9}) | Bundle: {}",
                format_size(app.total_size_bytes),
                app_size,
                app.bundle_id
            );
        }

        println!("\n-------------------------------------------------------");
        println!(" 共扫描到 {} 款应用程序", apps.len());
        println!(
            " 提示: 运行 `everyfiles clean uninstall <应用名>` 可深度卸载任意应用并连根拔起其全套数据！\n"
        );
        return Ok(());
    }

    let group = CleanupCoordinator::scan_category(CleanCategory::AppJunk);

    if json {
        println!("{}", serde_json::to_string_pretty(&group)?);
        return Ok(());
    }

    println!("\n📦 应用程序垃圾与卸载残留报告");
    println!("=======================================================\n");

    for item in &group.items {
        if leftovers_only && item.id != "app_leftovers" {
            continue;
        }
        print_item(item, "  ");
    }

    println!("\n总计应用垃圾: {}", format_size(group.total_size));
    println!("提示: 运行 `everyfiles clean apps --list` 可查看所有已安装应用的完整占用！\n");
    Ok(())
}

fn run_uninstall(
    app_query: String,
    dry_run: bool,
    yes: bool,
    permanent: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("正在全盘检索应用程序: \"{app_query}\"...");
    let matches = AppUninstaller::find_app(&app_query);

    if matches.is_empty() {
        println!("未找到名称或 Bundle ID 匹配 \"{app_query}\" 的已安装应用程序。");
        return Ok(());
    }

    let app = if matches.len() == 1 {
        &matches[0]
    } else {
        println!("\n找到多款名称相近的应用程序，请确认:");
        for (i, a) in matches.iter().enumerate() {
            println!(
                "  [{}] {} ({}) - {}",
                i + 1,
                a.display_name,
                a.bundle_id,
                a.bundle_path.display()
            );
        }
        &matches[0]
    };

    if app.is_system {
        println!(
            "\n⚠️  {} 是 macOS 系统预装核心应用，受到系统完整性保护 (SIP)，禁止卸载！",
            app.display_name
        );
        return Ok(());
    }

    println!("\n=======================================================");
    println!(
        "       深度应用卸载清单: {} (版本: {})",
        app.display_name, app.version
    );
    println!("=======================================================");
    println!(" 应用标识: {}", app.bundle_id);
    println!(" 全套占用: {}", format_size(app.total_size_bytes));
    println!(" 关联文件明细:\n");

    for (i, f) in app.associated_files.iter().enumerate() {
        let check = if f.selected { "[x]" } else { "[ ]" };
        println!(
            "  {check} {:>2}. {:<32} {:>10}  ({} 项)",
            i + 1,
            f.category_name,
            format_size(f.size_bytes),
            f.file_count
        );
        println!("       路径: {}", f.path.display());
    }

    println!("\n-------------------------------------------------------");
    println!(
        " 卸载模式: {}",
        if permanent {
            "彻底抹除 (不可逆)"
        } else {
            "移至系统废纸篓 (安全可放回)"
        }
    );
    if dry_run {
        println!(" 模拟运行: 是 (Dry-Run, 不会真正修改任何文件)");
    }
    println!("-------------------------------------------------------\n");

    if !yes && !dry_run {
        print!(
            "确认连根拔起并卸载 \"{}\" 的上述全部数据吗？ [y/N]: ",
            app.display_name
        );
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("已取消卸载操作。");
            return Ok(());
        }
    }

    println!("正在卸载应用并清理全套关联文件...");
    let report = AppUninstaller::uninstall(app, dry_run, permanent);

    if dry_run {
        println!(
            "\n模拟卸载完成: 预计释放空间 {}",
            format_size(report.reclaimed_bytes)
        );
    } else {
        println!(
            "\n✨ 卸载成功！已将 {} 项文件移至废纸篓，共释放空间 {}",
            report.cleaned_items,
            format_size(report.reclaimed_bytes)
        );
    }

    if !report.failed_paths.is_empty() {
        println!(
            " 警告: 共有 {} 项未能移除 (可能需要更高权限):",
            report.failed_paths.len()
        );
        for (p, err) in &report.failed_paths {
            println!("   - {}: {err}", p.display());
        }
    }

    Ok(())
}

fn run_web(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let group = CleanupCoordinator::scan_category(CleanCategory::WebJunk);

    if json {
        println!("{}", serde_json::to_string_pretty(&group)?);
        return Ok(());
    }

    println!("\n🌐 上网垃圾清理报告");
    println!("=======================================================\n");
    print_group(&group);
    Ok(())
}

fn run_large_files(
    min_size_str: String,
    _older_than_str: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let min_bytes = parse_size_bytes(&min_size_str).unwrap_or(1024 * 1024 * 100);
    let items = everyfiles_cleaner::scanners::large_files::scan_large_old_files_with_criteria(
        min_bytes,
        std::time::Duration::from_secs(86400 * 30),
    );

    println!("\n📁 大文件与旧文件扫描报告 (阈值: > {min_size_str})");
    println!("=======================================================\n");

    for item in &items {
        for sub in &item.subitems {
            println!("  - {:<40} {:>10}", sub.name, format_size(sub.size_bytes));
            println!("    路径: {}", sub.path.display());
        }
    }

    Ok(())
}

fn run_duplicates(
    path: PathBuf,
    min_size_str: String,
    max_depth: Option<usize>,
    clean: bool,
    dry_run: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let min_bytes = parse_size_bytes(&min_size_str).unwrap_or(1024 * 100);
    println!(
        "正在扫描重复文件: {} (阈值: > {min_size_str})...",
        path.display()
    );
    let report = DuplicateFinder::find(&[path], min_bytes, max_depth);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("\n=======================================================");
    println!("         重复文件查找报告 (xxHash3 极速比对)");
    println!("=======================================================\n");

    if report.groups.is_empty() {
        println!("✨ 未发现任何重复文件！磁盘非常整洁。");
        return Ok(());
    }

    for (idx, group) in report.groups.iter().enumerate() {
        println!(
            "[{}] 重复文件组 (单文件大小: {}, 浪费空间: {})",
            idx + 1,
            format_size(group.file_size),
            format_size(group.wasted_bytes)
        );
        for f in &group.files {
            let role = if f.is_original {
                "🟢 [保留原件]"
            } else {
                "🔴 [建议清理]"
            };
            println!("    {role} {}", f.path.display());
        }
        println!();
    }

    println!("-------------------------------------------------------");
    println!(" 扫描耗时: {:.2}s", report.scan_duration.as_secs_f32());
    println!(
        " 发现重复文件: {} 组共 {} 个副本 | 浪费磁盘空间: {}",
        report.groups.len(),
        report.total_duplicate_files,
        format_size(report.total_wasted_bytes)
    );
    println!("-------------------------------------------------------");

    if clean {
        println!("正在清理重复副本 (移至系统废纸篓)...");
        let (reclaimed, count) = DuplicateFinder::clean(&report.groups, dry_run, false);
        if dry_run {
            println!(
                " 模拟运行完成: 预计可清理 {} 个重复文件，释放空间 {}",
                count,
                format_size(reclaimed)
            );
        } else {
            println!(
                " 清理完成: 已安全清理 {} 个重复文件，释放空间 {}",
                count,
                format_size(reclaimed)
            );
        }
    } else {
        println!(
            " 提示: 运行 `everyfiles clean duplicates --clean` 即可将上述标记副本安全移入废纸篓。\n"
        );
    }

    Ok(())
}

fn run_space(path: PathBuf, summary: bool, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let target_path = if path.as_os_str().is_empty() || path == Path::new(".") {
        std::env::current_dir().unwrap_or(path)
    } else {
        path
    };

    if !summary && !json {
        SpaceLens::launch_interactive(target_path)?;
        return Ok(());
    }

    let (total_size, nodes) = SpaceLens::explore(&target_path);

    if json {
        let out = serde_json::json!({
            "path": target_path,
            "total_size": total_size,
            "nodes": nodes,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n=======================================================");
    println!("        磁盘空间透视器 (Space Lens - 排行)");
    println!("=======================================================");
    println!(" 路径: {}", target_path.display());
    println!(" 当前层级总体积: {}\n", format_size(total_size));

    for (i, node) in nodes.iter().take(20).enumerate() {
        let icon = if node.is_dir { "📁" } else { "📄" };
        let bar = SpaceLens::render_bar(node.percent, 16);
        println!(
            " {:>2}. {} {:<32} {} {:>5.1}% {:>10}  ({} 项)",
            i + 1,
            icon,
            node.name,
            bar,
            node.percent,
            format_size(node.size_bytes),
            node.file_count
        );
    }
    println!(
        "\n提示: 运行 `everyfiles clean space` 可启动 DaisyDisk 风格交互式可视化视图，支持回车下钻浏览！\n"
    );

    Ok(())
}

fn run_optimize(ram: bool, dns: bool, spotlight: bool) -> Result<(), Box<dyn std::error::Error>> {
    let all = !ram && !dns && !spotlight;

    println!("\n⚡ 正在执行 macOS 系统性能维护与优化任务...\n");

    if ram || all {
        print!("  - 正在释放 RAM 内存缓存 (purge)... ");
        io::stdout().flush()?;
        match SystemMaintenance::free_ram() {
            Ok(msg) => println!("✓ {msg}"),
            Err(e) => println!("✗ {e}"),
        }
    }

    if dns || all {
        print!("  - 正在刷新系统 DNS 缓存... ");
        io::stdout().flush()?;
        match SystemMaintenance::flush_dns() {
            Ok(msg) => println!("✓ {msg}"),
            Err(e) => println!("✗ {e}"),
        }
    }

    if spotlight {
        print!("  - 正在触发 Spotlight 索引重建... ");
        io::stdout().flush()?;
        match SystemMaintenance::rebuild_spotlight(None) {
            Ok(msg) => println!("✓ {msg}"),
            Err(e) => println!("✗ {e}"),
        }
    }

    println!("\n系统维护任务执行完毕！\n");
    Ok(())
}

fn run_apply(
    category: Option<String>,
    dry_run: bool,
    permanent: bool,
    yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let filter_cat = category.as_deref().and_then(parse_category);
    let summary = CleanupCoordinator::scan_all();

    let mut to_clean = Vec::new();
    for g in &summary.groups {
        if filter_cat.is_some_and(|fc| g.category != fc) {
            continue;
        }
        for it in &g.items {
            if it.selected {
                to_clean.push(it.clone());
            }
        }
    }

    let target_size: u64 = to_clean.iter().map(|it| it.size_bytes).sum();
    if target_size == 0 {
        println!("没有需要清理的选定项目。");
        return Ok(());
    }

    println!("\n=======================================================");
    println!("                 准备执行系统清理");
    println!("=======================================================");
    println!(" 选定待清理项: {} 项", to_clean.len());
    println!(" 预计释放空间: {}", format_size(target_size));
    println!(
        " 清理动作模式: {}",
        if permanent {
            "彻底抹除 (不可逆)"
        } else {
            "移至系统废纸篓 (安全可放回)"
        }
    );
    if dry_run {
        println!(" 模拟运行模式: 是 (Dry-Run, 不会真正修改任何文件)");
    }
    println!("=======================================================\n");

    if !yes && !dry_run {
        print!("确认继续吗？ [y/N]: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("已取消操作。");
            return Ok(());
        }
    }

    println!("正在执行清理...");
    let report = SafeDeleter::execute(&to_clean, dry_run, permanent, |item, cur, tot| {
        println!("  [{cur}/{tot}] 处理: {}", item.title);
    });

    println!("\n-------------------------------------------------------");
    if dry_run {
        println!(
            " 模拟清理完成: 预计可释放空间 {}",
            format_size(report.reclaimed_bytes)
        );
    } else {
        println!(
            " 清理成功！已清理 {} 项，共释放空间 {}",
            report.cleaned_items,
            format_size(report.reclaimed_bytes)
        );
    }
    if !report.failed_paths.is_empty() {
        println!(
            " 警告: 共有 {} 项未能移除 (可能需要更高权限):",
            report.failed_paths.len()
        );
        for (p, err) in &report.failed_paths {
            println!("   - {}: {err}", p.display());
        }
    }
    println!("-------------------------------------------------------\n");

    Ok(())
}

fn print_group(group: &CleanGroup) {
    println!(
        "{} {} (共 {}, 已选择 {})",
        group.category.icon(),
        group.category.title(),
        format_size(group.total_size),
        format_size(group.selected_size)
    );
    for item in &group.items {
        print_item(item, "  ");
    }
    println!();
}

fn print_item(item: &CleanItem, indent: &str) {
    let check = if item.selected { "[x]" } else { "[ ]" };
    let size_badge = if item.size_bytes == 0 {
        "很干净".to_string()
    } else {
        format!(
            "共 {}, {}",
            format_size(item.size_bytes),
            item.safety.label()
        )
    };

    println!(
        "{indent}{check} {:<24} {:<22} - {}",
        item.title, size_badge, item.description
    );

    for sub in &item.subitems {
        if sub.size_bytes > 0 {
            println!(
                "{indent}    - {:<36} {:>10}",
                sub.name,
                format_size(sub.size_bytes)
            );
        }
    }
}

fn parse_category(name: &str) -> Option<CleanCategory> {
    match name.to_ascii_lowercase().as_str() {
        "system" | "sys" | "系统" => Some(CleanCategory::SystemJunk),
        "app" | "apps" | "应用" => Some(CleanCategory::AppJunk),
        "web" | "net" | "上网" => Some(CleanCategory::WebJunk),
        "dev" | "developer" | "开发" => Some(CleanCategory::DevJunk),
        "large" | "old" | "大文件" => Some(CleanCategory::LargeOldFiles),
        "trash" | "废纸篓" => Some(CleanCategory::Trash),
        _ => None,
    }
}

fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_part, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let num: f64 = num_part.parse().ok()?;
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "K" | "KB" => 1024.0,
        "M" | "MB" => 1024.0 * 1024.0,
        "G" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    Some((num * mult) as u64)
}
