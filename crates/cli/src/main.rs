//! `everyfiles` — instant filename search over an in-memory index.

mod fmt;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use everyfiles::query::{self, KindFilter, Query, SortKey};
use everyfiles::scan::Kind;
use everyfiles::{
    BackendOptions, Id, Index, OpenOptions, OpenOutcome, ScanOptions, ScanStats, build_index,
    host_volume_config,
};
use everyfiles_testkit::{GeneratedScanner, oracle};

#[derive(Parser, Debug)]
#[command(name = "everyfiles", version, about, long_about = None)]
struct Cli {
    /// Backend: `auto`, `native`, `generic`, or a backend name (see `backends`).
    #[arg(long, global = true, default_value = "auto")]
    backend: String,
    /// Enumerator threads (default: all cores).
    #[arg(long, global = true)]
    threads: Option<usize>,
    /// Index file to use instead of the default cache location.
    #[arg(long, global = true)]
    index: Option<PathBuf>,
    /// Linux: also stat every entry so size and mtime are indexed (slower).
    #[arg(long, global = true)]
    with_stat: bool,
    /// Seconds between sweeps for backends that watch by polling.
    #[arg(long, global = true, default_value_t = 30)]
    poll_interval: u64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Scan a directory tree, save the index, and report statistics.
    Index {
        path: PathBuf,
        /// Do not recurse below this depth.
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Search file names. Loads the saved index for `--root` when it is still
    /// fresh, otherwise rescans and saves.
    Search {
        /// Literal substring to look for (case-insensitive by default).
        query: String,
        /// Directory tree to search.
        #[arg(long)]
        root: PathBuf,
        /// Ignore any saved index and rescan.
        #[arg(long)]
        force_rescan: bool,
        /// Skip the payload checksum when loading.
        #[arg(long)]
        no_verify: bool,
        /// Maximum rows to print; 0 means all.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, value_enum, default_value_t = SortArg::Name)]
        sort: SortArg,
        #[arg(long)]
        desc: bool,
        #[arg(long, conflicts_with = "dirs_only")]
        files_only: bool,
        #[arg(long)]
        dirs_only: bool,
        #[arg(long)]
        case_sensitive: bool,
        /// Match the whole name rather than a substring.
        #[arg(long)]
        exact: bool,
        /// Minimum size, e.g. `10K`, `4M`, `1G`.
        #[arg(long)]
        min_size: Option<String>,
        #[arg(long)]
        max_size: Option<String>,
        /// Include hidden/system entries.
        #[arg(long)]
        hidden: bool,
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(short = 'o', long, value_enum, default_value_t = OutputFmt::Human)]
        output: OutputFmt,
        /// Print a timing breakdown to stderr.
        #[arg(long)]
        stat: bool,
    },
    /// Keep the index for `path` up to date as the tree changes, printing
    /// each applied batch. Saves periodically and on exit of `--once`.
    Watch {
        path: PathBuf,
        /// Coalesce events for this long before applying a batch.
        #[arg(long, default_value_t = 500)]
        debounce_ms: u64,
        /// Persist at least this often.
        #[arg(long, default_value_t = 30)]
        save_every: u64,
        /// Skip the catch-up sweep before watching.
        #[arg(long)]
        no_sweep: bool,
        /// Apply one batch and exit.
        #[arg(long)]
        once: bool,
        #[arg(long)]
        force_rescan: bool,
    },
    /// Show the saved index for `path` (loading or rescanning as needed).
    Stats {
        path: PathBuf,
        #[arg(long)]
        force_rescan: bool,
        #[arg(long)]
        json: bool,
    },
    /// Differential check: index `path` and compare against a naive walk.
    /// Exits non-zero if anything is missing, extra, or mismatched.
    Verify {
        path: PathBuf,
        #[arg(long)]
        json: bool,
        /// Show at most this many examples per category.
        #[arg(long, default_value_t = 20)]
        examples: usize,
    },
    /// Build a synthetic index of `--records` entries and time queries.
    Bench {
        #[arg(long, default_value_t = 1_000_000)]
        records: u64,
        /// Needle to time; repeatable. Defaults to a representative set.
        #[arg(long = "query")]
        queries: Vec<String>,
        #[arg(long, default_value_t = 100)]
        iterations: usize,
        #[arg(long)]
        json: bool,
    },
    /// List the backends compiled into this binary.
    Backends,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum SortArg {
    Name,
    Size,
    Mtime,
    Path,
}

impl From<SortArg> for SortKey {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Name => SortKey::Name,
            SortArg::Size => SortKey::Size,
            SortArg::Mtime => SortKey::Mtime,
            SortArg::Path => SortKey::Path,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFmt {
    /// Aligned columns: size, modified, path.
    Human,
    /// One JSON object per line.
    Jsonl,
    /// A single JSON array.
    Json,
    /// Tab-separated, no header.
    Tsv,
    /// NUL-separated paths, for `xargs -0`.
    Null,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("everyfiles: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let backend = everyfiles::find_backend(&cli.backend).ok_or_else(|| {
        format!(
            "unknown backend `{}`; try `everyfiles backends`",
            cli.backend
        )
    })?;
    let bopts = BackendOptions {
        with_stat: cli.with_stat,
        poll_interval: std::time::Duration::from_secs(cli.poll_interval.max(1)),
    };

