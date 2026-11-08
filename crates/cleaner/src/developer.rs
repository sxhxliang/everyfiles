use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::deleter::SafeDeleter;
use crate::guard::SafetyGuard;
use crate::scanners::dir_size_and_count;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    NodeModules,
    RustTarget,
    PythonVenv,
    PyCache,
    GradleBuild,
    CocoaPods,
    FlutterTool,
}

impl ArtifactKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NodeModules => "前端依赖 (node_modules)",
            Self::RustTarget => "Rust 构建产物 (target)",
            Self::PythonVenv => "Python 虚拟环境 (.venv/venv)",
            Self::PyCache => "Python 字节码缓存 (__pycache__)",
            Self::GradleBuild => "Gradle 构建输出 (build)",
            Self::CocoaPods => "iOS CocoaPods 依赖 (Pods)",
            Self::FlutterTool => "Flutter 工具缓存 (.dart_tool)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectArtifact {
    pub project_name: String,
    pub project_path: PathBuf,
    pub artifact_kind: ArtifactKind,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_count: usize,
    pub idle_days: u64,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitRepoInfo {
    pub name: String,
    pub path: PathBuf,
    pub git_dir_size: u64,
    pub loose_objects_size: u64,
    pub is_bloated: bool,
}

pub struct DeveloperManager;

impl DeveloperManager {
    /// Scan workspaces for dormant project dependencies and build outputs.
    pub fn scan_dormant_artifacts(
        workspace_roots: &[PathBuf],
        min_idle_days: u64,
        min_size: u64,
    ) -> Vec<ProjectArtifact> {
        let now = SystemTime::now();
        let min_idle_duration = Duration::from_secs(min_idle_days * 86400);

        let mut candidate_artifacts = Vec::new();

        for root in workspace_roots {
            if !root.exists() {
                continue;
            }

            // Walk directory up to depth 4 to locate project roots
            for entry in WalkDir::new(root)
                .max_depth(4)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_dir() {
                    continue;
                }

                let dir_path = entry.into_path();
                let dir_name = dir_path.file_name().unwrap_or_default().to_string_lossy();

                // Skip inside already recognized target/build/node_modules
                if dir_name == "node_modules"
                    || dir_name == "target"
                    || dir_name == ".git"
                    || dir_name == ".venv"
                {
                    continue;
                }

                // Check for artifacts inside this project dir
                check_project_dir(&dir_path, &now, min_idle_duration, &mut candidate_artifacts);
            }
        }

        // Calculate sizes in parallel
        let mut results: Vec<ProjectArtifact> = candidate_artifacts
            .into_par_iter()
            .filter_map(|(p_name, p_path, kind, art_path, idle_days)| {
                if !art_path.exists() || !SafetyGuard::is_safe(&art_path) {
                    return None;
                }
                let (size_bytes, file_count) = dir_size_and_count(&art_path);
                if size_bytes >= min_size {
                    Some(ProjectArtifact {
                        project_name: p_name,
                        project_path: p_path,
                        artifact_kind: kind,
                        path: art_path,
                        size_bytes,
                        file_count,
                        idle_days,
                        selected: true,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort descending by size
        results.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
        results
    }

    /// Scan workspaces for Git repositories with bloated object stores.
    pub fn scan_git_repositories(workspace_roots: &[PathBuf]) -> Vec<GitRepoInfo> {
        let mut repos = Vec::new();

        for root in workspace_roots {
            if !root.exists() {
                continue;
            }

            for entry in WalkDir::new(root)
                .max_depth(4)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_dir() && entry.file_name().to_string_lossy() == ".git" {
                    let git_path = entry.into_path();
                    let repo_root = git_path.parent().unwrap_or(&git_path).to_path_buf();
                    let repo_name = repo_root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    let (git_size, _) = dir_size_and_count(&git_path);
                    let loose_path = git_path.join("objects");
                    let (loose_size, _) = if loose_path.exists() {
                        dir_size_and_count(&loose_path)
                    } else {
                        (0, 0)
                    };

                    let is_bloated = git_size > 1024 * 1024 * 50; // > 50MB

                    repos.push(GitRepoInfo {
                        name: repo_name,
                        path: repo_root,
                        git_dir_size: git_size,
                        loose_objects_size: loose_size,
                        is_bloated,
                    });
                }
            }
        }

        repos.sort_by_key(|r| std::cmp::Reverse(r.git_dir_size));
        repos
    }

    /// Perform Git garbage collection and repack to shrink .git directory size.
    pub fn optimize_git_repo(repo_path: &Path) -> Result<u64, String> {
        let before_size = dir_size_and_count(&repo_path.join(".git")).0;

        let out = Command::new("git")
            .current_dir(repo_path)
            .args(["gc", "--prune=now", "--aggressive"])
            .output()
            .map_err(|e| format!("Failed to execute git gc: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git gc returned error: {err}"));
        }

        let after_size = dir_size_and_count(&repo_path.join(".git")).0;
        let reclaimed = before_size.saturating_sub(after_size);
        Ok(reclaimed)
    }

    /// Delete unavailable iOS simulators via Xcode xcrun.
    pub fn clean_unavailable_simulators() -> Result<String, String> {
        let out = Command::new("xcrun")
            .args(["simctl", "delete", "unavailable"])
            .output()
            .map_err(|e| format!("Failed to run xcrun simctl: {e}"))?;

        if out.status.success() {
            Ok("已成功清理全部失效和不可用的 iOS 模拟器实例。".to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("xcrun simctl failed: {err}"))
        }
    }

    /// Run Docker deep prune to free unused containers, dangling images, and build caches.
    pub fn docker_system_prune(all: bool, volumes: bool) -> Result<String, String> {
        let mut cmd = Command::new("docker");
        cmd.args(["system", "prune", "-f"]);
        if all {
            cmd.arg("--all");
        }
        if volumes {
            cmd.arg("--volumes");
        }

        let out = cmd
            .output()
            .map_err(|e| format!("Failed to execute docker: {e}. (Is Docker running?)"))?;

        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!("docker system prune failed: {stderr}"))
        }
    }

