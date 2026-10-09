//! `everyfiles bench`: a reproducible suite covering index build, save/load,
//! substring, glob, combined-filter and sort queries on a synthetic index,
//! plus optional real-directory scans with every compiled-in backend.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use everyfiles::query::{self, KindFilter, Query, SortKey};
use everyfiles::scan::VolumeId;
use everyfiles::{BackendOptions, Index, ScanOptions, build_index, host_volume_config};
use everyfiles_testkit::GeneratedScanner;

use crate::fmt;

pub struct BenchArgs {
    pub records: u64,
    pub queries: Vec<String>,
    pub iterations: usize,
    pub scan_iterations: usize,
    pub path: Option<PathBuf>,
    pub json: bool,
    pub threads: Option<usize>,
    pub bopts: BackendOptions,
}

struct Case {
    category: &'static str,
    label: String,
    query: Query,
    /// `(key, desc, keep)`: time sorting the hits instead of matching.
    sort: Option<(SortKey, bool, usize)>,
}

fn q(needle: &str) -> Query {
    Query::new(needle)
}

/// One year before the newest generated mtime, so the time filter is selective.
const GEN_BASE_NS: i64 = 1_600_000_000_000_000_000;

fn cases(custom: &[String]) -> Vec<Case> {
    let mut v = Vec::new();
    if custom.is_empty() {
        for n in ["a", "rs", "report", "final_notes", "zzzqqqxxx"] {
            v.push(Case {
                category: "substring",
                label: format!("{n:?}"),
                query: q(n),
                sort: None,
            });
        }
        v.push(Case {
            category: "substring",
            label: "\"\" (match all)".into(),
            query: q(""),
            sort: None,
        });
    } else {
        for n in custom {
            v.push(Case {
                category: "substring",
                label: format!("{n:?}"),
                query: q(n),
                sort: None,
            });
        }
    }
    for n in ["*.rs", "report*", "*_*.pdf", "img????.*"] {
        v.push(Case {
            category: "glob (prefiltered)",
            label: format!("{n:?}"),
            query: q(n),
            sort: None,
        });
    }
    v.push(Case {
        category: "glob (slow path)",
        label: "\"*a*\" (no literal run)".into(),
        query: q("*a*"),
        sort: None,
    });
    v.push(Case {
        category: "filters",
        label: "\"\" files-only".into(),
        query: Query {
            kind: KindFilter::FilesOnly,
            ..q("")
        },
        sort: None,
    });
    v.push(Case {
        category: "filters",
        label: "\"\" files-only, size 1M..10M".into(),
        query: Query {
            kind: KindFilter::FilesOnly,
            min_size: Some(1 << 20),
            max_size: Some(10 << 20),
            ..q("")
        },
        sort: None,
    });
    v.push(Case {
        category: "filters",
        label: "\"report\" + mtime in newest third".into(),
        query: Query {
            after: Some(GEN_BASE_NS + 33_000_000_000),
            ..q("report")
        },
        sort: None,
    });
    v.push(Case {
        category: "filters",
        label: "\"*.jpg\" files-only, size > 25M".into(),
        query: Query {
            kind: KindFilter::FilesOnly,
            min_size: Some(25 << 20),
            ..q("*.jpg")
        },
        sort: None,
    });
    v.push(Case {
        category: "sort",
        label: "\"a\" hits: top 100 by size".into(),
        query: q("a"),
        sort: Some((SortKey::Size, true, 100)),
    });
    v.push(Case {
        category: "sort",
        label: "\"a\" hits: top 100 by mtime".into(),
        query: q("a"),
        sort: Some((SortKey::Mtime, true, 100)),
    });
    v.push(Case {
        category: "sort",
        label: "\"a\" hits: full sort by name".into(),
        query: q("a"),
        sort: Some((SortKey::Name, false, 0)),
    });
    v.push(Case {
        category: "sort",
        label: "\"rs\" hits: full sort by path".into(),
        query: q("rs"),
        sort: Some((SortKey::Path, false, 0)),
    });
    v
}