    match cli.cmd {
        Cmd::Backends => {
            for b in everyfiles::registry() {
                let def = if std::ptr::eq(b, everyfiles::default_backend()) {
                    "  (default)"
                } else {
                    ""
                };
                println!("{:<10} {:?}{def}", b.name, b.tier);
            }
            Ok(())
        }
        Cmd::Index {
            path,
            max_depth,
            json,
        } => {
            let path = normalize_root(&path)?;
            let scanner = (backend.make_scanner)(&bopts)?;
            let probe = scanner.probe(&path)?;
            let opts = ScanOptions {
                threads: cli.threads,
                max_depth,
            };
            let (index, stats) = build_index(scanner.as_ref(), &path, host_volume_config(), &opts)?;
            let index_path = index_path_for(cli.index.as_deref(), index.root_path())?;
            if let Some(parent) = index_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let vid = scanner.volume_id(index.root_path())?;
            let saved = everyfiles::store::save(
                &index,
                &index_path,
                &everyfiles::store::SaveMeta {
                    volume_id: &vid,
                    resume_token: &[],
                },
            )?;
            if json {
                let mut v: serde_json::Value =
                    serde_json::from_str(&stats_json(&index, &stats, backend.name, probe.active))?;
                v["index_path"] = index_path.to_string_lossy().into();
                v["index_bytes"] = saved.bytes.into();
                v["save_ms"] = (saved.elapsed.as_secs_f64() * 1000.0).into();
                println!("{v}");
            } else {
                print_stats(&index, &stats, backend.name, probe.active);
                println!(
                    "saved      {} ({}, {:.2?})",
                    index_path.display(),
                    fmt::human_size(saved.bytes),
                    saved.elapsed
                );
            }
            Ok(())
        }
        Cmd::Search {
            query,
            root,
            force_rescan,
            no_verify,
            limit,
            offset,
            sort,
            desc,
            files_only,
            dirs_only,
            case_sensitive,
            exact,
            min_size,
            max_size,
            hidden,
            max_depth,
            output,
            stat,
        } => {
            let root = normalize_root(&root)?;
            let scanner = (backend.make_scanner)(&bopts)?;
            let index_path = index_path_for(cli.index.as_deref(), &root)?;
            let opts = OpenOptions {
                force_rescan: force_rescan || max_depth.is_some(),
                verify_payload: !no_verify,
                scan: ScanOptions {
                    threads: cli.threads,
                    max_depth,
                },
                ..OpenOptions::default()
            };
            let t0 = std::time::Instant::now();
            let (index, outcome) = everyfiles::engine::open(
                scanner.as_ref(),
                &root,
                &index_path,
                host_volume_config(),
                &opts,
            )?;
            let t_scan = t0.elapsed();
            let source = describe_outcome(&outcome);

            let q = Query {
                needle: query,
                case_sensitive,
                exact,
                kind: if files_only {
                    KindFilter::FilesOnly
                } else if dirs_only {
                    KindFilter::DirsOnly
                } else {
                    KindFilter::Any
                },
                min_size: min_size.as_deref().map(fmt::parse_size).transpose()?,
                max_size: max_size.as_deref().map(fmt::parse_size).transpose()?,
                after: None,
                before: None,
                include_hidden: hidden,
            };
            let t1 = std::time::Instant::now();
            let mut m = query::search(&index, &q)?;
            let t_match = t1.elapsed();
            let t2 = std::time::Instant::now();
            let total_hits = m.ids.len();
            if limit > 0 {
                query::truncate_top(&index, &mut m.ids, sort.into(), desc, offset + limit);
            }
            query::sort_ids(&index, &mut m.ids, sort.into(), desc);
            let t_sort = t2.elapsed();
            let page = query::page(&m.ids, offset, limit);

            let t3 = std::time::Instant::now();
            emit(&index, page, output)?;
            let t_emit = t3.elapsed();

            if stat {
                eprintln!(
                    "open {:>8.2?}  ({} records, {source})\nmatch {:>7.2?}  ({} hits)\nsort {:>8.2?}\nemit {:>8.2?}  ({} rows)",
                    t_scan,
                    index.len(),
                    t_match,
                    total_hits,
                    t_sort,
                    t_emit,
                    page.len()
                );
            }
            Ok(())
        }
        Cmd::Watch {
            path,
            debounce_ms,
            save_every,
            no_sweep,
            once,
            force_rescan,
        } => watch(
            backend,
            &bopts,
            &path,
            cli.index.as_deref(),
            cli.threads,
            WatchArgs {
                debounce_ms,
                save_every,
                no_sweep,
                once,
                force_rescan,
            },
        ),
        Cmd::Stats {
            path,
            force_rescan,
            json,
        } => {
            let path = normalize_root(&path)?;
            let scanner = (backend.make_scanner)(&bopts)?;
            let probe = scanner.probe(&path)?;
            let caps = scanner.capabilities();
            let index_path = index_path_for(cli.index.as_deref(), &path)?;
            let opts = OpenOptions {
                force_rescan,
                scan: ScanOptions {
                    threads: cli.threads,
                    max_depth: None,
                },
                ..OpenOptions::default()
            };
            let (index, outcome) = everyfiles::engine::open(
                scanner.as_ref(),
                &path,
                &index_path,
                host_volume_config(),
                &opts,
            )?;
            let (with_size, with_mtime) = coverage(&index);
            let watcher_probe = (backend.make_watcher)(&bopts)?.probe(&path).ok();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "root": index.root_path().to_string_lossy(),
                        "index_path": index_path.to_string_lossy(),
                        "backend": backend.name,
                        "active_path": probe.active,
                        "watcher_path": watcher_probe.map(|w| w.active),
                        "watcher_note": watcher_probe.and_then(|w| w.reason),
                        "source": describe_outcome(&outcome),
                        "records": index.len(),
                        "live": index.live_count(),
                        "tombstones": index.tombstone_count(),
                        "size_coverage": with_size,
                        "mtime_coverage": with_mtime,
                        "heap_bytes": index.approx_bytes(),
                        "mapped_bytes": index.mapped_bytes(),
                        "capabilities": {
                            "provides_size": caps.provides_size,
                            "provides_mtime": caps.provides_mtime,
                            "requires_privilege": caps.requires_privilege,
                        },
                    })
                );
            } else {
                println!("root        {}", index.root_path().display());
                println!("index file  {}", index_path.display());
                println!("backend     {} ({})", backend.name, probe.active);
                if let Some(w) = &watcher_probe {
                    println!(
                        "watcher     {}{}",
                        w.active,
                        w.reason.map(|r| format!(" — {r}")).unwrap_or_default()
                    );
                }
                println!("source      {}", describe_outcome(&outcome));
                println!("records     {}", index.len());
                println!("live        {}", index.live_count());
                println!("tombstones  {}", index.tombstone_count());
                println!("size known  {with_size:.1}%");
                println!("mtime known {with_mtime:.1}%");
                println!(
                    "heap        {}",
                    fmt::human_size(index.approx_bytes() as u64)
                );
                println!(
                    "mapped      {}",
                    fmt::human_size(index.mapped_bytes() as u64)
                );
                if !caps.provides_size || !caps.provides_mtime {
                    println!("note        this backend does not supply size/mtime in bulk");
                }
            }
            Ok(())
        }
        Cmd::Verify {
            path,
            json,
            examples,
        } => {
            let path = normalize_root(&path)?;
            let scanner = (backend.make_scanner)(&bopts)?;
            let opts = ScanOptions {
                threads: cli.threads,
                max_depth: None,
            };
            let (index, stats) = build_index(scanner.as_ref(), &path, host_volume_config(), &opts)?;
            let caps = scanner.capabilities();
            let report = verify(&index, caps.provides_size, caps.provides_mtime, examples)?;
            if json {
                println!("{}", report.to_json(&stats));
            } else {
                report.print(&stats);
            }
            if report.ok() {
                Ok(())
            } else {
                Err("index differs from naive walk".into())
            }
        }
        Cmd::Bench {
            records,
            queries,
            iterations,
            json,
        } => bench(records, &queries, iterations, json, cli.threads),
    }
}

