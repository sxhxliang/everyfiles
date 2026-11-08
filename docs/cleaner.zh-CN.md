# everyfiles 多平台系统清理与环境管理套件 (Cleaner Suite) 使用指南

`everyfiles clean` 是基于 `everyfiles` 高性能文件系统核心构建的现代化全平台系统清理与开发环境管理套件，原生完整支持 **macOS**、**Linux** 与 **Windows**。

它集成了媲美 **CleanMyMac** 的智能垃圾清理与维护能力、**AppCleaner** 的深度应用卸载（macOS .app、Linux .desktop/Flatpak、Windows Program Files）、**DaisyDisk** 的交互式磁盘空间透镜、**Gemini** 的极速哈希重复文件查重，以及专为开发者量身定制的**开发套件专项清理**（涵盖 Cargo、npm、pnpm、pip、uv、Gradle、Git 膨胀治理与 Docker 空间回收）。

---

## 目录

1. [快速开始](#快速开始)
2. [多平台安全设计理念](#多平台安全设计理念)
3. [核心清理分类与智能扫描 (跨平台适配)](#核心清理分类与智能扫描-跨平台适配)
4. [终端交互式仪表盘 (TUI)](#终端交互式仪表盘-tui)
5. [空间向：重复文件查找器与空间透镜 (Option A)](#空间向重复文件查找器与空间透镜)
6. [应用向：应用全景与深度卸载 (Option B)](#应用向应用全景与深度卸载)
7. [开发者向：依赖治理与环境回收 (Option C)](#开发者向依赖治理与环境回收)
8. [多平台系统优化与维护](#多平台系统优化与维护)
9. [各平台差异与适配速查](#各平台差异与适配速查)
10. [完整 CLI 参数速查表](#完整-cli-参数速查表)

---

## 快速开始

```sh
# 1. 运行一键智能扫描并输出系统健康与垃圾建议报告
everyfiles clean scan

# 2. 启动终端交互式全彩仪表盘 (macOS / Linux / Windows 终端均原生支持)
everyfiles clean ui
# 或直接简写：
everyfiles clean -i

# 3. 一键安全将所有推荐清理项移至系统废纸篓/回收站
everyfiles clean apply

# 4. 模拟运行（Dry-Run，仅计算空间，不修改任何文件）
everyfiles clean apply --dry-run
```

---

## 多平台安全设计理念

系统清理的第一优先级是**绝对安全**。`everyfiles clean` 在全平台上遵循严格的安全防护规范：

1. **废纸篓/回收站优先 (Trash-First)**：
   所有清理操作（应用卸载、无用缓存、闲置依赖、重复文件等）默认**移入系统废纸篓或回收站**（macOS `~/.Trash`、Linux FreeDesktop `~/.local/share/Trash`、Windows `$Recycle.Bin`），杜绝不可逆的直接粉碎。若有误删，可在回收站中一键原样放回。
2. **SafetyGuard 核心白名单防御**：
   无论用户传入何种路径或参数，系统核心关键目录均被永久拒绝操作：
   - **Unix / macOS / Linux**：`/`、`/System`、`/usr`、`/bin`、`/sbin`、`/etc`、`/boot`、`/proc`、`/sys`、`/dev` 等；
   - **Windows**：磁盘驱动器根目录（如 `C:\`）、`C:\Windows`、`C:\Program Files`、`System32`、`SysWOW64`、`C:\ProgramData\Microsoft` 等；
   - **用户隐私与凭证**：`~/.ssh`、`~/.gnupg`、`~/Library/Keychains`、Linux `keyrings`、Windows `NTUSER.DAT`、密码库数据库（`.kdbx`、`.key`、`.pem`）；
   - **活跃工程源码**：严禁删除 `.git` 根目录及工程源码。
3. **系统预装保护识别**：
   在执行深度应用卸载时，自动识别 macOS SIP 系统预装、Linux 核心桌面组件与 Windows 关键程序，杜绝误卸载。
4. **全功能模拟试运行 (Dry-Run)**：
   所有具有修改能力的子命令均支持 `--dry-run` 参数，可在执行前完整预览将要清理的路径明细与可释放空间。

---

## 核心清理分类与智能扫描 (跨平台适配)

针对不同操作系统的文件组织规范，智能扫描器自动匹配最合理的系统垃圾与缓存项：

| 清理大类 | macOS 扫描目标 | Linux 扫描目标 | Windows 扫描目标 |
| :--- | :--- | :--- | :--- |
| **系统缓存** | `/Library/Caches`、原生组件缓存 | `~/.cache`（缩略图、字体缓存）、APT/Pacman/DNF 包缓存 | `%LOCALAPPDATA%\Temp`、`C:\Windows\Temp`、`SoftwareDistribution` 更新暂存 |
| **系统日志** | `~/Library/Logs`、系统崩溃诊断报告 | `/var/log`（系统日志）、`/var/crash`、X11/Wayland 会话日志 | `%LOCALAPPDATA%\CrashDumps`、`Minidump`、`WER` 错误报告 |
| **应用垃圾** | WeChat、VS Code、Cursor、媒体缓存、`.dmg`/`.pkg` | VS Code、Cursor、Discord、Chrome、`.deb`/`.rpm`/`.AppImage` | VS Code、Cursor、WeChat、Chrome、Edge、`.msi` 安装包 |
| **上网垃圾** | Safari、Chrome、Mail 附件、Quarantine 跟踪日志 | Chrome、Chromium、Firefox、Edge 缓存 | Chrome、Edge、Firefox 浏览器数据缓存 |
| **废纸篓/回收站**| `~/.Trash` 及外接盘 `.Trashes` | `~/.local/share/Trash` 及挂载盘回收站 | `C:\$Recycle.Bin` 及各驱动器回收站 |
| **大旧文件** | 全平台支持：筛选大于设定体积（默认 100MB）且长期未修改（默认 60 天）的大文件 |

```sh
# 执行全盘智能扫描
everyfiles clean scan

# 仅扫描指定分类 (system, app, web, dev, large, trash)
everyfiles clean scan --category app
everyfiles clean scan --category dev

# 输出格式化 JSON
everyfiles clean scan --json
```

---

## 终端交互式仪表盘 (TUI)

运行 `everyfiles clean ui` 即可进入跨平台自适应全彩终端仪表盘：

```
┌── everyfiles 系统清理管家 ────────────────────────────────────────────────────────┐
│                                                                                  │
│  [x] 1. 系统缓存与日志 (System Junk)       共 8.23 GB  [建议清理: 2.83 GB]          │
│  [x] 2. 应用程序垃圾   (App Junk)          共 20.0 GB  [建议清理: 1.49 GB]          │
│  [x] 3. 浏览器与网络   (Web Junk)          共 1.07 GB  [建议清理: 1.07 GB]          │
│  [x] 4. 开发工具与依赖 (Dev Tools)         共 8.67 GB  [建议清理: 8.67 GB]          │
│  [ ] 5. 大旧文件排查   (Large & Old Files) 共 15.2 GB  [需用户确认]                 │
│  [ ] 6. 回收站/废纸篓  (Trash Bins)        共 5.28 GB  [需用户确认]                 │
│                                                                                  │
│  操作提示: [Tab] 切换分类 | [Space] 勾选/取消 | [Enter] 清理选中项 | [q] 退出       │
└──────────────────────────────────────────────────────────────────────────────────┘
```

- **全键盘快捷控制**：`Tab` / `Shift+Tab` 切换分类与条目，`↑`/`↓` 上下浏览，`Space` 切换勾选，`Enter` 执行安全清理。
- **全平台终端适配**：在 macOS Terminal / iTerm2、Linux GNOME Terminal / Alacritty、Windows Terminal / PowerShell 下均具备一致的平滑渲染体验。

---

## 空间向：重复文件查找器与空间透镜 (Option A)

### 1. 毫秒级极速重复文件查找器 (`everyfiles clean duplicates`)

基于跨平台高效 xxHash3 硬件加速与三级渐变过滤算法（文件大小初筛 -> 16KB 头部特征检验 -> 全文哈希比对）：

```sh
# 扫描指定目录所有大于 10KB 的重复文件
everyfiles clean duplicates ~/Downloads --min-size 10K

# 限制递归深度为 3
everyfiles clean duplicates . --max-depth 3

# 自动保留最新/原始副本，将多余的重复副本移至废纸篓/回收站
everyfiles clean duplicates ~/Downloads --clean

# 模拟预演可释放的重复空间
everyfiles clean duplicates ~/Downloads --clean --dry-run
```

### 2. DaisyDisk 风格交互式磁盘空间透镜 (`everyfiles clean space`)

类似 DaisyDisk / WinDirStat / Baobab，以树状聚合形式排查磁盘空间占用，支持终端交互式下钻探索。

```sh
# 启动空间透镜交互式下钻浏览器（按 Enter 进入子目录，Backspace 返回上一层）
everyfiles clean space ~

# 以纯文本结构树方式打印占用排行
everyfiles clean space ~/Code --summary

# 输出 JSON 格式目录空间树
everyfiles clean space ~/Code --json
```

---

## 应用向：应用全景与深度卸载 (Option B)

### 1. 全盘已安装应用资产全景 (`everyfiles clean apps --list`)

- **macOS**：扫描 `/Applications`，自动汇总 `.app` 本体与 `~/Library` 内各沙盒与支持数据占用。
- **Linux**：扫描 `/usr/share/applications`、`~/.local/share/applications` 中的 `.desktop` 应用，以及 `~/.var/app/` 中的 Flatpak 独立应用容器，汇总 `~/.config`、`~/.cache`、`~/.local/share` 数据。
- **Windows**：扫描 `C:\Program Files`、`C:\Program Files (x86)`、`%LOCALAPPDATA%\Programs` 安装目录，并关联 `%APPDATA%` 与 `%LOCALAPPDATA%` 数据。

```sh
# 列出系统已安装的应用程序（按全套真实总空间降序排列）
everyfiles clean apps --list

# 仅查看已卸载应用遗留在系统中的孤立残留垃圾 (Leftovers)
everyfiles clean apps --leftovers
```

### 2. 深度卸载器 (`everyfiles clean uninstall`)

连根拔起指定应用及其散落在系统各角落的关联配置、缓存与沙盒数据：

```sh
# 深度卸载指定应用（支持显示名、英文名、包名）
everyfiles clean uninstall <应用名>

# 卸载预览（模拟测算可释放空间，不改动文件）
everyfiles clean uninstall <应用名> --dry-run

# 免确认直接卸载
everyfiles clean uninstall <应用名> -y
```

---

## 开发者向：依赖治理与环境回收 (Option C)

针对跨平台程序员日常开发中最严重的磁盘膨胀痛点（node_modules 黑洞、Rust target 目录、Python 虚拟环境、Git 历史对象累积、Docker 镜像无法回收等）：

```sh
# 1. 查看全局开发工具离线缓存 (Cargo, npm, pnpm, Gradle, Maven, pip, uv, Homebrew)
everyfiles clean dev

# 2. 一键清理开发工具缓存
everyfiles clean dev --clean

# 3. 排查本地闲置工程依赖与构建产物 (node_modules, target, .venv, Pods, build)
# 默认扫描当前目录及 ~/Code、~/Projects、~/Developer 等，筛选闲置 >= 30 天的工程
everyfiles clean dev --projects

# 4. 指定工作区目录与自定义闲置阈值（如筛选闲置超过 60 天的工程）
everyfiles clean dev --projects --workspace ~/Projects --idle-days 60

# 5. 将满足闲置条件的工程依赖与构建产物移至废纸篓/回收站
everyfiles clean dev --projects --workspace ~/Projects --idle-days 60 --clean

# 6. 排查本地 Git 仓库体积与松散对象膨胀情况
everyfiles clean dev --git

# 7. 一键对所有标记为 [⚠️ 膨胀] 的 Git 仓库执行 git gc --prune=now --aggressive 压缩
everyfiles clean dev --git --clean

# 8. 清理因 Xcode 升级遗留的不可用 iOS 模拟器 (macOS 独有)
everyfiles clean dev --simulators

# 9. 深度回收 Docker Desktop / Docker Engine 磁盘空间 (docker system prune)
everyfiles clean dev --docker
```

---

## 多平台系统优化与维护

除了文件垃圾清理外，还集成了各操作系统底层的核心优化维护能力：

```sh
# 1. 立即释放系统空闲内存 (RAM purge)
# - macOS: purge 指令
# - Linux: sync & drop_caches
# - Windows: 触发系统工作集内存回收
everyfiles clean optimize --ram

# 2. 刷新系统 DNS 解析缓存
# - macOS: dscacheutil -flushcache
# - Linux: resolvectl flush-caches / systemd-resolve
# - Windows: ipconfig /flushdns
everyfiles clean optimize --dns

# 3. 重建文件搜索索引
# - macOS: mdutil 重建 Spotlight 索引
# - Linux: updatedb 更新 locate 索引数据库
# - Windows: 重启并触发 Windows Search 服务重构
everyfiles clean optimize --spotlight
```

---

## 各平台差异与适配速查

| 特性 | macOS | Linux | Windows |
| :--- | :--- | :--- | :--- |
| **删除目标** | 移入系统废纸篓 (`~/.Trash`) | 移入 FreeDesktop 回收站 (`~/.local/share/Trash`) | 移入系统回收站 (`$Recycle.Bin`) |
| **应用格式** | `.app` Bundle 沙盒容器 | `.desktop` 快捷方式 / Flatpak `~/.var/app` | `Program Files` 安装目录 + `AppData` |
| **磁盘空间获取** | `libc::statvfs` | `libc::statvfs` | Win32 `GetDiskFreeSpaceExW` API |
| **DNS 刷新** | `dscacheutil` & `mDNSResponder` | `resolvectl` / `systemd-resolved` | `ipconfig /flushdns` |
| **内存释放** | `purge` | `vm.drop_caches=3` | GC / Working Set Trim |
| **搜索重构** | `mdutil -E` | `updatedb` | Windows Search (`WSearch`) |
| **跨平台编译保证** | 原生真机硬件通过 | `x86_64` & `aarch64` CI / `xtask xcheck` 验证通过 | `x86_64` & `i686` CI / `xtask xcheck` 验证通过 |

---

## 完整 CLI 参数速查表

| 子命令 / 参数 | 简写 / 选项 | 说明 |
| :--- | :--- | :--- |
| `everyfiles clean ui` | `-i`, `--interactive` | 启动全彩终端交互式仪表盘 |
| `everyfiles clean scan` | `--category <cat>` | 执行智能扫描报告（可选 system/app/web/dev/large/trash） |
| `everyfiles clean apply` | `--dry-run`, `-y` | 立即将智能扫描的推荐项移至回收站 |
| `everyfiles clean dev` | `--projects` | 排查工作区内闲置的 `node_modules`、`target`、`.venv` |
| | `--git` | 排查本地 Git 仓库存储与膨胀率 |
| | `--simulators` | 清理失效的 Xcode iOS 模拟器实例（macOS） |
| | `--docker` | 回收 Docker 悬空镜像、无用容器与卷 |
| | `--idle-days <N>` | 闲置工程最小天数阈值（默认 30 天） |
| | `--clean` | 执行对应的清理与压缩操作 |
| `everyfiles clean apps` | `-l`, `--list` | 盘点已安装应用的真实全套体积占用 |
| | `--leftovers` | 仅查看已卸载软件的残留文件 |
| `everyfiles clean uninstall <app>` | `--dry-run`, `-y` | 深度卸载指定应用程序及全套关联文件 |
| `everyfiles clean duplicates <path>` | `--min-size <size>` | 极速哈希去重扫描（如 `--min-size 1M`） |
| | `--clean` | 自动保留一份最新/原始文件，多余副本移至回收站 |
| `everyfiles clean space <path>` | `--summary` | 空间透镜交互式下钻探索（DaisyDisk 模式） |
| `everyfiles clean large-files` | `--min-size`, `--older-than` | 排查大旧文件（如 `--min-size 500M --older-than 90d`） |
| `everyfiles clean web` | `--json` | 专项排查各浏览器缓存 |
| `everyfiles clean optimize` | `--ram`, `--dns`, `--spotlight` | 执行内存释放、DNS 刷新与系统搜索索引重建 |
