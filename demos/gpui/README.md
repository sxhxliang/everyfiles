# everyfiles gpui demo

用 [gpui](https://gpui.rs)（Zed 的 GPU 加速 UI 框架）给 everyfiles 引擎套一个最小的窗口：输入即搜索，`⌘P` 切换拼音匹配。

```sh
cd demos/gpui
cargo run --release -- ~/Documents     # 省略参数则索引当前目录
```

第一次打开会扫描并保存索引；索引文件和 `everyfiles` 命令行工具用的是同一个缓存位置，所以之后再打开（或者命令行先 `everyfiles index` 过）就是零拷贝加载，不到一毫秒。

界面：

- 顶部：根目录、拼音开关（默认开）。
- 搜索框：支持中文输入法、粘贴、光标移动与选区；每次编辑都重新查询。
- 状态行：命中数、匹配耗时、索引规模、索引来源；查询不合法时（例如拼音模式下用了 `*`）显示引擎返回的错误。
- 结果：按名字排序的前 300 条，目录名黄色，右侧是所在目录。引擎用 `select_nth` 取前 300，再多的命中也不会全排序。

试试：`pysousuo` → 拼音搜索，`cq` → 重庆，`拼音sousuo` 混排，`report` 走和命令行一样的子串快路径。

按键：`⌘P` 切换拼音 · `esc` 清空 · `⌘A/⌘C/⌘V/⌘X` · `⌘Q` 退出。

这个 crate 有自己的 `[workspace]`，**不是**仓库 workspace 的成员：gpui 的依赖树很大，Linux 上还需要显示服务器，不该进入引擎的 `cargo test --workspace`、`xcheck` 和 CI。它只通过门面 crate `everyfiles`（开启 `pinyin` 特性）使用引擎。搜索框改自 gpui 自带的 `input` 示例。