struct WatchArgs {
    debounce_ms: u64,
    save_every: u64,
    no_sweep: bool,
    once: bool,
    force_rescan: bool,
}

fn watch(
    backend: &everyfiles::BackendDef,
    bopts: &BackendOptions,
    path: &std::path::Path,
    explicit_index: Option<&std::path::Path>,
    threads: Option<usize>,
    a: WatchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    use everyfiles::core::arc_swap::ArcSwap;
    use everyfiles::engine::{Updater, WatchOptions, watch_loop};

    let scanner = (backend.make_scanner)(bopts)?;
    let mut watcher = (backend.make_watcher)(bopts)?;
    let root = normalize_root(path)?;
    let index_path = index_path_for(explicit_index, &root)?;
    let opts = OpenOptions {
        force_rescan: a.force_rescan,
        scan: ScanOptions {
            threads,
            max_depth: None,
        },
        ..OpenOptions::default()
    };
    let t0 = std::time::Instant::now();
    let (index, outcome) = everyfiles::engine::open(
        scanner.as_ref(),
        &root,
        &index_path,
        host_volume_config(),
        &opts,
    )?;
    eprintln!(
        "open     {} ({:.2?})",
        describe_outcome(&outcome),
        t0.elapsed()
    );
    let vid = scanner.volume_id(&root)?;
    let token = match &outcome {
        OpenOutcome::Loaded { resume_token, .. } if !resume_token.is_empty() => {
            Some(everyfiles::ResumeToken(resume_token.clone()))
        }
        _ => None,
    };

    let caps = watcher.capabilities();
    let probe = watcher.probe(&root)?;
    eprintln!(
        "watcher  {} ({}){}",
        backend.name,
        probe.active,
        probe.reason.map(|r| format!(", {r}")).unwrap_or_default()
    );

    let mut updater = Updater::new(index, scanner.as_ref());
    let replaying = match watcher.start(&root, token.as_ref()) {
        Ok(()) => caps.resumable && token.is_some(),
        Err(everyfiles::Error::TokenStale) => {
            eprintln!("resume   token stale; sweeping instead");
            watcher.start(&root, None)?;
            false
        }
        Err(e) => return Err(e.into()),
    };
    if replaying {
        eprintln!("resume   replaying events since the last save");
    } else if !a.no_sweep {
        let t = std::time::Instant::now();
        let before = updater.stats;
        updater.sweep();
        let d = updater.stats;
        eprintln!(
            "sweep    +{} -{} ~{} in {:.2?} ({} dirs listed)",
            d.added - before.added,
            d.removed - before.removed,
            d.updated - before.updated,
            t.elapsed(),
            d.dirs_listed - before.dirs_listed
        );
    }
    let published = ArcSwap::from_pointee(updater.snapshot());
    eprintln!("records  {}", updater.index().len());

    let wopts = WatchOptions {
        debounce: std::time::Duration::from_millis(a.debounce_ms),
        save_every: std::time::Duration::from_secs(a.save_every.max(1)),
        once: a.once,
        ..WatchOptions::default()
    };
    watch_loop(
        &mut updater,
        watcher.as_mut(),
        &published,
        Some(&index_path),
        &vid,
        &wopts,
        |r| {
            println!(
                "batch    {} events  +{} -{} ~{}  records {}{}{}{}",
                r.events,
                r.added,
                r.removed,
                r.updated,
                r.records,
                if r.overflow {
                    "  [overflow: swept]"
                } else {
                    ""
                },
                if r.compacted { "  [compacted]" } else { "" },
                if r.saved { "  [saved]" } else { "" },
            );
        },
    )?;
    watcher.stop()?;
    Ok(())
}

