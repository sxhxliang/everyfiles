use crate::domain::{CleanCategory, CleanItem, SafetyLevel, SubItem};
use crate::scanners::dir_size_and_count;
use std::fs;
use std::path::Path;

/// Comprehensive developer tools & environment scanner.
pub fn scan_dev_junk() -> Vec<CleanItem> {
    let mut items = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return items,
    };

    // 1. Rust / Cargo
    items.push(scan_cargo(&home));

    // 2. Node.js / 前端包管理器 (npm / yarn / pnpm / bun)
    items.push(scan_node_frontend(&home));

    // 3. Python 环境与包缓存 (pip / uv / poetry / conda / pyenv)
    items.push(scan_python(&home));

    // 4. Go (Golang) 模块与编译缓存
    items.push(scan_golang(&home));

    // 5. Java / Gradle / Maven / Android
    items.push(scan_jvm_android(&home));

    // 6. Homebrew 包管理器缓存
    items.push(scan_homebrew(&home));

    // 7. JetBrains IDE 历史缓存
    items.push(scan_jetbrains(&home));

    // 8. Docker 磁盘空间分析
    if let Some(docker_item) = scan_docker(&home) {
        items.push(docker_item);
    }

    // 9. 闲置项目构建产物 (target / node_modules)
    if let Some(project_item) = scan_project_artifacts(&home) {
        items.push(project_item);
    }

    items
}

