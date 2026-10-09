# everyfiles

Instant filename search over an in-memory index, in the spirit of
[Everything](https://www.voidtools.com/): hold every file name on a volume in
memory and answer substring queries in single-digit milliseconds, on macOS,
Linux and Windows.

The engine is one portable crate. Each platform contributes only its fastest
way to list what is on disk and its fastest way to hear about changes, behind
two small traits. Speed lives in the core; the backends never reimplement index
logic.

```
┌───────────────────────────────────────────┐
│ everyfiles-core                           │
│  index · query · store · engine · watch   │  ← portable, no cfg(target_os)
├───────────────────────────────────────────┤
│ trait Scanner            trait Watcher    │
├──────────────┬──────────────┬─────────────┤
│ macOS        │ Linux        │ Windows     │  + generic fallback (std::fs)
│ getattrlist- │ getdents64   │ FileIdBoth- │
│ bulk         │ (+statx)     │ DirectoryInfo│
│ FSEvents     │ fanotify /   │ USN journal │
│ (resumable)  │ inotify      │ / RDCW      │
└──────────────┴──────────────┴─────────────┘
```

## Status

Everything below is implemented and tested. The macOS backend is exercised on
real hardware; the Linux and Windows backends are type-checked for their
targets from any host (`cargo xtask xcheck`) and run on their own OS in CI.

| Platform | Scan (unprivileged) | Watch (unprivileged) | Privileged tier | Resume across restarts |
|---|---|---|---|---|
| **macOS** | `getattrlistbulk(2)`: name, type, size, mtime, inode in one call per directory | FSEvents | none needed | **yes** — `(volume UUID, last event id)` replays missed events |
| **Linux** | `getdents64`: names and types only by default (`--with-stat` adds a `statx` pass) | inotify, one watch per directory, watch-budget aware | fanotify `FAN_MARK_FILESYSTEM` (root) | no — directory-mtime sweep on start |
| **Windows** | `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`: name, size, mtime, attributes, file id in bulk | `ReadDirectoryChangesW` | USN journal (Administrator) | **yes** with USN — `(journal id, next USN)`; RDCW sweeps |
| any | `std::fs::read_dir` + `lstat` (the reference the others are checked against) | periodic sweep | — | no |

Privileged tiers are tried first and degrade automatically; `everyfiles stats`
and `everyfiles watch` print which path is active and why.

## Measured

On this development machine (Apple Silicon, APFS), release build:

| What | Result |
|---|---|
| Index of ~1,000,000 synthetic records | 65 MiB resident (33 B metadata/record + both name pools) |
| Query, 1M records, typical needle | 0.2 – 1 ms |
| Query, 1M records, one letter matching 491k names | 3.2 ms |
| Load a saved 1M-record index (66 MB file) | **0.26 ms** (zero-copy mmap; only partial tail blocks are copied) |
| Load + verify payload checksum | 6.6 ms |
| Save 1M records | 26 – 57 ms |
| Scan `/usr` (77,256 entries), native vs generic | **99 ms vs 632 ms** |

The memory figure is enforced by a test with a counting allocator, not just
computed.

## Usage

```sh
cargo build --release
B=target/release/everyfiles

$B index  ~/Code                      # scan, save to the cache dir, print stats
$B search main.rs --root ~/Code       # loads the saved index if still fresh
$B search report --root ~/Code --files-only --min-size 1M --sort size --desc
$B search "" --root ~/Code -o null | xargs -0 ...   # NUL-separated paths
$B watch  ~/Code                      # keep the index current; prints each batch
$B stats  ~/Code                      # records, memory, coverage, active paths
$B verify ~/Code                      # differential check against a naive walk
$B bench  --records 1000000           # synthetic index + query latencies
$B backends                           # what this binary was built with
```

Output formats: `human` (default on a tty), `tsv` (default when piped),
`jsonl`, `json`, `null`. `--index <path>` overrides the cache location
(`~/Library/Caches/everyfiles`, `$XDG_CACHE_HOME/everyfiles`,
`%LOCALAPPDATA%\everyfiles\cache`). `--backend generic` forces the portable
path on any host.

Queries are case-insensitive substrings by default. `--case-sensitive`,
`--exact`, `--files-only`/`--dirs-only`, `--min-size`/`--max-size`, `--scope`
are supported. Wildcards are **rejected with a clear error** rather than
silently mis-answered.

## How it works

- **Index.** Struct-of-arrays: eight parallel arrays (name offset/len, folded
  offset/len, parent, size, mtime, flags) plus two byte pools, raw names and
  case-folded names, each NUL-terminated. Paths are never stored; one is rebuilt
  by walking `parent` to the root, only for the rows about to be shown.
- **Two invariants.** `parent(r) < r` for every record (the assembler allocates
  a directory's id before listing it), and a tombstoned directory's whole
  subtree is tombstoned. Together they make the tree provably acyclic, path
  reconstruction provably terminating, and compaction a single forward pass.
- **Query.** One SIMD `memmem` scan per 1 MiB block of the folded pool, in
  parallel, then a linear merge of hit offsets back to record ids. Because a
  needle never contains `0x00` a hit can never straddle two names. Filters are
  one cheapest-first predicate per hit. Paging uses `select_nth` so a 100-row
  page never sorts a million hits.
- **Persistence.** A flat little-endian file: 256-byte header, scalar-array
  sections, section table, xxh3 checksums. Loading maps the file and casts
  slices; every full block is served from the mapping, so a 66 MB index is
  usable in a quarter of a millisecond. Saves are atomic (temp, fsync, rename,
  fsync dir).
- **Staleness.** A saved index is reused only if the format, volume identity
  and root all match **and** a 16-record random spot-check against the
  filesystem agrees. Otherwise it is rebuilt. Until the watcher is running, a
  changed tree therefore means a rescan: a stale index never serves wrong
  answers.
- **Updates.** Every change notification becomes *reconcile this path against
  the filesystem*. That is robust to coalesced flags and rename ordering. Loss
  signals (`MustScanSubDirs`, `IN_Q_OVERFLOW`, journal wrap,
  `ERROR_NOTIFY_ENUM_DIR`) become a directory-mtime sweep. Readers get
  snapshots through `arc-swap`; blocks are shared and copied on write, so
  publishing a snapshot is a few reference-count bumps.

## Known gaps (deliberate)

- **Linux indexes names only by default.** `getdents64` yields nothing else and
  Linux has no MFT equivalent. `--with-stat` adds size/mtime at the cost of a
  much slower first scan. Linux volume identity is a heuristic; the spot-check
  carries the weight.
- **Windows USN enumeration is not implemented.** Scans use the unprivileged
  walk, which already supplies size and mtime. The journal is used for
  resumable watching only. Raw MFT parsing is out of scope permanently.
- **macOS Full Disk Access.** Without it, TCC silently withholds protected
  directories; the index is incomplete rather than erroring. `stats` cannot
  detect this.
- **macOS firmlinks.** For stubs like `/usr/local`, `getattrlistbulk` reports
  the sealed system volume's mtime for the directory itself; contents are
  indexed correctly and the first sweep corrects the directory's mtime.
  `verify` reports such directory-mtime differences as informational.
- **inotify watch budget.** `/proc/sys/fs/inotify/max_user_watches` caps
  watched directories; uncovered ones trigger periodic sweeps instead.
- **Simple case folding only.** `ß` does not match `ss`.
- **No globs or regex yet.** Literal substrings only.
- The directory-mtime sweep cannot see a size change to a file inside an
  otherwise untouched directory; the live watcher covers that case.

## Development

```sh
cargo test --workspace                       # 90+ tests; macOS-only ones run here
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask xcheck                           # type-check Linux/Windows backends from any host
cargo bench -p everyfiles-core               # criterion: query + store
```

`cargo xtask xcheck` proves FFI names, signatures, struct fields and cfg
gating for the targets this host cannot run. It does **not** prove ABI layout,
ioctl numbers or runtime behaviour; CI's `test` matrix on Ubuntu, macOS and
Windows is where those are covered. Every platform constant in this repo was
read from the SDK headers or the vendored crate sources rather than assumed.

No dependency may carry a C build script: that is what keeps cross-target
checking possible. (`blake3` was rejected for exactly this reason; checksums
are xxh3.)

## Layout

```
crates/core             everyfiles-core        engine: index, query, store, engine, watch
crates/everyfiles       everyfiles             facade: registry, default backend, cache path
crates/backend-generic  everyfiles-backend-generic   std::fs reference + polling watcher
crates/backend-macos    everyfiles-backend-macos     getattrlistbulk + FSEvents
crates/backend-linux    everyfiles-backend-linux     getdents64 + fanotify/inotify
crates/backend-windows  everyfiles-backend-windows   FileIdBothDirectoryInfo + USN/RDCW
crates/testkit          everyfiles-testkit     synthetic scanners, naive oracle, fixtures
crates/cli              everyfiles-cli         the `everyfiles` binary
xtask                                          cargo xtask xcheck
```

License: MIT OR Apache-2.0.