/// Canonical root: the kernel reports change events by real path, so a root
/// given through a symlink (macOS `/var` → `/private/var`) must be resolved or
/// every event would fall outside the index. On Windows the verbatim `\\?\`
/// prefix that canonicalization adds is stripped for readability.
fn normalize_root(p: &std::path::Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p).or_else(|_| std::path::absolute(p))?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC")
        {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(c)
}

fn index_path_for(
    explicit: Option<&std::path::Path>,
    root: &std::path::Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    everyfiles::default_index_path(root)
        .ok_or_else(|| "no cache directory available; pass --index <path>".into())
}

fn describe_outcome(o: &OpenOutcome) -> String {
    match o {
        OpenOutcome::Loaded {
            elapsed, file_len, ..
        } => format!("loaded {} in {elapsed:.2?}", fmt::human_size(*file_len)),
        OpenOutcome::Rescanned {
            reason,
            stats,
            saved_bytes,
            save_elapsed,
        } => format!(
            "rescanned ({reason}) in {:.2?}, saved {} in {save_elapsed:.2?}",
            stats.elapsed,
            fmt::human_size(*saved_bytes)
        ),
    }
}

/// Percent of live records with a known size / mtime.
fn coverage(index: &Index) -> (f64, f64) {
    let mut n = 0u64;
    let mut sz = 0u64;
    let mut mt = 0u64;
    for id in 1..index.len() as Id {
        let f = index.flags(id);
        if f & everyfiles::flags::TOMBSTONE != 0 {
            continue;
        }
        n += 1;
        if f & everyfiles::flags::HAS_SIZE != 0 {
            sz += 1;
        }
        if f & everyfiles::flags::HAS_MTIME != 0 {
            mt += 1;
        }
    }
    if n == 0 {
        return (100.0, 100.0);
    }
    (sz as f64 * 100.0 / n as f64, mt as f64 * 100.0 / n as f64)
}

