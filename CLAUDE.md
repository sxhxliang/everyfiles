# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`everyfiles` is an Everything-style filename indexer: a portable in-memory
index and query engine (`everyfiles-core`) behind two traits, with one backend
crate per platform providing only the fastest native scan and change-watch
calls. See `README.md` for the capability matrix and measured numbers.

Edition 2024, `rust-version = 1.98`. Workspace of nine crates under `crates/`
plus `xtask/`.

## Commands

```sh
cargo test --workspace                                  # unit + integration; macOS-only tests run on macOS
cargo clippy --workspace --all-targets -- -D warnings   # CI gate
cargo fmt --all -- --check                              # CI gate
cargo xtask xcheck                                      # cross-target type check of backends (see below)
cargo bench -p everyfiles-core                          # criterion: benches/query.rs, benches/store.rs
cargo run --release -p everyfiles-cli -- verify <dir>   # differential check vs naive walk
cargo test -p everyfiles-core --lib query::             # one module's tests
```

CI (`.github/workflows/ci.yml`): fmt + clippy; `cargo test --workspace` and
`verify` on ubuntu / macos-14 / windows; a `sudo` step runs the `#[ignore]`d
`root_*` fanotify test on Linux; `xcheck` on macOS; benches compile and run
once.

## Architecture

Dependency direction is strictly one-way: backends → `everyfiles-core`;
`everyfiles` (facade) → core + generic + the host's native backend (via
`[target.'cfg(...)'.dependencies]`); `everyfiles-cli` → facade + testkit.

- **`crates/core`** has **no `cfg(target_os)` and no platform dependency**.
  Platform facts arrive as values (`platform::VolumeConfig`) or through traits.
  - `index/` — SoA arrays (`Chunked<T>`) and NUL-terminated name pools
    (`NamePool`), both built from `Block<T>` = owned `Arc<[T]>` or a view into an
    mmap. `Index: Clone` is cheap and structural; writes copy shared blocks
    (COW). `ChildIndex` answers "child of P named X". `compact()` is one pass.
  - `scan/` — `trait Scanner`, `RawEntry`, and `assemble::build_index`: N
    enumerator threads over a FIFO `Injector`, one assembler thread allocating
    ids. The assembler decrements the pending counter only *after* queueing a
    directory's children.
  - `query/` — `search` (pool scan + offset merge in `matcher.rs`; whole-name
    globs in `glob.rs`, prefiltered by their longest literal run), filters,
    `sort_ids`, `truncate_top` (select_nth), `page`.
  - `store/` — on-disk format (`header.rs`), atomic `save` (`write.rs`),
    zero-copy `load` (`load.rs`). xxh3 checksums.
  - `engine/` — `open` (load if fresh else rescan+save, with the spot-check),
    `Updater` (reconcile-by-path incremental updates, sweep, compaction),
    `watch_loop` (drain → reconcile → publish snapshot via `arc-swap` → persist).
  - `watch/` — `trait Watcher`, `WatchEvent`, `ResumeToken`.
  - `testing.rs` (feature `testing`) — `SyntheticScanner` (mutable, in-memory)
    and `GeneratedScanner` (procedural, any size, no memory). The testkit
    re-exports these; they live in core to avoid a dev-dependency cycle.
- **Backends** gate everything on `cfg(target_os)` inside `imp.rs` so every
  crate compiles (to an empty lib) on every host. Each has a `sys.rs`/FFI layer
  owning the `unsafe`, a scanner, and a watcher; Linux and Windows wrap two
  watcher tiers in an auto-selecting type (`LinuxWatcher`, `WindowsWatcher`)
  that picks the privileged one at `start` when available.

## Invariants and contracts (do not break)

- **INV-1:** `parent(r) < r` for every non-root record. Guaranteed by the
  assembler allocating a directory's id before listing it. Path reconstruction
  and compaction rely on it.
- **INV-2:** tombstoning a directory tombstones its whole subtree
  (`Updater::remove_subtree`). Compaction `debug_assert!`s it.
- **Pools are NUL-terminated and names never straddle a 1 MiB block.** The
  query engine's whole-block `memmem` scan and the hit→id merge depend on both.
- **`RawEntry::name` is exactly `OsStr::as_encoded_bytes()`** (use
  `scan::name_bytes`). The core turns stored bytes back into paths with
  `os_str_from_bytes`, which is `unsafe` and sound only under that contract.
  The index file records the encoding and refuses to load across platforms.
- **Every `unsafe` block has a `// SAFETY:` comment** stating the invariant.
  Parsing of kernel buffers (getattrlistbulk groups, `FILE_ID_BOTH_DIR_INFO`
  chains, USN records, fanotify info records) is done on byte slices with
  bounds checks and `read_unaligned`, never by casting to a struct reference.
- **No dependency with a C build script.** It would break `cargo xtask xcheck`
  (criterion's `alloca` is why xcheck checks `core` with `--lib` only; the
  backends are also checked with `--profile test --lib`). Prefer `rustix` over
  `libc` where it covers the call; hand-declare FFI only where no pure-Rust
  binding is adequate (FSEvents, fanotify).

## Verification model

- The **differential oracle test** is the most important test: index a tree,
  compare the live path set and metadata against `walkdir`+`lstat`
  (`testkit::oracle::naive_scan`). It runs per backend
  (`crates/backend-*/tests/`) and is exposed as `everyfiles verify`.
- Each native scanner has a **layout-proof test** comparing parsed entries
  against `std::fs` on a real tempdir. That is what turns a guessed buffer
  layout into a verified one; it runs on the matching OS in CI.
- `crates/core/tests/memory.rs` measures real allocation with a counting
  global allocator and asserts the 33-bytes-per-record arithmetic.
- `cargo xtask xcheck` proves names/signatures/cfg for Linux and Windows from
  any host, including each backend's `#[cfg(test)]` module (`--profile test
  --lib`). It does **not** prove ABI layout or runtime behaviour.

## Conventions

- Backends degrade automatically and `probe()` says which path is active and
  why; never fail hard when an unprivileged path exists.
- Add platform constants from the SDK header or kernel UAPI with the source
  noted, not from memory. `libc` omits several (`ATTR_CMN_ERROR`,
  `fsobj_type_t` values, most `FAN_*`), which is why some are declared locally.
- Honest capability reporting: `ScanCaps::provides_size/mtime` and the
  `HAS_SIZE`/`HAS_MTIME` flags must reflect what the backend actually supplied.
- Keep `core` free of filesystem I/O; even root validation goes through
  `Scanner::root_info`, which is why the synthetic scanner can test everything.