fn scan_cargo(home: &Path) -> CleanItem {
    let cargo_home = home.join(".cargo");
    let mut subs = Vec::new();

    let targets = [
        (
            "Cargo crate 压缩包缓存 (registry/cache)",
            cargo_home.join("registry/cache"),
        ),
        (
            "Cargo crate 源码解压树 (registry/src)",
            cargo_home.join("registry/src"),
        ),
        (
            "Cargo Git 依赖检出 (git/checkouts)",
            cargo_home.join("git/checkouts"),
        ),
        ("Cargo Git 数据库 (git/db)", cargo_home.join("git/db")),
    ];

    for (name, path) in targets {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                subs.push(SubItem {
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "dev_cargo",
        CleanCategory::DevJunk,
        "Rust / Cargo 缓存",
        "Cargo 下载的 .crate 归档包、解压源码树及 Git 依赖库 (Cargo 重新构建时会自动拉取)",
        cargo_home,
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_node_frontend(home: &Path) -> CleanItem {
    let mut subs = Vec::new();

    let targets = [
        (
            "npm 全局内容寻址缓存 (_cacache)",
            home.join(".npm/_cacache"),
        ),
        (
            "Yarn 离线包缓存 (Library/Caches/Yarn)",
            home.join("Library/Caches/Yarn"),
        ),
        ("Yarn 缓存 (~/.cache/yarn)", home.join(".cache/yarn")),
        (
            "pnpm 全局存储 (Library/pnpm/store)",
            home.join("Library/pnpm/store"),
        ),
        (
            "pnpm 全局存储 (~/.local/share/pnpm/store)",
            home.join(".local/share/pnpm/store"),
        ),
        (
            "Bun 安装包缓存 (~/.bun/install/cache)",
            home.join(".bun/install/cache"),
        ),
    ];

    for (name, path) in targets {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                subs.push(SubItem {
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "dev_node_frontend",
        CleanCategory::DevJunk,
        "Node.js / 前端包缓存",
        "npm、yarn、pnpm、bun 全局下载的依赖包离线缓存",
        home.join(".npm"),
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_python(home: &Path) -> CleanItem {
    let mut subs = Vec::new();

    let targets = [
        (
            "pip 轮子与源码缓存 (Library/Caches/pip)",
            home.join("Library/Caches/pip"),
        ),
        ("pip 缓存 (~/.cache/pip)", home.join(".cache/pip")),
        (
            "uv 高性能包缓存 (Library/Caches/uv)",
            home.join("Library/Caches/uv"),
        ),
        ("uv 缓存 (~/.cache/uv)", home.join(".cache/uv")),
        (
            "Poetry 缓存 (Library/Caches/pypoetry)",
            home.join("Library/Caches/pypoetry"),
        ),
        (
            "Pipenv 虚拟环境 (~/.local/share/virtualenvs)",
            home.join(".local/share/virtualenvs"),
        ),
        (
            "Miniconda 包缓存 (~/miniconda3/pkgs)",
            home.join("miniconda3/pkgs"),
        ),
        (
            "Anaconda 包缓存 (~/anaconda3/pkgs)",
            home.join("anaconda3/pkgs"),
        ),
        ("Conda 全局包缓存 (~/.conda/pkgs)", home.join(".conda/pkgs")),
        (
            "pyenv 构建源码缓存 (~/.pyenv/sources)",
            home.join(".pyenv/sources"),
        ),
        ("pyenv 下载归档 (~/.pyenv/cache)", home.join(".pyenv/cache")),
    ];

    for (name, path) in targets {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                subs.push(SubItem {
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "dev_python",
        CleanCategory::DevJunk,
        "Python 环境与包缓存",
        "pip / uv / poetry 轮子缓存、conda 安装包归档与 pyenv 构建缓存",
        home.join(".cache/pip"),
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_golang(home: &Path) -> CleanItem {
    let mut subs = Vec::new();

    let targets = [
        (
            "Go 模块压缩包缓存 (~/go/pkg/mod/cache)",
            home.join("go/pkg/mod/cache"),
        ),
        (
            "Go 编译输出缓存 (Library/Caches/go-build)",
            home.join("Library/Caches/go-build"),
        ),
    ];

    for (name, path) in targets {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                subs.push(SubItem {
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "dev_golang",
        CleanCategory::DevJunk,
        "Go 语言缓存",
        "Go 模块下载缓存与 go build 编译中间件",
        home.join("go"),
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_jvm_android(home: &Path) -> CleanItem {
    let mut subs = Vec::new();

    let targets = [
        (
            "Gradle 构建任务缓存 (~/.gradle/caches)",
            home.join(".gradle/caches"),
        ),
        (
            "Gradle 守护进程日志 (~/.gradle/daemon)",
            home.join(".gradle/daemon"),
        ),
        (
            "Gradle Wrapper 发行包 (~/.gradle/wrapper/dists)",
            home.join(".gradle/wrapper/dists"),
        ),
        (
            "Maven 本地依赖库 (~/.m2/repository)",
            home.join(".m2/repository"),
        ),
        (
            "Android 模拟器镜像 (Library/Android/sdk/system-images)",
            home.join("Library/Android/sdk/system-images"),
        ),
    ];

    for (name, path) in targets {
        if path.exists() {
            let (sz, count) = dir_size_and_count(&path);
            if sz > 0 {
                subs.push(SubItem {
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

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    CleanItem::new(
        "dev_jvm_android",
        CleanCategory::DevJunk,
        "Java / Gradle / Android 缓存",
        "Gradle 构建缓存、多版本 Wrapper 分发包与历史依赖",
        home.join(".gradle"),
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_homebrew(home: &Path) -> CleanItem {
    let brew_cache = home.join("Library/Caches/Homebrew");
    let (sz, count) = if brew_cache.exists() {
        dir_size_and_count(&brew_cache)
    } else {
        (0, 0)
    };

    CleanItem::new(
        "dev_homebrew",
        CleanCategory::DevJunk,
        "Homebrew 下载缓存",
        "brew install 下载的 bottle 二进制包与源码归档 (清理后不影响已安装软件)",
        brew_cache,
        sz,
        count,
        SafetyLevel::Suggested,
    )
}

fn scan_jetbrains(home: &Path) -> CleanItem {
    let jb_cache = home.join("Library/Caches/JetBrains");
    let mut subs = Vec::new();

    if jb_cache.exists() {
        if let Ok(entries) = fs::read_dir(&jb_cache) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let (sz, count) = dir_size_and_count(&p);
                    if sz > 1024 * 1024 * 10 {
                        // > 10MB
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        subs.push(SubItem {
                            name: format!("JetBrains 产品缓存: {name}"),
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
        "dev_jetbrains",
        CleanCategory::DevJunk,
        "JetBrains IDE 历史缓存",
        "IntelliJ, PyCharm, RustRover, WebStorm 等旧版本索引与日志残留",
        jb_cache,
        total_sz,
        total_count,
        SafetyLevel::Suggested,
    )
    .with_subitems(subs)
}

fn scan_docker(home: &Path) -> Option<CleanItem> {
    let docker_vm_candidates = [
        home.join("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"),
        home.join("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw"),
        home.join("Library/Containers/com.docker.docker/Data/vms"),
    ];

    for p in &docker_vm_candidates {
        if p.exists() {
            let (sz, count) = dir_size_and_count(p);
            if sz > 0 {
                return Some(CleanItem::new(
                    "dev_docker",
                    CleanCategory::DevJunk,
                    "Docker 虚拟机磁盘映像",
                    "Docker Desktop 容器运行底座虚拟磁盘 (提示: 可运行 docker system prune 释放内部空间)",
                    p.clone(),
                    sz,
                    count,
                    SafetyLevel::Caution,
                ));
            }
        }
    }
    None
}

fn scan_project_artifacts(home: &Path) -> Option<CleanItem> {
    let workspace_roots = [
        home.join("Code"),
        home.join("Projects"),
        home.join("Workspace"),
        home.join("Desktop"),
    ];

    let mut subs = Vec::new();

    for root in &workspace_roots {
        if !root.exists() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    // Check for Rust target/
                    let target_dir = p.join("target");
                    if target_dir.exists() && target_dir.join("CACHEDIR.TAG").exists() {
                        let (sz, count) = dir_size_and_count(&target_dir);
                        if sz > 1024 * 1024 * 50 {
                            // > 50MB
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            subs.push(SubItem {
                                name: format!("Rust 项目构建产物: {name}/target"),
                                path: target_dir,
                                size_bytes: sz,
                                file_count: count,
                                safety: SafetyLevel::Caution,
                                selected: false,
                            });
                        }
                    }

                    // Check for node_modules
                    let nm_dir = p.join("node_modules");
                    if nm_dir.exists() {
                        let (sz, count) = dir_size_and_count(&nm_dir);
                        if sz > 1024 * 1024 * 50 {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            subs.push(SubItem {
                                name: format!("前端依赖目录: {name}/node_modules"),
                                path: nm_dir,
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

    if subs.is_empty() {
        return None;
    }

    let (total_sz, total_count) = (
        subs.iter().map(|s| s.size_bytes).sum(),
        subs.iter().map(|s| s.file_count).sum(),
    );

    Some(
        CleanItem::new(
            "dev_project_artifacts",
            CleanCategory::DevJunk,
            "项目构建产物 (target / node_modules)",
            "开发项目下的本地构建输出，再次编译时会自动重新生成",
            home.join("Code"),
            total_sz,
            total_count,
            SafetyLevel::Caution,
        )
        .with_subitems(subs),
    )
}