#[derive(Default)]
struct VerifyReport {
    indexed: usize,
    walked: usize,
    missing: Vec<PathBuf>,
    extra: Vec<PathBuf>,
    mismatched: Vec<(PathBuf, String)>,
    missing_n: usize,
    extra_n: usize,
    mismatched_n: usize,
    /// Entries neither side could read (permissions, vanished).
    skipped: usize,
    /// Directories whose mtime differs. Informational: on macOS a firmlink
    /// stub (e.g. `/usr/local`) reports the sealed system volume's mtime via
    /// `getattrlistbulk` while `lstat` follows it to the data volume. The
    /// engine's sweep corrects the stored value on its first pass.
    dir_mtime_differs: Vec<PathBuf>,
    dir_mtime_differs_n: usize,
}

impl VerifyReport {
    /// Directory-mtime-only differences do not fail the check.
    fn ok(&self) -> bool {
        self.missing_n == 0 && self.extra_n == 0 && self.mismatched_n == 0
    }

    fn print(&self, stats: &ScanStats) {
        println!("indexed    {}  ({:.2?})", self.indexed, stats.elapsed);
        println!("walked     {}", self.walked);
        println!("skipped    {}", self.skipped);
        println!("missing    {}", self.missing_n);
        for p in &self.missing {
            println!("  - {}", p.display());
        }
        println!("extra      {}", self.extra_n);
        for p in &self.extra {
            println!("  + {}", p.display());
        }
        println!("mismatched {}", self.mismatched_n);
        for (p, why) in &self.mismatched {
            println!("  ~ {}  ({why})", p.display());
        }
        if self.dir_mtime_differs_n > 0 {
            println!(
                "dir mtime  {} differ (informational; firmlink stubs on macOS report the system volume's mtime)",
                self.dir_mtime_differs_n
            );
            for p in &self.dir_mtime_differs {
                println!("  ? {}", p.display());
            }
        }
        println!("{}", if self.ok() { "OK" } else { "DIFFERENT" });
    }

