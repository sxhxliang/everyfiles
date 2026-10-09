# everyfiles 命令行使用说明

`everyfiles` 是一个 Everything 风格的文件名搜索工具：把一棵目录树（或整卷）的全部文件名装进内存索引，用毫秒级延迟回答子串和通配符查询，并能在文件变化时增量维护索引。支持 macOS、Linux、Windows，每个平台使用各自最快的原生接口，权限不足时自动降级。

本文只讲命令行的用法。架构与内部实现见仓库的 `README.md` 和 `docs/development.zh-CN.md`。

---

## 目录

- [安装](#安装)
- [三分钟上手](#三分钟上手)
- [子命令](#子命令)
  - [index：建立并保存索引](#index建立并保存索引)
  - [search：搜索](#search搜索)
  - [watch：持续跟踪变化](#watch持续跟踪变化)
  - [stats：查看索引状态](#stats查看索引状态)
  - [verify：与朴素遍历做差分校验](#verify与朴素遍历做差分校验)
  - [bench：性能基准](#bench性能基准)
  - [backends：列出可用后端](#backends列出可用后端)
- [查询语法](#查询语法)
- [输出格式与管道用法](#输出格式与管道用法)
- [索引文件：位置、新鲜度、何时会重扫](#索引文件位置新鲜度何时会重扫)
- [后端与权限](#后端与权限)
- [全局选项](#全局选项)
- [退出码](#退出码)
- [常见问题](#常见问题)

---

## 安装

需要 Rust 1.98 或更新版本。

```sh
git clone <repo> everyfiles
cd everyfiles
cargo build --release -p everyfiles-cli
# 可执行文件：target/release/everyfiles
```

建议把 `target/release/everyfiles` 放进 `PATH`，下文的示例假定可以直接输入 `everyfiles`。

## 三分钟上手

```sh
# 1. 为一棵目录树建立索引并保存到缓存目录
everyfiles index ~/Code

# 2. 搜索（子串，大小写不敏感）。索引仍新鲜时直接加载，不到 1 ms
everyfiles search main.rs --root ~/Code

# 3. 通配符按整个文件名匹配；记得加引号，别让 shell 展开
everyfiles search '*.toml' --root ~/Code

# 4. 组合过滤：只看文件、大于 1 MiB、最近一周改过、按大小倒序
everyfiles search report --root ~/Code --files-only --min-size 1M --after -7d --sort size --desc

# 5. 让索引随文件系统变化持续更新（前台运行，Ctrl-C 结束）
everyfiles watch ~/Code

# 6. 看看索引状态、用的是哪条平台路径
everyfiles stats ~/Code
```

`search` 和 `stats` 在没有索引或索引过期时会自动重扫并保存，所以第 1 步也可以省略。

## 子命令

### index：建立并保存索引

```
everyfiles index <PATH> [--max-depth N] [--json]
```

扫描 `<PATH>` 下的整棵树，把索引写入缓存目录（或 `--index` 指定的文件），并打印统计：记录数、目录/文件/符号链接数、读取失败的目录数、名字池大小、内存占用、耗时、保存位置和文件大小。

| 选项 | 说明 |
|---|---|
| `--max-depth N` | 不深入到第 N 层以下。根是第 0 层；到达上限的目录仍会被记录，只是不再展开 |
| `--json` | 以单个 JSON 对象输出统计 |

`index` 总是重新扫描，即使已有新鲜索引。

```sh
everyfiles index /usr
everyfiles index ~/Projects --max-depth 3 --json | jq .records
```

### search：搜索

```
everyfiles search <QUERY> --root <PATH> [选项]
```

在 `--root` 的索引中查找 `<QUERY>`。索引仍新鲜时直接从缓存加载；否则先重扫并保存，再查询。

**匹配方式**

| 选项 | 说明 |
|---|---|
| `<QUERY>` | 默认是大小写不敏感的子串。含 `*` 或 `?` 时按整个文件名做通配匹配，见[查询语法](#查询语法) |
| `--exact` | 整个文件名完全相等，而不是子串（通配模式自带此语义） |
| `--case-sensitive` | 按字节精确比较，不做大小写折叠（对子串和通配都生效） |

**过滤**

| 选项 | 说明 |
|---|---|
| `--files-only` / `--dirs-only` | 只要文件（含符号链接）或只要目录，二者互斥 |
| `--min-size S` / `--max-size S` | 大小区间，二进制单位：`512`、`10K`、`4M`、`1.5G`、`2MiB`。目录没有大小，用了大小过滤就不会出现 |
| `--after T` / `--before T` | 修改时间区间，闭区间。`T` 可以是 `2024-02-29`、`2024-02-29T13:05`、`2024-02-29T13:05:09Z`（均按 UTC），或相对于现在的 `-45s`、`-30m`、`-2h`、`-7d`、`-3w` |
| `--hidden` | 包含隐藏/系统条目（Windows 的隐藏/系统属性；Unix 下点文件本身不被标记为隐藏，总会出现） |
| `--max-depth N` | 只搜前 N 层。会强制重新扫描而不加载已保存的索引 |

**排序与分页**

| 选项 | 说明 |
|---|---|
| `--sort name\|size\|mtime\|path` | 排序键，默认 `name`。`path` 需要为每个命中重建完整路径，结果很多时较慢 |
| `--desc` | 倒序 |
| `--limit N` | 最多输出 N 行，默认 100；`0` 表示全部 |
| `--offset N` | 跳过前 N 行，配合 `--limit` 翻页 |

分页时只对前 `offset + limit` 个结果做选择再排序，百万命中也不会全量排序。

**输出与诊断**

| 选项 | 说明 |
|---|---|
| `-o, --output human\|tsv\|jsonl\|json\|null` | 输出格式，见[输出格式](#输出格式与管道用法)。默认 `human` |
| `--stat` | 在 stderr 打印耗时分解：打开/加载、匹配、排序、输出，以及索引来源（加载还是重扫、为何重扫） |
| `--force-rescan` | 忽略已保存的索引，强制重扫 |
| `--verify` | 搜索前对整个索引文件做负载哈希校验（约每 100 MB 10 ms）。头部校验和随机抽查总是执行，这是额外的端到端完整性检查 |

```sh
# 所有 Rust 源文件，按路径排序，全部输出
everyfiles search '*.rs' --root ~/Code --sort path --limit 0

# 最近两小时改动过的 10 个文件，按修改时间倒序
everyfiles search '' --root ~/Code --files-only --after -2h --sort mtime --desc --limit 10

# 大于 100 MiB 的文件
everyfiles search '' --root ~ --files-only --min-size 100M --sort size --desc

# 严格区分大小写的整名匹配
everyfiles search Makefile --root ~/Code --exact --case-sensitive

# 第二页（第 101～200 条）
everyfiles search log --root /var --limit 100 --offset 100

# 看看这次查询花了多久、索引是加载的还是重扫的
everyfiles search config --root ~/Code --stat
```

空查询 `''` 匹配全部条目，常和过滤、排序一起使用。

### watch：持续跟踪变化

```
everyfiles watch <PATH> [--debounce-ms N] [--save-every S] [--no-sweep] [--once] [--force-rescan]
```

打开（或建立）索引，然后监听文件系统变化并增量更新，定期把索引连同"续传位置"一起写回磁盘。每应用一批变更打印一行：

```
batch    4 events  +3 -1 ~0  records 5
```

分别是事件数、新增、删除、元数据更新、当前记录数；末尾可能附带 `[overflow: swept]`（后端丢了事件，已做全树核对）、`[compacted]`（已压缩墓碑）、`[saved]`（本批已持久化）。

启动阶段会打印使用的监听路径，例如 `watcher  macos (fsevents+resume)`，以及是"回放缺失事件"还是"做一次追赶核对（sweep）"。

| 选项 | 说明 |
|---|---|
| `--debounce-ms N` | 把 N 毫秒内的事件合并成一批再应用，默认 500 |
| `--save-every S` | 至少每 S 秒持久化一次，默认 30；退出时也会保存 |
| `--no-sweep` | 启动时跳过追赶核对。只在确定没有离线变更时使用 |
| `--once` | 应用一批后退出，适合脚本和测试 |
| `--force-rescan` | 先完整重扫再开始监听 |

能否跨重启回放取决于平台：macOS（FSEvents）和 Windows 管理员下的 USN 日志可以，索引里保存的令牌仍有效时会直接回放离线期间的变更；Linux 的 inotify/fanotify 和 Windows 的 `ReadDirectoryChangesW` 不能，启动时用目录 mtime 核对来追赶。

```sh
everyfiles watch ~/Code
everyfiles watch ~/Code --debounce-ms 200 --save-every 10
nohup everyfiles watch ~/Code > ~/.everyfiles-watch.log 2>&1 &
```

### stats：查看索引状态

```
everyfiles stats <PATH> [--force-rescan] [--json]
```

打开 `<PATH>` 的索引（必要时重扫），打印：根路径、索引文件、扫描后端及其生效路径、监听后端及其生效路径和降级原因、索引来源（加载耗时或重扫原因）、记录数、存活数、墓碑数、size/mtime 的覆盖率、堆内存与 mmap 映射量，以及后端能力提示。

```sh
everyfiles stats ~/Code
everyfiles stats ~/Code --json | jq '{records, size_coverage, watcher_path}'
```

`size known` 低于 100% 通常是正常的：目录本身没有大小；Linux 默认扫描不采集 size/mtime。

### verify：与朴素遍历做差分校验

```
everyfiles verify <PATH> [--examples N] [--json]
```

用当前后端完整重扫 `<PATH>`，再用最朴素的 `walkdir` + `lstat` 遍历一遍，比对两边的路径集合、种类、文件大小和修改时间。输出缺失、多余、不符的数量和示例；全部为零时打印 `OK` 并以 0 退出，否则打印 `DIFFERENT` 并以 1 退出。

两边都读不到的条目（无权限、途中消失）会跳过并计入 `skipped`。目录自身的 mtime 差异单独列为信息性输出，不算失败——macOS 上 `/usr/local` 这类 firmlink 桩目录通过批量接口读到的是系统卷快照的时间，`lstat` 则跟随到数据卷，两者天然不一致，目录内容的索引并不受影响。

```sh
everyfiles verify ~/Code
everyfiles verify /usr --backend generic   # 用参考实现自检
everyfiles verify ~/Code --json | jq .ok
```

### bench：性能基准

```
everyfiles bench [--records N] [--iterations N] [--path DIR] [--scan-iterations N] [--query Q]... [--json]
```

在内存里生成约 `N` 条（默认 100 万）过程式合成记录，报告：建索引、保存、零拷贝加载、校验和的耗时；子串、通配（预过滤与慢路径）、组合过滤、排序（取前 100 与全量）各类查询的命中数与 p50/p95/p99 延迟。加 `--path DIR` 时还会用每个编入的后端对真实目录各扫描 `--scan-iterations` 次（交替进行，报告最小值与中位数）并计时保存/加载。`--query` 可替换默认的子串用例。

```sh
everyfiles bench
everyfiles bench --records 200000 --iterations 50 --path ~/Code
everyfiles bench --json > bench.json
scripts/bench.sh ~/Code            # 同上，并先打印系统信息、预热页缓存
```

结果与分析见 `docs/benchmarks.zh-CN.md`。

### backends：列出可用后端

```
everyfiles backends
```

列出本二进制编入的后端及层级（`Native`/`Generic`），并标出默认项。

## 查询语法

**子串（默认）。** 查询串出现在文件名任意位置即命中。两边都先做大小写折叠：ASCII 直接小写，非 ASCII 用 Unicode 小写映射（简单折叠，`ß` 不等于 `ss`）。匹配的是**文件名**，不是完整路径。

**通配符。** 查询串含 `*` 或 `?` 时切换为整名通配匹配：`*` 匹配任意长度（可为空），`?` 恰好匹配一个字符（按 UTF-8 字符计，`'?.txt'` 能匹配 `日.txt`）。通配模式自带整名语义，`--exact` 无需再加。请用引号包住模式，否则 shell 会先展开。

```
'*.rs'            所有 .rs 文件
'IMG_????.JPG'    IMG_ 加四个字符加 .JPG（大小写不敏感）
'report*'         以 report 开头
'*2024*'          含 2024
```

模式中若没有长度 ≥ 2 的连续字面量（如 `'*a*'`、`'?'`），无法使用 SIMD 预过滤，会逐条匹配，结果正确但慢一些。

**不支持的。** 正则表达式；转义 `*`/`?` 以匹配字面星号或问号；路径分隔符语义（`src/*.rs` 里的 `/` 只是普通字面量，而文件名里不会有 `/`，所以不会命中）。

## 输出格式与管道用法

| 格式 | 内容 | 适用 |
|---|---|---|
| `human` | 对齐的三列：大小、UTC 修改时间、完整路径；目录显示 `<DIR>`，未知显示 `-` | 终端阅读（默认） |
| `tsv` | `大小<TAB>修改时间纳秒<TAB>是否目录(0/1)<TAB>路径`，无表头 | `cut`/`awk` |
| `jsonl` | 每行一个对象：`path`、`name`、`is_dir`、`size`、`mtime_ns`（后两者未知时为 `null`） | 流式处理、`jq` |
| `json` | 上述对象组成的一个数组 | 一次性读入 |
| `null` | 仅路径，以 NUL 分隔 | `xargs -0`，对含空格、换行的文件名安全 |

```sh
# 删除所有 .orig 文件（先 -n 预览）
everyfiles search '*.orig' --root ~/Code --files-only --limit 0 -o null | xargs -0 -n 20 echo rm

# 按扩展名统计数量
everyfiles search '' --root ~/Code --files-only --limit 0 -o jsonl \
  | jq -r '.name | split(".") | last' | sort | uniq -c | sort -rn | head

# 最大的 20 个文件，人类可读
everyfiles search '' --root ~ --files-only --sort size --desc --limit 20

# 配合 fzf 交互挑选
everyfiles search '' --root ~/Code --files-only --limit 0 -o tsv | cut -f4 | fzf
```

路径以字节原样输出；`json`/`jsonl` 中非 UTF-8 的文件名会做有损转换。

## 索引文件：位置、新鲜度、何时会重扫

**位置。** 默认放在系统缓存目录，文件名是根路径指纹：

| 平台 | 目录 |
|---|---|
| macOS | `~/Library/Caches/everyfiles/` |
| Linux | `$XDG_CACHE_HOME/everyfiles/`（默认 `~/.cache/everyfiles/`） |
| Windows | `%LOCALAPPDATA%\everyfiles\cache\` |

全局选项 `--index <文件>` 可以改用任意路径，适合临时实验或把索引放在特定磁盘。缓存目录可被系统清理，丢了只需重扫。

**新鲜度判定。** `search`、`stats`、`watch` 打开索引时按顺序检查，任一不满足即重扫并覆盖保存：

1. 文件存在、格式版本兼容、校验和通过；
2. 索引记录的根路径与本次一致；
3. 卷身份未变（macOS 卷 UUID、Windows 卷序列号、Linux 启发式）；
4. 根目录仍可读；
5. **抽查**：随机取 16 条记录去 `stat`，种类、大小、修改时间都与索引一致，文件也都还在。

第 5 条意味着：在没有 `watch` 维护的情况下，目录树只要有变化（且被抽到），下一次 `search` 就会整体重扫。这是刻意的——过期的索引宁可重建，也不给出错误答案。要让索引在频繁变动的目录上保持"秒开"，请运行 `watch`。

`--stat` 会告诉你本次是 `loaded`（加载，通常不到 1 ms）还是 `rescanned (原因)`。

## 后端与权限

每个平台有一个原生后端和一个通用后端（`generic`，基于 `std::fs`，最慢但三平台行为一致）。`--backend` 取值：`auto`（默认，等于本机原生后端）、`native`、`generic`，或 `backends` 列出的具体名字。

| 平台 | 扫描路径 | 监听路径 | 需要特权的更快路径 |
|---|---|---|---|
| macOS | `getattrlistbulk`，一次调用取回名字、类型、大小、mtime、inode | FSEvents，可跨重启回放 | 不需要 |
| Linux | `getdents64`，默认只有名字和类型 | inotify（每目录一个 watch） | 以 root 运行时自动改用 fanotify 整文件系统监听 |
| Windows | `FileIdBothDirectoryInfo` 批量读目录，含大小与 mtime | `ReadDirectoryChangesW` | 以管理员运行时自动改用 USN 日志，可跨重启回放 |

特权路径总是先尝试、失败即降级，不会弹窗提权。`stats` 的 `watcher` 一行会说明当前生效的路径以及降级原因（例如 `inotify — fanotify needs root (CAP_SYS_ADMIN)`）。

**Linux 的 `--with-stat`。** `getdents64` 不提供大小和修改时间，所以 Linux 默认索引里没有这两项，`--min-size`/`--after` 等过滤会把所有条目都排除掉（`stats` 的覆盖率显示 0%）。加 `--with-stat` 会对每个条目多做一次 `statx`，获得完整元数据，代价是首次扫描慢得多。

## 全局选项

对所有子命令生效：

| 选项 | 说明 |
|---|---|
| `--backend <名字>` | 选择后端，见上一节 |
| `--index <文件>` | 索引文件路径，替代默认缓存位置 |
| `--threads N` | 扫描线程数，默认全部核心。机械硬盘或网络文件系统上调小可能更快 |
| `--with-stat` | Linux：扫描时逐条 `statx` 以索引大小和修改时间 |
| `--poll-interval S` | 轮询式监听（通用后端）的核对间隔秒数，默认 30 |

## 退出码

| 码 | 含义 |
|---|---|
| 0 | 成功；`verify` 两侧一致 |
| 1 | 运行错误（路径不存在、无法读取、索引文件无法写入……），错误信息在 stderr；或 `verify` 发现差异 |
| 2 | 命令行参数错误 |

## 常见问题

**索引里少了一些目录，但没有报错（macOS）。**
没有授予终端"完全磁盘访问权限"时，系统会静默隐藏受保护目录（`~/Library/Mail`、`~/Documents` 的部分内容等），索引只是不完整，`stats` 无法检测。在"系统设置 → 隐私与安全性 → 完全磁盘访问权限"中授权运行 `everyfiles` 的终端程序。

**`watch` 收不到事件。**
请确认传给 `watch` 和 `search` 的是同一个根。工具会先把根规范化为真实路径（符号链接会被解析，例如 macOS 的 `/var` → `/private/var`），因为内核上报的是真实路径；如果用 `--index` 手动指定了文件，两个命令要指向同一个文件。

**Linux 上 `--min-size`、`--after` 什么都搜不到。**
默认索引没有大小和修改时间。重新 `index` 并加 `--with-stat`，或在 `search` 时加 `--with-stat`（会触发重扫）。

**Windows 上 `stats` 显示监听路径是 `read-directory-changes` 而不是 USN。**
USN 日志需要管理员权限打开卷设备。以管理员身份运行即可自动切换，否则使用 `ReadDirectoryChangesW`，功能相同，只是不能跨重启回放。

**`verify` 报告目录 mtime 差异。**
见 [verify](#verify与朴素遍历做差分校验)：这是信息性输出，不影响结果，常见于 macOS firmlink（`/usr/local` 等）。

**每次 `search` 都在重扫。**
用 `--stat` 看重扫原因。常见原因是目录树确实在变（抽查失败）——对活跃目录请运行 `watch`；或者你用了 `--max-depth`，它总是触发新扫描。

**首次扫描慢。**
macOS 和 Windows 的原生后端比通用后端快数倍（`/usr` 约 77k 条：99 ms 对 632 ms）。确认 `stats` 里 `backend` 一行显示的是原生路径而不是 `std-read-dir`；网络文件系统可能不支持批量接口而被降级。
