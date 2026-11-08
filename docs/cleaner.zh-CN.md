# everyfiles Mac 清理管家 (Cleaner Suite) 使用指南

`everyfiles clean` 是基于 `everyfiles` 高性能文件系统核心构建的 macOS 现代化系统清理与开发环境管理套件。它集成了媲美 **CleanMyMac** 的垃圾清理与维护能力、**AppCleaner** 的深度应用卸载、**DaisyDisk** 的交互式磁盘空间透镜、**Gemini** 的极速哈希重复文件查重，以及专为 Mac 开发者量身定制的**开发套件专项清理**（涵盖 Cargo、npm、pnpm、pip、Gradle、Git 膨胀治理与 Docker/Xcode 模拟器回收）。

---

## 目录

1. [快速开始](#快速开始)
2. [安全设计理念](#安全设计理念)
3. [核心清理分类与智能扫描](#核心清理分类与智能扫描)
4. [终端交互式仪表盘 (TUI)](#终端交互式仪表盘-tui)
5. [空间向：重复文件查找器与空间透镜 (Option A)](#空间向重复文件查找器与空间透镜)
6. [应用向：应用全景与深度卸载 (Option B)](#应用向应用全景与深度卸载)
7. [开发者向：依赖治理与环境回收 (Option C)](#开发者向依赖治理与环境回收)
8. [系统优化与深度维护](#系统优化与深度维护)
9. [完整 CLI 参数速查表](#完整-cli-参数速查表)

---

## 快速开始

```sh
# 1. 运行一键智能扫描并输出系统健康与垃圾建议报告
everyfiles clean scan

# 2. 启动 CleanMyMac 风格的终端交互式全彩仪表盘
everyfiles clean ui
# 或直接简写：
everyfiles clean -i

# 3. 一键安全将所有推荐清理项移至系统废纸篓
everyfiles clean apply

# 4. 模拟运行（Dry-Run，仅计算空间，不修改任何文件）
everyfiles clean apply --dry-run
```

---

## 安全设计理念

系统清理的第一优先级是**绝对安全**。`everyfiles clean` 遵循严格的安全防护规范：

1. **废纸篓优先 (Trash-First)**：
   所有清理操作（应用卸载、无用缓存、闲置依赖、重复文件等）默认**移入 macOS 废纸篓 (`~/.Trash`)**，而不是执行不可逆的 `rm -rf`。如果误删，可在废纸篓中随时一键“放回原处”。
2. **SafetyGuard 核心白名单防护**：
   底层内置硬性拦截规则，无论用户传入何种路径或参数，系统核心关键目录均被永久拒绝：
   - 系统核心：`/System`、`/usr`、`/bin`、`/sbin`、`/etc`、`/Library/Apple` 等；
   - 用户隐私与凭证：`~/.ssh`、`~/.gnupg`、`~/Library/Keychains`、密码管理数据库；
   - 活跃工程源码：严禁删除工程源码与非构建输出目录。
3. **系统保护应用识别**：
   深度卸载模块自动识别 macOS 系统预装与受 SIP 保护的应用（如 Finder、Safari、Music 等），杜绝破坏系统。
4. **模拟试运行 (Dry-Run)**：
   所有具有修改能力的子命令均支持 `--dry-run` 参数，可在执行前完整预览将要清理的路径明细与可释放字节数。

---

## 核心清理分类与智能扫描

智能扫描器覆盖 CleanMyMac 核心三大垃圾板块及大旧文件排查：

```sh
# 执行全盘智能扫描
everyfiles clean scan

# 仅扫描指定分类 (system, app, web, dev, large, trash)
everyfiles clean scan --category app
everyfiles clean scan --category dev

# 输出易于脚本处理的格式化 JSON 对象
everyfiles clean scan --json
```

### 扫描涵盖的板块：
- **系统垃圾 (System Junk)**：
  - 系统通用缓存 (`/Library/Caches`、`~/Library/Caches`)
  - 系统与诊断日志 (`/var/log`、`~/Library/Logs`、DiagnosticReports)
  - 无用多语言本地化包 (`.lproj`，保留中英文及系统默认语言)
  - 损坏的开机启动项与失效的 LaunchAgents
  - iOS 废弃同步缓存与软件升级临时镜像
- **应用垃圾 (App Junk)**：
  - 微信 (WeChat)、企业微信 (WeCom) 历史解压临时文件与网页缓存
  - VS Code / Cursor 代码编辑器历史缓存与日志
  - 网易云音乐、腾讯视频等视听应用媒体缓存
  - 卸载残留：已从 `/Applications` 移除的应用残留孤立数据
  - 历史无用安装包 (`.dmg`、`.pkg`、`.iso`)
- **上网垃圾 (Web Junk)**：
  - Safari、Chrome、Quark 浏览器的页面缓存与 Code Cache
  - 邮件 (Apple Mail) 历史下载附件
  - 下载文件检疫跟踪日志 (`com.apple.quarantine` 记录)
- **大旧文件 (Large & Old Files)**：
  - 默认筛选体积大于 100MB 且闲置超过 60 天的文件，按视频、归档、磁盘镜像、文档等分类呈现。

---

## 终端交互式仪表盘 (TUI)

运行 `everyfiles clean ui` 即可进入基于 `ratatui` 构建的终端全彩 CleanMyMac 风格仪表盘：

```
┌── everyfiles Mac 清理管家 ────────────────────────────────────────────────────────┐
│                                                                                  │
│  [x] 1. 系统垃圾 (System Junk)       共 8.23 GB  [建议清理: 2.83 GB]               │
│  [x] 2. 应用垃圾 (App Junk)          共 20.0 GB  [建议清理: 1.49 GB]               │
│  [x] 3. 上网垃圾 (Web Junk)          共 1.07 GB  [建议清理: 1.07 GB]               │
│  [x] 4. 开发工具 (Dev Tools)         共 8.67 GB  [建议清理: 8.67 GB]               │
│  [ ] 5. 大旧文件 (Large & Old Files) 共 15.2 GB  [需用户确认]                      │
│  [ ] 6. 废纸篓   (Trash Bins)        共 5.28 GB  [需用户确认]                      │
│                                                                                  │
│  操作提示: [Tab] 切换分类 | [Space] 勾选/取消 | [Enter] 清理选中项 | [q] 退出    │
└──────────────────────────────────────────────────────────────────────────────────┘
```

- **全键盘快捷控制**：`Tab` / `Shift+Tab` 切换分类列表与条目，`↑`/`↓` 上下浏览，`Space` 切换勾选，`Enter` 执行安全清理。
- **三种安全指示标**：
  - `🟢 建议清理`：完全无害的临时构建包、系统及浏览器运行时缓存；
  - `🟡 需用户确认`：废纸篓、下载文件夹中的历史大文件；
  - `⚪ 已处于很干净状态`：经探测无垃圾积累的项目。

---

## 空间向：重复文件查找器与空间透镜

### 1. 毫秒级极速重复文件查找器 (`everyfiles clean duplicates`)

采用**三级渐变过滤算法**（文件大小分组 -> 16KB 头部特征过滤 -> 全文 xxHash3 硬件加速哈希），避免无脑读取大文件，秒级扫描数万文件。

```sh
# 扫描当前工作区所有大于 10KB 的重复文件
everyfiles clean duplicates . --min-size 10K

# 扫描指定目录，限制递归深度为 3
everyfiles clean duplicates ~/Downloads --max-depth 3

# 自动保留最新/原始副本，将多余的重复文件安全移入废纸篓
everyfiles clean duplicates ~/Downloads --clean

# 模拟预演可释放的重复空间
everyfiles clean duplicates ~/Downloads --clean --dry-run
```

### 2. DaisyDisk 风格交互式磁盘空间透镜 (`everyfiles clean space`)

类似 DaisyDisk / GrandPerspective，以树状多级聚合形式排查磁盘空间占用，支持在终端交互式下钻（Drill-down）浏览。

```sh
# 启动空间透镜交互式下钻浏览器（按 Enter 进入子目录，Backspace 返回上一层）
everyfiles clean space ~

# 以纯文本结构树方式打印占用排行
everyfiles clean space ~/Code --summary

# 输出 JSON 格式目录空间树
everyfiles clean space ~/Code --json
```

---

## 应用向：应用全景与深度卸载

### 1. 全盘已安装应用资产全景 (`everyfiles clean apps --list`)

解决 macOS 下应用本体与支持数据分离、无法直观了解真实占用的痛点。自动汇总应用本体（.app）、Application Support、Caches、Preferences、Containers 沙盒等全部数据。

```sh
# 列出系统已安装的应用程序（按全套真实总空间降序排列）
everyfiles clean apps --list

# 仅查看已卸载应用遗留在系统中的孤立残留垃圾 (Leftovers)
everyfiles clean apps --leftovers
```

*实测输出示例：*
```
 1. WeChat           (版本: 4.2.0)  全套占用: 36.79 GB  (本体: 1.43 GB) | Bundle: com.tencent.xinWeChat
 2. Xcode            (版本: 27.2)   全套占用:  9.64 GB  (本体: 9.64 GB) | Bundle: com.apple.dt.Xcode
 3. 企业微信          (版本: 5.0.6)  全套占用:  3.19 GB  (本体: 1.30 GB) | Bundle: com.tencent.WeWorkMac
 4. Zed              (版本: 0.221)  全套占用:  1.58 GB  (本体: 357 MB)  | Bundle: dev.zed.Zed
```

### 2. 媲美 AppCleaner 的深度卸载器 (`everyfiles clean uninstall`)

连根拔起指定应用及其散落在系统各角落的关联文件（包括 `.app`、沙盒容器、WebKit 缓存、开机启动项等）。

```sh
# 深度卸载指定应用（支持显示名、英文名、Bundle ID，操作将移入废纸篓）
everyfiles clean uninstall Zed

# 卸载预览（模拟测算可释放空间，不改动文件）
everyfiles clean uninstall Zed --dry-run

# 免确认提示直接卸载
everyfiles clean uninstall Zed -y
```

---

## 开发者向：依赖治理与环境回收

专门解决程序员日常开发中最严重的磁盘膨胀痛点（node_modules 黑洞、Rust target 目录动辄数十 GB、Git 历史对象累积、Docker 镜像无法回收等）。

```sh
# 1. 查看全局开发工具离线缓存 (Cargo, npm, pnpm, Gradle, Maven, pip, uv, Homebrew)
everyfiles clean dev

# 2. 一键清理开发工具缓存
everyfiles clean dev --clean

# 3. 排查本地闲置工程依赖与构建产物 (node_modules, target, .venv, Pods)
# 默认扫描当前目录及 ~/Code、~/Projects、~/Developer 等，筛选闲置 >= 30 天的工程
everyfiles clean dev --projects

# 4. 指定工作区目录与自定义闲置阈值（如筛选闲置超过 60 天的工程）
everyfiles clean dev --projects --workspace ~/Code/Rust-Project --idle-days 60

# 5. 将满足闲置条件的工程依赖与构建产物移至废纸篓
everyfiles clean dev --projects --workspace ~/Code/Rust-Project --idle-days 60 --clean

# 6. 排查本地 Git 仓库体积与松散对象膨胀情况
everyfiles clean dev --git

# 7. 一键对所有标记为 [⚠️ 膨胀] 的 Git 仓库执行 git gc --prune=now --aggressive 压缩
everyfiles clean dev --git --clean

# 8. 清理因 Xcode 升级遗留的不可用 iOS 模拟器
everyfiles clean dev --simulators

# 9. 深度回收 Docker Desktop 磁盘空间 (docker system prune)
everyfiles clean dev --docker
```

---

## 系统优化与深度维护

除了文件垃圾清理外，还集成了 macOS 系统底层优化命令：

```sh
# 1. 立即释放系统空闲内存 (RAM purge)
everyfiles clean optimize --ram

# 2. 刷新 macOS 系统 DNS 解析缓存 (dscacheutil & mDNSResponder)
everyfiles clean optimize --dns

# 3. 重建指定分区的 Spotlight 聚焦搜索索引 (mdutil)
everyfiles clean optimize --spotlight /
```

---

## 完整 CLI 参数速查表

| 子命令 / 参数 | 简写 / 选项 | 说明 |
| :--- | :--- | :--- |
| `everyfiles clean ui` | `-i`, `--interactive` | 启动全彩终端交互式仪表盘 |
| `everyfiles clean scan` | `--category <cat>` | 执行智能扫描报告（可选 system/app/web/dev/large/trash） |
| `everyfiles clean apply` | `--dry-run`, `-y` | 立即将智能扫描的推荐项移至废纸篓 |
| `everyfiles clean dev` | `--projects` | 排查工作区内闲置的 `node_modules`、`target`、`.venv` |
| | `--git` | 排查本地 Git 仓库存储与膨胀率 |
| | `--simulators` | 清理失效的 Xcode iOS 模拟器实例 |
| | `--docker` | 回收 Docker 悬空镜像、无用容器与卷 |
| | `--idle-days <N>` | 闲置工程最小天数阈值（默认 30 天） |
| | `--clean` | 执行对应的清理与压缩操作 |
| `everyfiles clean apps` | `-l`, `--list` | 盘点已安装应用的真实全套体积占用 |
| | `--leftovers` | 仅查看已卸载软件的残留文件 |
| `everyfiles clean uninstall <app>` | `--dry-run`, `-y` | 深度卸载指定应用程序及全套关联文件 |
| `everyfiles clean duplicates <path>` | `--min-size <size>` | 极速哈希去重扫描（如 `--min-size 1M`） |
| | `--clean` | 自动保留一份最新/原始文件，多余副本移至废纸篓 |
| `everyfiles clean space <path>` | `--summary` | 空间透镜交互式下钻探索（DaisyDisk 模式） |
| `everyfiles clean large-files` | `--min-size`, `--older-than` | 排查大旧文件（如 `--min-size 500M --older-than 90d`） |
| `everyfiles clean web` | `--json` | 专项排查 Safari、Chrome、Mail 缓存 |
| `everyfiles clean optimize` | `--ram`, `--dns`, `--spotlight` | 执行内存释放、DNS 刷新与 Spotlight 索引重建 |