    fn to_json(&self, stats: &ScanStats) -> String {
        serde_json::json!({
            "ok": self.ok(),
            "indexed": self.indexed,
            "walked": self.walked,
            "skipped": self.skipped,
            "scan_errors": stats.errors,
            "missing": self.missing_n,
            "extra": self.extra_n,
            "mismatched": self.mismatched_n,
            "dir_mtime_differs": self.dir_mtime_differs_n,
            "missing_examples": self.missing.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
            "extra_examples": self.extra.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
            "mismatched_examples": self.mismatched.iter().map(|(p, w)| serde_json::json!({"path": p.to_string_lossy(), "why": w})).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

fn verify(
    index: &Index,
    check_size: bool,
    check_mtime: bool,
    examples: usize,
) -> Result<VerifyReport, Box<dyn std::error::Error>> {
    let (walked, skipped) = oracle::naive_scan_with_skips(index.root_path())?;
    let mut rep = VerifyReport {
        walked: walked.len(),
        skipped,
        ..VerifyReport::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    for id in 1..index.len() as Id {
        if index.is_tombstone(id) {
            continue;
        }
        rep.indexed += 1;
        let p = index.resolve_path(id);
        match walked.get(&p) {
            None => {
                rep.extra_n += 1;
                if rep.extra.len() < examples {
                    rep.extra.push(p);
                }
            }
            Some(o) => {
                let mut why = Vec::new();
                if index.is_dir(id) != (o.kind == Kind::Dir) {
                    why.push("kind");
                }
                if check_size && o.kind == Kind::File && index.size(id) != o.size {
                    why.push("size");
                }
                if check_mtime && index.mtime(id) != o.mtime {
                    if o.kind == Kind::Dir {
                        rep.dir_mtime_differs_n += 1;
                        if rep.dir_mtime_differs.len() < examples {
                            rep.dir_mtime_differs.push(p.clone());
                        }
                    } else {
                        why.push("mtime");
                    }
                }
                if !why.is_empty() {
                    rep.mismatched_n += 1;
                    if rep.mismatched.len() < examples {
                        rep.mismatched.push((p.clone(), why.join(",")));
                    }
                }
                seen.insert(p);
            }
        }
    }
    for p in walked.keys() {
        if !seen.contains(p) {
            rep.missing_n += 1;
            if rep.missing.len() < examples {
                rep.missing.push(p.clone());
            }
        }
    }
    Ok(rep)
}

fn bench(
    records: u64,
    queries: &[String],
    iterations: usize,
    json: bool,
    threads: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let g = GeneratedScanner::with_approx_total("/bench", records);
    let opts = ScanOptions {
        threads,
        max_depth: None,
    };
    let (index, stats) = build_index(&g, g.root(), host_volume_config(), &opts)?;
    let defaults = ["", "a", "rs", "img", "report", "final_notes", "zzzqqqxxx"];
    let queries: Vec<&str> = if queries.is_empty() {
        defaults.to_vec()
    } else {
        queries.iter().map(String::as_str).collect()
    };
    let mut rows = Vec::new();
    for needle in &queries {
        let q = Query::new(*needle);
        let mut lat: Vec<f64> = Vec::with_capacity(iterations);
        let mut hits = 0usize;
        for _ in 0..iterations.max(1) {
            let t = std::time::Instant::now();
            let m = query::search(&index, &q)?;
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
            hits = m.ids.len();
        }
        lat.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| lat[((lat.len() - 1) as f64 * p).round() as usize];
        rows.push((needle.to_string(), hits, pct(0.5), pct(0.95), pct(0.99)));
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "records": index.len(),
                "approx_bytes": index.approx_bytes(),
                "build_ms": stats.elapsed.as_secs_f64() * 1000.0,
                "iterations": iterations,
                "queries": rows.iter().map(|(q, h, p50, p95, p99)| serde_json::json!({
                    "needle": q, "hits": h, "p50_ms": p50, "p95_ms": p95, "p99_ms": p99
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        println!(
            "records {}   memory {}   build {:.2?}   iterations {}",
            index.len(),
            fmt::human_size(index.approx_bytes() as u64),
            stats.elapsed,
            iterations
        );
        println!(
            "{:<14} {:>9} {:>9} {:>9} {:>9}",
            "needle", "hits", "p50 ms", "p95 ms", "p99 ms"
        );
        for (q, h, p50, p95, p99) in rows {
            println!(
                "{:<14} {h:>9} {p50:>9.3} {p95:>9.3} {p99:>9.3}",
                format!("{q:?}")
            );
        }
    }
    Ok(())
}

fn emit(index: &Index, ids: &[everyfiles::Id], output: OutputFmt) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf = Vec::with_capacity(256);
    match output {
        OutputFmt::Human => {
            for &id in ids {
                index.resolve_path_into(id, &mut buf);
                let size = if index.is_dir(id) {
                    "<DIR>".to_string()
                } else if index.flags(id) & everyfiles::flags::HAS_SIZE != 0 {
                    fmt::human_size(index.size(id))
                } else {
                    "-".to_string()
                };
                let mtime = if index.flags(id) & everyfiles::flags::HAS_MTIME != 0 {
                    fmt::format_utc(index.mtime(id))
                } else {
                    "-".to_string()
                };
                write!(out, "{size:>10}  {mtime:<16}  ")?;
                out.write_all(&buf)?;
                out.write_all(b"\n")?;
            }
        }
        OutputFmt::Tsv => {
            for &id in ids {
                index.resolve_path_into(id, &mut buf);
                write!(
                    out,
                    "{}\t{}\t{}\t",
                    index.size(id),
                    index.mtime(id),
                    u8::from(index.is_dir(id))
                )?;
                out.write_all(&buf)?;
                out.write_all(b"\n")?;
            }
        }
        OutputFmt::Null => {
            for &id in ids {
                index.resolve_path_into(id, &mut buf);
                out.write_all(&buf)?;
                out.write_all(b"\0")?;
            }
        }
        OutputFmt::Jsonl | OutputFmt::Json => {
            let rows: Vec<serde_json::Value> = ids.iter().map(|&id| row_json(index, id)).collect();
            if output == OutputFmt::Json {
                serde_json::to_writer(&mut out, &rows)?;
                out.write_all(b"\n")?;
            } else {
                for r in rows {
                    serde_json::to_writer(&mut out, &r)?;
                    out.write_all(b"\n")?;
                }
            }
        }
    }
    out.flush()
}

fn row_json(index: &Index, id: everyfiles::Id) -> serde_json::Value {
    let f = index.flags(id);
    serde_json::json!({
        "path": index.resolve_path(id).to_string_lossy(),
        "name": String::from_utf8_lossy(index.name(id)),
        "is_dir": index.is_dir(id),
        "size": (f & everyfiles::flags::HAS_SIZE != 0).then_some(index.size(id)),
        "mtime_ns": (f & everyfiles::flags::HAS_MTIME != 0).then_some(index.mtime(id)),
    })
}

fn print_stats(index: &Index, stats: &ScanStats, backend: &str, active: &str) {
    println!("root       {}", index.root_path().display());
    println!("backend    {backend} ({active})");
    println!("records    {}", index.len());
    println!("  dirs     {}", stats.dirs);
    println!("  files    {}", stats.files);
    println!("  symlinks {}", stats.symlinks);
    println!("  other    {}", stats.others);
    println!("errors     {}", stats.errors);
    println!(
        "pool bytes {} raw + {} folded",
        fmt::human_size(index.name_pool_bytes() as u64),
        fmt::human_size(index.fold_pool_bytes() as u64)
    );
    println!(
        "memory     {} (approx)",
        fmt::human_size(index.approx_bytes() as u64)
    );
    println!("elapsed    {:.2?}", stats.elapsed);
}

fn stats_json(index: &Index, stats: &ScanStats, backend: &str, active: &str) -> String {
    serde_json::json!({
        "root": index.root_path().to_string_lossy(),
        "backend": backend,
        "active_path": active,
        "records": index.len(),
        "dirs": stats.dirs,
        "files": stats.files,
        "symlinks": stats.symlinks,
        "other": stats.others,
        "errors": stats.errors,
        "name_pool_bytes": index.name_pool_bytes(),
        "fold_pool_bytes": index.fold_pool_bytes(),
        "approx_bytes": index.approx_bytes(),
        "elapsed_ms": stats.elapsed.as_secs_f64() * 1000.0,
    })
    .to_string()
}
