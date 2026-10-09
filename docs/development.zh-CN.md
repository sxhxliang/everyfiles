# everyfiles 开发文档

本文面向参与开发的工程师，描述 everyfiles 的架构、核心数据结构、各平台后端、持久化格式、增量更新机制、测试与验证方式，以及当前已知的缺口。用户向的使用说明见仓库根目录 `README.md`。

目录：

1. [项目目标与设计原则](#1-项目目标与设计原则)
2. [工作区布局与依赖方向](#2-工作区布局与依赖方向)
3. [核心索引结构](#3-核心索引结构)
4. [扫描与树的装配](#4-扫描与树的装配)
5. [查询引擎](#5-查询引擎)
6. [持久化格式](#6-持久化格式)
7. [引擎：打开、过期判定、增量更新](#7-引擎打开过期判定增量更新)
8. [平台后端](#8-平台后端)
9. [命令行工具](#9-命令行工具)
10. [测试与验证](#10-测试与验证)
11. [开发约定](#11-开发约定)
12. [实测性能](#12-实测性能)
13. [已知缺口与后续工作](#13-已知缺口与后续工作)

---

## 1. 项目目标与设计原则

everyfiles 是一个 Everything 风格的文件名索引器：把一个卷上所有文件名装进内存，用毫秒级延迟回答子串查询，并在文件系统变化时增量维护索引。目标平台是 macOS、Linux、Windows。

设计上有三条贯穿始终的原则：

- **速度在核心，不在后端。** `everyfiles-core` 持有索引与查询逻辑，是可移植的；每个平台后端只提供"最快的列目录方式"和"最快的变更监听方式"，绝不重复实现索引逻辑。
- **核心不含任何平台代码。** `everyfiles-core` 没有 `cfg(target_os)`，不依赖任何平台 crate，自身也不做文件系统 I/O（连根目录校验都通过 `Scanner` trait 完成）。平台差异要么以值的形式注入（`VolumeConfig`），要么通过 trait 进入。这样核心的测试在三个 CI 平台上含义完全一致，且整个核心可以用纯内存的假扫描器测试。
- **能力如实上报，特权自动降级。** 每个后端通过 `capabilities()` 和 `probe()` 如实说明自己能提供什么（是否有 size/mtime、是否可断点续传、是否需要特权）。特权快路径（fanotify、USN 日志）先尝试，失败就自动退到非特权路径，并报告当前生效的是哪条路径。不弹提权窗口，不做 helper 守护进程，不做 IPC。

## 2. 工作区布局与依赖方向

```
everyfiles/
├── Cargo.toml                   # workspace 根，resolver = "3"
├── crates/
│   ├── core/            everyfiles-core          引擎：index / scan / query / store / engine / watch
│   ├── everyfiles/      everyfiles               门面：后端注册表、默认后端、缓存路径
│   ├── backend-generic/ everyfiles-backend-generic   std::fs 参考实现 + 轮询式监听
│   ├── backend-macos/   everyfiles-backend-macos     getattrlistbulk + FSEvents
│   ├── backend-linux/   everyfiles-backend-linux     getdents64 + fanotify / inotify
│   ├── backend-windows/ everyfiles-backend-windows   FileIdBothDirectoryInfo + USN / RDCW
│   ├── testkit/         everyfiles-testkit       假扫描器、朴素对照扫描、夹具树
│   └── cli/             everyfiles-cli           `everyfiles` 二进制
└── xtask/                                        `cargo xtask xcheck`
```

依赖方向严格单向：后端 → core；门面 → core + generic + 本机原生后端（通过 `[target.'cfg(...)'.dependencies]` 声明）；CLI → 门面 + testkit。

每个平台后端把全部平台代码放在 `imp.rs` 后面，用 `#[cfg(target_os = "...")] mod imp;` 门控，所以每个 crate 在任何宿主机上都能编译（在非目标平台上编译成空库）。这正是 `cargo xtask xcheck` 能在 Mac 上对 Linux/Windows 后端做类型检查的前提。

门面里的后端注册表 `registry()` 列出本次构建编入的全部后端；`default_backend()` 是 `cfg(target_os)` 选出的原生后端，`--backend generic` 可以在任何机器上强制走可移植路径（用于差分测试）。

## 3. 核心索引结构

代码位于 `crates/core/src/index/`。

### 3.1 记录布局（SoA）

索引是"数组结构体"（struct of arrays）：文件和目录同在一个竞技场里，记录 id 就是在每个数组中的下标。

| 数组 | 类型 | 字节 |
|---|---|---|
| `name_off` | `u32` | 4 |
| `name_len` | `u16` | 2 |
| `fold_off` | `u32` | 4 |
| `fold_len` | `u16` | 2 |
| `parent` | `u32` | 4 |
| `size` | `u64` | 8 |
| `mtime`（纳秒） | `i64` | 8 |
| `flags` | `u8` | 1 |
| 合计 | | **33** |

另有两个字节池：原始文件名池和大小写折叠后的文件名池。`fold_off/fold_len` 独立存储，因为 Unicode 简单折叠会改变字节长度（例如 U+212A 开尔文符号折叠为 ASCII `k`，3 字节变 1 字节）。

`flags` 位：`DIR`、`TOMBSTONE`、`SYMLINK`、`HIDDEN`、`SYSTEM`、`REPARSE`、`HAS_SIZE`、`HAS_MTIME`。后两位用来如实表示后端是否真的提供了该字段（Linux 默认扫描不提供，Windows USN 不提供）。

**路径从不存储。** 需要展示某条记录时，沿 `parent` 一路走到根，拼接各级名字即可，且只对即将展示的那一页记录做这件事。

100 万条记录、平均文件名 22 字节时，元数据 33 MB + 两个名字池各 22 MB ≈ 81 MB；实测合成数据约 65 MiB。`crates/core/tests/memory.rs` 用计数分配器把这个算术变成了断言。

### 3.2 两条不变量

- **INV-1：对任意非根记录 r，`parent(r) < r`。** 装配器在列目录之前就为目录分配 id，所以子记录的 id 一定大于父记录。它保证树无环、路径重建必然终止、压缩只需单次正向遍历。
- **INV-2：被标记删除（tombstone）的目录，其整棵子树都被标记删除。** `Updater::remove_subtree` 负责维持；`compact()` 用 `debug_assert!` 检查。

### 3.3 名字池的两个约束

- 每个名字以 NUL 结尾写入池中。查询串绝不含 `0x00`，所以一次子串匹配的命中不可能跨越两个相邻名字。
- 名字绝不跨越 1 MiB 的块边界（写入方宁可填充到下一块）。块尾未用字节为零，等价于更多 NUL 分隔符。

查询引擎对整块做一次 SIMD 扫描、再把命中偏移线性合并回记录 id，全靠这两点成立。

### 3.4 块、零拷贝与写时复制

数组和名字池都由固定大小的块组成（`Block<T>`）。一个块要么是我们自己持有的 `Arc<[T]>`，要么是指向只读 mmap 的视图。追加元素只会重新分配"块指针的向量"，从不移动块本身，因此读者手里的切片在块存活期间始终有效。

`Index: Clone` 是廉价的结构性克隆：块被共享，克隆只是若干引用计数加一。写入经过 `Block::as_mut`：如果块被某个快照共享或者是 mmap 视图，先复制成私有块（写时复制）。这就是发布读快照几乎免费、而且底层文件永远不会被写穿的原因。

### 3.5 子节点查找

`ChildIndex` 回答"目录 P 下名为 X 的记录是哪条"。基础部分是按 `(parent, name)` 排序的存活记录 id 排列（100 万条约 4 MB），"P 的所有子节点"和"P 下名为 X 的子节点"都是二分查找；基础构建之后新增的记录放在一个按父目录分组的小哈希表里，查找时先查它。被标记删除的基础记录在查找时直接跳过，所以删除不需要改动排列，压缩时整体重建即可。

### 3.6 压缩

`Index::compact()` 单次正向遍历：跳过 tombstone，为存活记录重新编号，`parent` 改写为 `remap[parent]`。由于 INV-1，遍历到某条记录时其父记录必已重编号；由于 INV-2，该父记录必然存活。

## 4. 扫描与树的装配

代码位于 `crates/core/src/scan/`。

### 4.1 后端接口

```rust
pub trait Scanner: Send + Sync {
    fn capabilities(&self) -> ScanCaps;                  // 静态能力
    fn probe(&self, root: &Path) -> Result<ProbeReport>; // 此刻在此根上实际能用哪条路径
    fn root_info(&self, root: &Path) -> Result<RootInfo>;// 根目录校验与元数据
    fn volume_id(&self, root: &Path) -> Result<VolumeId>;// 卷身份，默认 none
    fn stat_entry(&self, path: &Path) -> Result<Option<EntryStat>>; // 抽查用，默认"不能"
    fn read_dir(&self, dir: &Path, out: &mut Vec<RawEntry>) -> Result<()>; // 列一个目录
    fn enumerate(&self, root: &Path, sink: &mut dyn EntrySink) -> Result<()>; // 整卷流，默认不支持
}
```

`RawEntry` 是后端"观察到"的一条目录项：名字字节、种类、`Option<size>`、`Option<mtime>`、可选的原生 id。`Option` 让后端可以诚实地说"我不知道"，核心绝不会为了填充某个字段而逐文件 `stat`。

**名字字节的契约：** `RawEntry::name` 必须恰好是 `OsStr::as_encoded_bytes()` 的结果（统一通过 `scan::name_bytes` 取得）。核心把存储的字节变回路径时使用 `os_str_from_bytes`，它是 `unsafe` 的，只有在这个契约下才是健全的。索引文件会记录编码（Unix 原始字节 / Windows WTF-8），跨平台加载会被拒绝。

### 4.2 并行装配

`build_index` 的结构是：N 个枚举线程从一个共享 FIFO 队列（`crossbeam_deque::Injector`）取目录，调用后端 `read_dir`，把子项批量发给**唯一的**装配线程；装配线程分配 id、把名字写进池、追加记录，并把新发现的子目录压回队列。

两个细节决定了正确性：

- INV-1 由构造保证：目录的 id 在其父目录的批次到达时分配，一定早于任何线程去列它。
- 终止计数 `pending` 只由装配线程递减，而且是在把该目录的子目录全部入队**之后**，所以计数不可能在还有工作在途时归零。

队列是 FIFO 而不是 LIFO：浅而宽的树（文件系统的常态）第二层会立刻铺满所有核心，而不是一个子树被钻到底、其余 999 个兄弟干等。

目录读取失败（权限不够、途中消失）只计数不致命。`max_depth` 到达上限时该层目录仍被记录，只是不再深入。

## 5. 查询引擎

代码位于 `crates/core/src/query/`。

### 5.1 子串匹配

查询默认大小写不敏感：查询串和索引里的名字用同一套折叠函数（`fold.rs`）处理。ASCII 走 `| 0x20` 快路径，非 ASCII 用 `std` 的 Unicode 小写映射（近似简单折叠），非法 UTF-8 字节原样保留。

匹配不是对每个名字各调一次 `memmem`（20 字节的名字上，调用开销占大头），而是对折叠名字池的每个 1 MiB 块做**一次** `memchr::memmem` SIMD 扫描，块再切成 128 KiB 的任务交给 rayon。每个任务多扫 `needle.len() - 1` 字节以免漏掉跨任务边界的命中，并丢弃起点越过自己边界的命中；一个跨边界的名字可能被两个任务各报一次，所以结果最后做一次相邻去重。

命中偏移映射回记录 id 是**线性合并**而非逐命中二分：因为 `fold_off` 随 id 单调递增，而命中偏移也单调递增。对于像 `a` 这种命中 70 万个名字的查询，这一点决定了是 3 ms 还是 30 ms。

小索引（少于 32K 条）直接在调用线程上扫，不启动线程池。

### 5.2 通配符

查询串含 `*` 或 `?` 时按**整名**通配匹配（`glob.rs`）。`*` 匹配任意长度，`?` 匹配一个 UTF-8 字符（名字是合法 UTF-8 时），否则一个字节。匹配器是贪心双指针加回溯到最近一个 `*` 的经典算法。

通配模式里最长的一段连续字面量必然原样出现在任何匹配的名字中，所以它可以作为 SIMD 预过滤：先用它跑子串扫描，只对幸存者跑完整匹配器。字面量段不足 2 字节的模式（如 `*a*`）退化为对每条记录跑匹配器，正确但慢。`glob.rs` 的测试把匹配器与一个朴素递归参考实现在小字母表上做了穷举比对。

### 5.3 过滤、排序、分页

过滤条件（tombstone、种类、size 区间、mtime 区间、隐藏）编译成一个按代价从低到高排列的谓词，对每个命中执行一次，不存在对内存的第二遍扫描。

排序对**匹配结果**排序而不是预建排序数组；只需要一页时先用 `select_nth_unstable` 取前 K 再排序，100 行的页面不会去排 100 万个命中。按路径排序需要重建每个命中的路径，是明确记录的昂贵选项。

路径重建只对最终那一页进行。

## 6. 持久化格式

代码位于 `crates/core/src/store/`。

文件是扁平、小端、8 字节对齐的"零反序列化"格式：

```
头部（256 字节）  魔数 "EVFILES\0"、格式版本、字节序标记、计数、
                  段表位置、字符串段位置、uuid、负载哈希、头部哈希
各段（8 字节对齐）NAME_POOL FOLD_POOL NAME_OFF NAME_LEN FOLD_OFF FOLD_LEN
                  PARENT SIZE MTIME FLAGS STRINGS
段表              每项 32 字节：kind、elem_size、align、off、len
```

字符串段是三个 `[u32 len][bytes]`：根路径、卷身份（kind 字节 + 数据）、监听器的续传令牌。

**加载**（`load.rs`）：mmap 文件，校验头部哈希，读段表，对每个段做边界与对齐检查，然后直接把切片当作 `Chunked<T>` 的 mmap 块。所有整块都是零拷贝视图，只有末尾不满一块的部分被复制一份以便继续追加。100 万条记录的 66 MB 索引实测 0.26 ms 可用。

负载哈希覆盖头部之后的全部字节，约 80 MB 要 6～10 ms，不在加载时强制校验；`Loaded::verify_payload()` 由调用方决定何时（可放后台线程），`everyfiles verify` 总是校验。

**保存**（`write.rs`）是原子的：写同目录临时文件 → `fsync` → `rename` 覆盖 → `fsync` 目录。崩溃时要么保留旧文件，要么得到完整的新文件，不会出现撕裂文件。

头部里的字节序标记让大端机器拒绝加载；编码标记让在另一种路径编码的平台上写出的索引被拒绝（这是 `os_str_from_bytes` 保持健全的前提之一）。

## 7. 引擎：打开、过期判定、增量更新

代码位于 `crates/core/src/engine/`。

### 7.1 `open`

`engine::open` 在索引文件新鲜时加载它，否则重新扫描并保存。满足以下任一条即重扫：

1. 文件缺失、损坏，或由不兼容的版本写出；
2. 存储的卷身份与当前不同（两者都已知时）；
3. 根目录不再是可读目录；
4. **抽查失败**：随机抽 16 条存活记录去 `stat`，种类、size、mtime 任一不符或文件已消失。16 次系统调用就能抓住重装系统、从备份恢复、换盘这类情形，也是 Linux 那个较弱的卷身份能被接受的原因；
5. 调用方要求。

在监听器运行之前，抽查失败意味着整体重扫。这是刻意的：过期的索引绝不能给出错误答案。

### 7.2 `Updater`：以路径为单位的调和

增量更新只有一个原语：`reconcile_path(path, deep)`——看索引认为这条路径是什么，看文件系统说它是什么，修正差异。

- 索引里有、磁盘上也有：种类变了（目录变文件）就删子树再加；否则更新元数据，目录且 `deep` 时重新列出子项。
- 索引里有、磁盘上没有：删除整棵子树（维持 INV-2）。
- 索引里没有、磁盘上有：从第一个缺失的组件起加入，目录递归列出。
- 路径在根之外：忽略。

所有监听器事件都归结为这一个原语，所以事件标志被合并、重命名事件乱序到达都不会出错——事件只是提示，文件系统才是事实。

`sweep()` 是追赶机制：从根开始，只重新列出 mtime 与存储值不符（或未知）的目录，但无论如何都递归进子目录。代价 O(目录数)。它看不到"目录没动、但里面某个文件的 size 变了"，这由在线监听器覆盖，是记录在案的局限。

`maybe_compact()` 在 tombstone 超过 `max(10000, 记录数/8)` 时压缩并重建 `ChildIndex`。

### 7.3 `watch_loop`

循环：等事件 → 在防抖窗口内继续收集 → 逐条 `reconcile_path`（`Overflow`/`RescanNeeded` 则 `sweep`）→ 视情况压缩 → 通过 `arc-swap` 发布 `updater.snapshot()`（廉价克隆）→ 按节奏把索引连同续传令牌一起持久化。读者随时 `load_full()` 得到一个一致的快照，不加任何锁。

### 7.4 监听接口

```rust
pub enum WatchEvent {
    Path { path: PathBuf, kind: EventKind, deep: bool },
    Overflow,       // 后端丢了事件：全根扫一遍
    RescanNeeded,   // 后端状态不可用（日志回绕、身份变化）
}

pub trait Watcher: Send {
    fn capabilities(&self) -> WatchCaps;
    fn probe(&self, root: &Path) -> Result<ProbeReport>;
    fn start(&mut self, root: &Path, since: Option<&ResumeToken>) -> Result<()>;
    fn poll(&mut self, timeout: Duration, out: &mut Vec<WatchEvent>) -> Result<PollOutcome>;
    fn resume_token(&self) -> ResumeToken;
    fn stop(&mut self) -> Result<()>;
}
```

续传令牌是后端自定义编码的不透明字节串，核心原样持久化、原样交还。无法兑现令牌的后端**必须**返回 `Error::TokenStale`，调用方随即改为 `sweep`，而不是信任一次残缺的回放。

每个平台都有"我丢事件了"的信号，忽略它是索引器悄悄出错的头号原因，所以 `Overflow`/`RescanNeeded` 是接口的一等公民。

## 8. 平台后端

### 8.1 能力矩阵

| 平台 | 非特权扫描 | 非特权监听 | 特权层 | 跨重启续传 |
|---|---|---|---|---|
| macOS | `getattrlistbulk(2)`：一次调用得到名字、类型、size、mtime、inode | FSEvents | 无需 | **有**：`(卷 UUID, 最后事件 id)` |
| Linux | `getdents64`：默认只有名字和类型；`--with-stat` 加一遍 `statx` | inotify，每目录一个 watch，感知配额 | fanotify `FAN_MARK_FILESYSTEM`（root） | 无：启动时做 mtime sweep |
| Windows | `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`：批量得到名字、size、mtime、属性、文件 id | `ReadDirectoryChangesW` | USN 日志（管理员） | **有**（USN）：`(日志 id, NextUsn)`；RDCW 需 sweep |
| 任意 | `std::fs::read_dir` + `lstat`（其他后端的对照基准） | 周期性 sweep | — | 无 |

### 8.2 macOS（`crates/backend-macos`）

**扫描**用 `libc` 已声明的 `getattrlistbulk`。几个从 SDK 头文件核实过、容易踩坑的事实：

- 返回值是**条目数**而不是字节数；返回 0 表示结束，且在 fd 回绕或重开之前一直返回 0。
- 每个条目组以 `u32` 长度开头（8 字节对齐），接着固定是 `ATTR_CMN_RETURNED_ATTRS`（一个 `attribute_set_t`），请求了 `ATTR_CMN_ERROR` 的话它紧随其后，然后各属性按位序排列，变长数据放在组末尾。
- `attrreference_t.attr_dataoffset` 相对于**该结构体自身**，不是相对于缓冲区。
- 字段只保证 4 字节对齐，所以解析全部用字节切片 `from_ne_bytes`，整个解析器没有 `unsafe`。
- `libc` 没有 `ATTR_CMN_ERROR`（0x20000000）和 `fsobj_type_t` 的取值（VREG=1、VDIR=2、VLNK=5），在 `sys.rs` 里本地声明。
- bulk 调用要求 `volattr` 为 0；`ATTR_DIR_ENTRYCOUNT` 在非 HFS+ 上很贵，不请求。
- 不传 `FSOPT_PACK_INVAL_ATTRS`，解析器按**返回**的属性掩码走，而不是按请求的。

文件系统拒绝（`ENOTSUP`/`EINVAL`/`ERANGE`）时该目录退到通用后端。内核无法 stat 的条目（`ATTR_CMN_ERROR` 非零）单独 `lstat` 补齐。

**firmlink**：`/usr/local` 这类目录是密封系统卷上的桩，指向数据卷。列父目录得到的是桩自己的 mtime（等于系统快照时间），而 `lstat` 会跟随到数据卷。目录**内容**枚举无误，只有目录自身的 mtime 不一致，第一次 sweep 会用 `lstat` 值覆盖。`verify` 把这类目录 mtime 差异作为信息性输出，不算失败。

**监听**用手写 FFI 的 FSEvents（`fsevents.rs`），不用 crate：`fsevent-sys` 已弃用且没有 `FSEventsCopyUUIDForDevice`，`notify` 会丢弃事件 id。要点：

- 事件 id 在**整个系统**上单调递增且跨重启，事件数据库按卷存储。所以 `(卷 UUID, 最后 id)` 是合法的续传令牌，只要 UUID 仍匹配，流会先回放历史事件、发一个 `HistoryDone`、再转入实时。
- 不用 `kFSEventStreamCreateFlagUseCFTypes`，回调直接收到 `char**` 的 UTF-8 路径，热路径上无需解析 CF 对象。
- `MustScanSubDirs`/`UserDropped`/`KernelDropped`/`EventIdsWrapped`/`RootChanged` 分别映射为 `deep`、`Overflow`、`RescanNeeded`。
- 流必须 `SetDispatchQueue` 后再 `Start`，回调在 GCD 队列上执行；`stop` 时 Stop → Invalidate → Release，之后才释放泄漏给回调的状态。

内核上报的是真实路径，所以通过符号链接给出的根（macOS 的 `/var` → `/private/var`）会导致事件落在根之外。CLI 对根做了 `canonicalize`。

**TCC**：没有"完全磁盘访问权限"时受保护目录会被静默跳过，索引是不完整而不是报错，`stats` 无法检测这一点。

### 8.3 Linux（`crates/backend-linux`）

**扫描**用 `rustix::fs::Dir`（封装 `getdents64`），`d_type` 直接取自 dirent，不会偷偷 `stat`；整个扫描器没有 `unsafe`。目录以 `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` 打开，`statx` 经由目录 fd 进行，所以并发重命名不会把遍历引到别处。`d_type == DT_UNKNOWN`（无 `ftype` 的 XFS、不少 NFS）的条目单独 `statx(STATX_TYPE)` 一次，其余条目绝不 stat。

默认只索引名字和类型，`ScanCaps::provides_size/mtime = false`；`--with-stat` 开启逐条 `statx(AT_STATX_DONT_SYNC)`。卷身份是 `f_type + st_dev + root inode` 的启发式组合，没有 libblkid 就拿不到真正的文件系统 UUID，所以引擎的抽查承担了主要责任。

**监听**：`LinuxWatcher` 在 `start` 时自动选层——

- **fanotify**（`fanotify.rs`，需要 root）：一个 `FAN_MARK_FILESYSTEM` 标记覆盖整个文件系统，没有逐目录 watch 配额，启动时也不用遍历树。`FAN_REPORT_DFID_NAME` 让事件携带父目录的 file handle 和条目名，`open_by_handle_at` + `/proc/self/fd` 还原路径（有界缓存）。`FAN_MARK_FILESYSTEM` 需要 `CAP_SYS_ADMIN`，`open_by_handle_at` 需要 `CAP_DAC_READ_SEARCH`；内核要求 4.20（`FAN_MARK_FILESYSTEM`）和 5.9（`FAN_REPORT_DFID_NAME`）。5.13 起非特权进程虽可创建 FID 组但仍不能标记整个文件系统，所以这条路径没有非特权变体。常量按内核 UAPI 本地声明，因为 `libc` 缺好几个，`rustix` 则完全没有 fanotify。
- **inotify**（`inotify.rs`）：非递归，启动时遍历树为每个目录加 watch，新建/移入的目录到达时补加。`/proc/sys/fs/inotify/max_user_watches` 是硬配额，加不上的目录会被计数，只要存在未覆盖目录就每分钟发一次 `RescanNeeded` 让引擎 sweep 补洞。队列溢出以 `wd == -1` 的事件到达，映射为 `Overflow`。不做 cookie 配对，引擎会分别调和新旧两条路径。

两者都没有回放能力，启动时总是 sweep。

### 8.4 Windows（`crates/backend-windows`）

**扫描**用 `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`：每目录一次调用批量得到名字、size、mtime、属性、文件 id，不需要提权，NTFS/ReFS/FAT 都可用。`FILE_ID_BOTH_DIR_INFO` 链按 `NextEntryOffset` 逐项前进，每一步都对缓冲区做边界检查，字段用 `read_unaligned` 读取（内核只保证 8 字节对齐）。重解析点（junction、符号链接）记录为 `Symlink` 且不深入。路径一律加 `\\?\` 前缀以突破 `MAX_PATH`。卷身份是 `GetVolumeInformationW` 的序列号加文件系统名。

**监听**：`WindowsWatcher` 在 `start` 时自动选层——

- **USN 日志**（`usn.rs`，需要管理员）：打开 `\\.\C:` 卷设备，`FSCTL_QUERY_USN_JOURNAL` 取 `UsnJournalID/FirstUsn/NextUsn`，`FSCTL_READ_USN_JOURNAL` 以 `BytesToWaitFor = 1` 阻塞读取。续传令牌是 `(日志 id, NextUsn)`：日志 id 变了、或位置落到 `FirstUsn` 之前（日志回绕）都判为 `TokenStale`。USN 记录只有条目名和父目录的文件引用号，没有 size 和 mtime，`TimeStamp` 是变更时间不是文件 mtime；父目录路径用 `OpenFileById` + `GetFinalPathNameByHandleW` 解析并缓存。`USN_RECORD_V2/V3` 按文档偏移手工解析，不依赖结构体 ABI。`stop` 用 `CancelIoEx` 解除阻塞。
- **ReadDirectoryChangesW**（`watcher.rs`）：专用线程阻塞调用，解析 `FILE_NOTIFY_INFORMATION` 链；`ERROR_NOTIFY_ENUM_DIR` 或返回 0 字节表示缓冲区溢出，映射为 `Overflow`。不可续传，启动时 sweep。

**明确未实现**：基于 `FSCTL_ENUM_USN_DATA` 的整卷初始枚举。扫描走上面的非特权遍历（它本来就提供 size/mtime），USN 只用于可续传监听。原始 MFT 解析永久不在范围内。

### 8.5 通用后端（`crates/backend-generic`）

`std::fs::read_dir` 加每条目一次 `symlink_metadata`，刻意写成最慢但最显然正确的实现。它是所有原生后端差分测试的对照方、是降级的终点，也是三个平台上行为完全一致的唯一扫描器。它的监听器是周期性 sweep，也是追赶机制的参考实现。

## 9. 命令行工具

```
everyfiles index  <path> [--max-depth N] [--json]
everyfiles search <query> --root <path> [--limit N] [--offset N] [--sort name|size|mtime|path] [--desc]
                  [--files-only|--dirs-only] [--case-sensitive] [--exact]
                  [--min-size 10K] [--max-size 4M] [--after -7d] [--before 2024-02-29]
                  [--hidden] [-o human|tsv|jsonl|json|null] [--stat] [--force-rescan] [--no-verify]
everyfiles watch  <path> [--debounce-ms 500] [--save-every 30] [--no-sweep] [--once]
everyfiles stats  <path> [--json]
everyfiles verify <path> [--json] [--examples N]
everyfiles bench  [--records 1000000] [--query Q]... [--iterations N] [--json]
everyfiles backends
全局：--backend auto|native|generic|<name>  --index <file>  --threads N  --with-stat  --poll-interval S
```

实现要点：

- 所有命令对根做 `canonicalize`（Windows 下再去掉 `\\?\` 前缀），原因见 8.2。
- `search` 与 `stats` 通过 `engine::open` 打开索引，新鲜则加载，否则重扫并保存；`index` 总是重扫。默认索引文件在系统缓存目录，以根路径的 xxh3 指纹命名。
- `watch` 在 `open` 之后：如果后端可续传且索引里带令牌，就以令牌 `start`（回放缺失的事件）；令牌失效或后端不可续传则先 `sweep`。之后进入 `watch_loop`，每批打印 `+新增 -删除 ~更新`，并按节奏持久化。
- `verify` 用所选后端重扫，与 `testkit::oracle::naive_scan`（walkdir + lstat）比对路径集合、种类、文件 size 与 mtime。两边都读不到的条目跳过并计数；目录 mtime 的差异单独列为信息性输出（firmlink）。
- `bench` 用 `GeneratedScanner` 生成任意规模的过程式树（不占内存），报告 p50/p95/p99。

## 10. 测试与验证

### 10.1 测试层次

- **单元测试**：折叠函数、块与池的边界（正好填满一块、跨块、追加后旧切片地址不变）、INV-1、路径重建、tombstone 与压缩、持久化往返、头部逐字节翻转必被检出、通配符匹配器与朴素递归实现在小字母表上穷举比对。
- **布局证明测试**：每个原生扫描器把真实临时目录的解析结果与 `std::fs` 逐项比对（名字、种类、size、纳秒 mtime、inode）。这是把"猜的缓冲区布局"变成"验证过的布局"的那一步，在对应平台的 CI 上运行。macOS 还有一个 3 万文件的单目录测试，强制一次目录经历多次内核调用。
- **差分对照测试**：最重要的一类。用某个后端建索引，与 `naive_scan` 比对存活路径集合和元数据；再通过监听器施加一串变更（创建、删除、重命名、删子树），断言索引与一次全新扫描一致。每个后端在自己平台上跑，`everyfiles verify` 复用同一份代码。
- **纯内存测试**：`SyntheticScanner` 是可变的内存树，实现了 `Scanner` 的全部方法（包括 `stat_entry`），所以装配、更新、sweep、压缩都能在没有文件系统的情况下测试。它放在 core 的 `testing` 特性下而不是 testkit 里，是为了避免开发依赖环导致 trait 出现两个实例。
- **内存测试**：`tests/memory.rs` 用计数全局分配器测量 100 万条记录的真实占用，断言落在 `33 × n + 名字池` 加一块粒度的松弛范围内，并要求 `approx_bytes()` 的自报数字与实测相差不超过 5%。
- **基准**：`benches/query.rs`（各类 needle 的查询延迟）、`benches/store.rs`（保存、加载、加载并校验）。

### 10.2 跨平台类型检查

```sh
cargo xtask xcheck
```

对 Linux/Windows 后端在各自目标三元组上执行 `cargo check --lib`。它证明 FFI 的名字、签名、结构体字段和 `cfg` 门控都正确，**但不证明** ABI 布局、ioctl 编号或运行时行为——那些由 CI 的 Ubuntu/Windows 任务覆盖。只检查 `--lib`，因为 `criterion` 间接依赖的 `alloca` 带 C 构建脚本，在没有交叉 C 工具链的机器上无法为外来目标构建。

### 10.3 CI

`.github/workflows/ci.yml`：`fmt` + `clippy -D warnings`；在 ubuntu / macos-14 / windows 上跑 `cargo test --workspace` 和 `verify`；Linux 上额外以 `sudo` 运行 `#[ignore]` 的 `root_*` fanotify 测试；macOS 上跑 `xcheck`；基准编译并各跑一次。

## 11. 开发约定

- **core 不碰平台**：不加 `cfg(target_os)`，不加平台 crate，不做文件系统 I/O。
- **不引入带 C 构建脚本的依赖**，否则 `xcheck` 失败（`blake3` 因此被 `xxhash-rust` 取代）。优先 `rustix`，只有没有合适纯 Rust 绑定时才手写 FFI（FSEvents、fanotify）。
- **每个 `unsafe` 块都有 `// SAFETY:` 注释**写明依赖的不变量。内核缓冲区一律在字节切片上做边界检查后解析，用 `read_unaligned` 读值，不把缓冲区强转为结构体引用。
- **平台常量来自 SDK 头文件或内核 UAPI 并注明来源**，不凭记忆。`libc` 缺 `ATTR_CMN_ERROR`、`fsobj_type_t` 取值和大部分 `FAN_*`，所以一些常量在本地声明。
- **能力如实上报**：`ScanCaps::provides_size/mtime` 和 `HAS_SIZE/HAS_MTIME` 必须反映后端真正提供了什么；`probe()` 要说明当前生效的路径和降级原因。
- **后端自动降级，绝不在存在非特权路径时硬失败。**
- 修改名字池或数组布局时，重新审视第 3.3 节的两个约束和查询引擎的合并逻辑。

## 12. 实测性能

开发机（Apple Silicon，APFS），release 构建：

| 项目 | 结果 |
|---|---|
| 约 100 万条合成记录的索引 | 常驻 65 MiB |
| 查询（100 万条，典型 needle） | 0.2 ～ 1 ms |
| 查询（100 万条，单字母命中 49 万条） | 3.2 ms |
| 加载 100 万条索引（66 MB 文件） | 0.26 ms（零拷贝 mmap） |
| 加载并校验负载哈希 | 6.6 ms |
| 保存 100 万条 | 26 ～ 57 ms |
| 扫描 `/usr`（77,256 条）：原生 vs 通用 | 99 ms vs 632 ms |
| `verify /usr` | 77,255 条两侧一致，0 缺失、0 多余、0 不符 |

## 13. 已知缺口与后续工作

**刻意的缺口**（见 README "Known gaps"）：Linux 默认只索引名字；Windows 未实现 USN 整卷枚举；macOS 的 TCC 与 firmlink 行为；inotify watch 配额；只做简单大小写折叠；不支持正则；无字面量段的通配模式走慢路径；mtime sweep 看不到未变动目录内的文件 size 变化。

**计划中但未做的优化**（都不影响正确性，按真实语料的剖析结果决定是否做）：

- 三元组（trigram）倒排预过滤——100 万名字约 +60 MB，只有索引规模超过约 2000 万条、线性扫描超过 50 ms 时才值得。
- 预建的按名字/size/mtime 排序排列（+12 MB），仅在用户真要翻阅几十万条结果时有收益。
- 把 `ChildIndex` 持久化进索引文件，省去 `watch` 启动时那一次排序（100 万条约 150 ms）。
- 对已是小写 ASCII 的名字省略折叠池（约 −22 MB）。
- Linux 自带缓冲区的 `getdents64` 循环，以及用 `d_off` 切分超大单目录。
- Windows 基于 `FSCTL_ENUM_USN_DATA` 的整卷枚举（需要 core 支持 `ScanMode::VolumeWide` 的装配）。