    /// Clean selected project artifacts into macOS Trash.
    pub fn clean_artifacts(
        artifacts: &[ProjectArtifact],
        dry_run: bool,
        permanent: bool,
    ) -> (u64, usize) {
        let mut reclaimed = 0u64;
        let mut count = 0usize;

        for art in artifacts {
            if art.selected {
                if let Ok(bytes) = SafeDeleter::clean_path(&art.path, dry_run, permanent) {
                    reclaimed += bytes;
                    count += 1;
                }
            }
        }

        (reclaimed, count)
    }
}

fn check_project_dir(
    project_dir: &Path,
    now: &SystemTime,
    min_idle: Duration,
    out: &mut Vec<(String, PathBuf, ArtifactKind, PathBuf, u64)>,
) {
    let p_name = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Check project modification age
    let idle_days = calculate_project_idle_days(project_dir, now);

    // Only flag as dormant if idle time exceeds threshold
    if idle_days * 86400 < min_idle.as_secs() {
        return;
    }

    // 1. Rust target/
    let target = project_dir.join("target");
    if target.exists()
        && (project_dir.join("Cargo.toml").exists() || target.join("CACHEDIR.TAG").exists())
    {
        out.push((
            p_name.clone(),
            project_dir.to_path_buf(),
            ArtifactKind::RustTarget,
            target,
            idle_days,
        ));
    }

    // 2. Node node_modules/
    let nm = project_dir.join("node_modules");
    if nm.exists() && project_dir.join("package.json").exists() {
        out.push((
            p_name.clone(),
            project_dir.to_path_buf(),
            ArtifactKind::NodeModules,
            nm,
            idle_days,
        ));
    }

    // 3. Python .venv or venv
    for venv_name in [".venv", "venv", "env"] {
        let venv_dir = project_dir.join(venv_name);
        if venv_dir.exists()
            && (venv_dir.join("bin/activate").exists() || venv_dir.join("pyvenv.cfg").exists())
        {
            out.push((
                p_name.clone(),
                project_dir.to_path_buf(),
                ArtifactKind::PythonVenv,
                venv_dir,
                idle_days,
            ));
            break;
        }
    }

    // 4. CocoaPods Pods/
    let pods = project_dir.join("Pods");
    if pods.exists() && project_dir.join("Podfile").exists() {
        out.push((
            p_name.clone(),
            project_dir.to_path_buf(),
            ArtifactKind::CocoaPods,
            pods,
            idle_days,
        ));
    }

    // 5. Gradle build/
    let build_dir = project_dir.join("build");
    if build_dir.exists()
        && (project_dir.join("build.gradle").exists()
            || project_dir.join("build.gradle.kts").exists())
    {
        out.push((
            p_name,
            project_dir.to_path_buf(),
            ArtifactKind::GradleBuild,
            build_dir,
            idle_days,
        ));
    }
}

/// Calculate idle days based on latest modification time of key manifest or directory.
fn calculate_project_idle_days(dir: &Path, now: &SystemTime) -> u64 {
    let mut latest_mtime = None;

    let manifests = [
        dir.join("Cargo.toml"),
        dir.join("package.json"),
        dir.join("pyproject.toml"),
        dir.join("requirements.txt"),
        dir.join("Podfile"),
        dir.join("build.gradle"),
    ];

    for m in &manifests {
        if let Ok(meta) = fs::metadata(m) {
            if let Ok(modified) = meta.modified() {
                if latest_mtime.is_none() || Some(modified) > latest_mtime {
                    latest_mtime = Some(modified);
                }
            }
        }
    }

    // Fallback to project directory mtime
    if latest_mtime.is_none() {
        if let Ok(meta) = fs::metadata(dir) {
            if let Ok(modified) = meta.modified() {
                latest_mtime = Some(modified);
            }
        }
    }

    if let Some(mtime) = latest_mtime {
        if let Ok(elapsed) = now.duration_since(mtime) {
            return elapsed.as_secs() / 86400;
        }
    }

    0
}