fn pct(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    sorted_ms[((sorted_ms.len() - 1) as f64 * p).round() as usize]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct QueryRow {
    category: &'static str,
    label: String,
    hits: usize,
    p50: f64,
    p95: f64,
    p99: f64,
}

struct ScanRow {
    backend: String,
    active: String,
    records: usize,
    errors: u64,
    scan_min: f64,
    scan_median: f64,
    save: f64,
    load: f64,
}

pub fn run(a: BenchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    let iterations = a.iterations.max(1);

    // ---- synthetic index ------------------------------------------------
    let g = GeneratedScanner::with_approx_total("/bench", a.records);
    let opts = ScanOptions {
        threads: a.threads,
        max_depth: None,
    };
    let mut build_times = Vec::new();
    let mut index: Option<Index> = None;
    for _ in 0..a.scan_iterations.max(1) {
        let t = Instant::now();
        let (idx, _) = build_index(&g, g.root(), host_volume_config(), &opts)?;
        build_times.push(ms(t.elapsed()));
        index = Some(idx);
    }
    let index = index.expect("at least one build");
    build_times.sort_by(f64::total_cmp);

    let tmp = std::env::temp_dir().join(format!("everyfiles-bench-{}.idx", std::process::id()));
    let t = Instant::now();
    let saved = everyfiles::store::save(
        &index,
        &tmp,
        &everyfiles::store::SaveMeta {
            volume_id: &VolumeId::none(),
            resume_token: &[],
        },
    )?;
    let save_ms = ms(t.elapsed());
    let mut load_times = Vec::with_capacity(iterations);
    let mut verify_ms = 0.0;
    for i in 0..iterations.min(50) {
        let t = Instant::now();
        let l = everyfiles::store::load(&tmp, host_volume_config())?;
        load_times.push(ms(t.elapsed()));
        if i == 0 {
            let t = Instant::now();
            let ok = l.verify_payload();
            verify_ms = ms(t.elapsed());
            assert!(ok, "payload checksum");
        }
    }
    load_times.sort_by(f64::total_cmp);
    let _ = std::fs::remove_file(&tmp);

    // ---- queries ---------------------------------------------------------
    let mut rows = Vec::new();
    for c in cases(&a.queries) {
        let mut lat = Vec::with_capacity(iterations);
        let hits;
        match c.sort {
            None => {
                let mut h = 0;
                for _ in 0..iterations {
                    let t = Instant::now();
                    let m = query::search(&index, &c.query)?;
                    lat.push(ms(t.elapsed()));
                    h = m.ids.len();
                }
                hits = h;
            }
            Some((key, desc, keep)) => {
                let base = query::search(&index, &c.query)?.ids;
                hits = base.len();
                for _ in 0..iterations {
                    let mut ids = base.clone();
                    let t = Instant::now();
                    if keep > 0 {
                        query::truncate_top(&index, &mut ids, key, desc, keep);
                    }
                    query::sort_ids(&index, &mut ids, key, desc);
                    lat.push(ms(t.elapsed()));
                }
            }
        }
        lat.sort_by(f64::total_cmp);
        rows.push(QueryRow {
            category: c.category,
            label: c.label,
            hits,
            p50: pct(&lat, 0.5),
            p95: pct(&lat, 0.95),
            p99: pct(&lat, 0.99),
        });
    }

    // ---- real directory, every backend ----------------------------------
    // Free the synthetic index first so real scans are not measured next to
    // a 65 MiB neighbour, and alternate backend order each round so neither
    // consistently benefits from the cache the other just warmed.
    let synthetic_records = index.len();
    let synthetic_bytes = index.approx_bytes();
    drop(index);
    let mut scans: Vec<ScanRow> = Vec::new();
    if let Some(p) = &a.path {
        let root = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        let backends: Vec<_> = everyfiles::registry().iter().collect();
        let mut scanners = Vec::new();
        for b in &backends {
            scanners.push((b.make_scanner)(&a.bopts)?);
        }
        let mut all_times: Vec<Vec<f64>> = vec![Vec::new(); backends.len()];
        let mut last: Vec<Option<(Index, u64)>> = (0..backends.len()).map(|_| None).collect();
        for round in 0..a.scan_iterations.max(1) {
            let order: Vec<usize> = if round % 2 == 0 {
                (0..backends.len()).collect()
            } else {
                (0..backends.len()).rev().collect()
            };
            for i in order {
                let t = Instant::now();
                let (idx, st) =
                    build_index(scanners[i].as_ref(), &root, host_volume_config(), &opts)?;
                all_times[i].push(ms(t.elapsed()));
                last[i] = Some((idx, st.errors));
            }
        }
        for (i, b) in backends.iter().enumerate() {
            let scanner = &scanners[i];
            let active = scanner
                .probe(&root)
                .map(|r| r.active.to_string())
                .unwrap_or_else(|e| e.to_string());
            let mut times = std::mem::take(&mut all_times[i]);
            times.sort_by(f64::total_cmp);
            let (idx, errors) = last[i].take().expect("at least one scan");
            let tmp = std::env::temp_dir().join(format!(
                "everyfiles-bench-{}-{}.idx",
                std::process::id(),
                b.name
            ));
            let t = Instant::now();
            everyfiles::store::save(
                &idx,
                &tmp,
                &everyfiles::store::SaveMeta {
                    volume_id: &VolumeId::none(),
                    resume_token: &[],
                },
            )?;
            let save = ms(t.elapsed());
            let t = Instant::now();
            let _ = everyfiles::store::load(&tmp, host_volume_config())?;
            let load = ms(t.elapsed());
            let _ = std::fs::remove_file(&tmp);
            scans.push(ScanRow {
                backend: b.name.to_string(),
                active,
                records: idx.len(),
                errors,
                scan_min: times[0],
                scan_median: times[times.len() / 2],
                save,
                load,
            });
        }
    }

    // ---- output ------------------------------------------------------------
    if a.json {
        let v = serde_json::json!({
            "system": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "cpus": cpus,
                "threads": a.threads,
            },
            "synthetic": {
                "records": synthetic_records,
                "approx_bytes": synthetic_bytes,
                "build_min_ms": build_times[0],
                "build_median_ms": build_times[build_times.len() / 2],
                "file_bytes": saved.bytes,
                "save_ms": save_ms,
                "load_p50_ms": pct(&load_times, 0.5),
                "load_p95_ms": pct(&load_times, 0.95),
                "verify_payload_ms": verify_ms,
            },
            "iterations": iterations,
            "queries": rows.iter().map(|r| serde_json::json!({
                "category": r.category, "label": r.label, "hits": r.hits,
                "p50_ms": r.p50, "p95_ms": r.p95, "p99_ms": r.p99,
            })).collect::<Vec<_>>(),
            "real": a.path.as_ref().map(|p| serde_json::json!({
                "path": p.to_string_lossy(),
                "scan_iterations": a.scan_iterations.max(1),
                "backends": scans.iter().map(|s| serde_json::json!({
                    "backend": s.backend, "active": s.active, "records": s.records, "errors": s.errors,
                    "scan_min_ms": s.scan_min, "scan_median_ms": s.scan_median,
                    "save_ms": s.save, "load_ms": s.load,
                })).collect::<Vec<_>>(),
            })),
        });
        println!("{v}");
        return Ok(());
    }

    println!(
        "system     {} {}  {cpus} cpus{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.threads
            .map(|t| format!("  (--threads {t})"))
            .unwrap_or_default()
    );
    println!();
    println!(
        "## synthetic index ({} records, {} in memory)",
        synthetic_records,
        fmt::human_size(synthetic_bytes as u64)
    );
    println!("{:<34} {:>10} {:>10}", "operation", "min ms", "median ms");
    println!(
        "{:<34} {:>10.2} {:>10.2}",
        "build (in-memory scanner)",
        build_times[0],
        build_times[build_times.len() / 2]
    );
    println!(
        "{:<34} {:>10.2} {:>10}",
        format!("save ({})", fmt::human_size(saved.bytes)),
        save_ms,
        ""
    );
    println!(
        "{:<34} {:>10.3} {:>10.3}",
        "load (mmap, zero-copy)",
        pct(&load_times, 0.5),
        pct(&load_times, 0.95)
    );
    println!(
        "{:<34} {:>10.2} {:>10}",
        "verify payload checksum", verify_ms, ""
    );
    println!();
    println!("## queries ({iterations} iterations each)");
    println!(
        "{:<20} {:<36} {:>9} {:>9} {:>9} {:>9}",
        "category", "query", "hits", "p50 ms", "p95 ms", "p99 ms"
    );
    for r in &rows {
        println!(
            "{:<20} {:<36} {:>9} {:>9.3} {:>9.3} {:>9.3}",
            r.category, r.label, r.hits, r.p50, r.p95, r.p99
        );
    }
    if let Some(p) = &a.path {
        println!();
        println!(
            "## real tree: {} ({} scans each, min / median)",
            p.display(),
            a.scan_iterations.max(1)
        );
        println!(
            "{:<10} {:<24} {:>9} {:>7} {:>10} {:>10} {:>9} {:>9}",
            "backend",
            "active path",
            "records",
            "errors",
            "scan min",
            "scan med",
            "save ms",
            "load ms"
        );
        for s in &scans {
            println!(
                "{:<10} {:<24} {:>9} {:>7} {:>10.2} {:>10.2} {:>9.2} {:>9.3}",
                s.backend, s.active, s.records, s.errors, s.scan_min, s.scan_median, s.save, s.load
            );
        }
    }
    let _ = Path::new("");
    Ok(())
}
