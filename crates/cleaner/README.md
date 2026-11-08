# everyfiles-cleaner

CleanMyMac-style macOS system cleaner, application uninstaller, and developer suite built on `everyfiles`' high-speed filesystem engine.

## Overview

`everyfiles-cleaner` provides modular, high-performance scanning and safe cleaning primitives for macOS:

1. **Safety Guard (`SafetyGuard`)**: Hardened safety barrier preventing accidental deletion of macOS system roots (`/System`, `/usr`, `/Library/Apple`, etc.), user identity/keys (`~/.ssh`, `~/.gnupg`, Keychains), and critical user data.
2. **Safe Deleter (`SafeDeleter`)**: Safe-by-default file deletion mechanism utilizing macOS Trash (`~/.Trash` via the `trash` crate), with dry-run estimation, progress callbacks, and optional permanent deletion.
3. **Scanners (`CleanupCoordinator`)**: Parallel scanning engine covering:
   - System junk (system caches, logs, diagnostic reports, unused `.lproj` language packs, broken login items)
   - Application junk (WeChat, WeCom, VS Code, Cursor, media caches, orphaned app leftovers, old dmg/pkg installers)
   - Web junk (Mail downloads, Safari, Google Chrome, Quark caches, quarantine logs)
   - Developer junk (Rust Cargo, npm/yarn/pnpm/bun, Python pip/uv/poetry, Java Gradle/Maven, Homebrew, Docker)
   - Large & old files (>100MB, >60 days idle)
4. **Duplicate Finder (`DuplicateFinder`)**: Multi-tier duplicate file identification engine (size matching -> 16KB header hash -> xxHash3 full file hash).
5. **Space Lens (`SpaceLens`)**: Interactive DaisyDisk-style visual drilldown disk space analyzer.
6. **App Uninstaller (`AppUninstaller`)**: Complete footprint analyzer for installed macOS `.app` bundles, inspecting `Application Support`, `Caches`, `Preferences`, `Containers`, `Saved Application State`, `WebKit`, and `LaunchAgents`.
7. **Developer Suite (`DeveloperManager`)**: Workspace-level scanner for dormant `node_modules`, `target/`, and `.venv` artifacts based on idle age, Git repository object store bloat scanner with `git gc` compaction, Xcode simulator cleanup, and Docker system prune.
8. **Interactive Terminal UI (`CleanerApp`)**: Full-color keyboard-driven dashboard implemented with `ratatui` and `crossterm`.

## Usage as a Library

```toml
[dependencies]
everyfiles-cleaner = { path = "crates/cleaner" }
```

```rust
use everyfiles_cleaner::{CleanupCoordinator, CleanCategory, SafeDeleter, format_size};

fn main() {
    // 1. Parallel system scan
    let summary = CleanupCoordinator::scan_all();
    println!("Found {} items totaling {}", summary.total_items, format_size(summary.total_size));

    // 2. Safe deletion to Trash
    let group = CleanupCoordinator::scan_category(CleanCategory::DevJunk);
    let report = SafeDeleter::execute(&group.items, false, false, |item, cur, tot| {
        println!("[{cur}/{tot}] Trashing: {}", item.title);
    });
    println!("Reclaimed {}", format_size(report.reclaimed_bytes));
}
```
