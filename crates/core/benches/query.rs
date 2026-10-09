//! Query latency at scale. Builds a ~1M-record generated index once, then
//! times representative needles. Run with `cargo bench -p everyfiles-core`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::query::{KindFilter, Query, SortKey, search, sort_ids, truncate_top};
use everyfiles_core::scan::{ScanOptions, build_index};
use everyfiles_testkit::GeneratedScanner;

fn bench_query(c: &mut Criterion) {
    let total: u64 = std::env::var("EVERYFILES_BENCH_RECORDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let g = GeneratedScanner::with_approx_total("/bench", total);
    let (idx, stats) = build_index(&g, g.root(), VolumeConfig::unix(), &ScanOptions::default())
        .expect("generated scan");
    eprintln!(
        "index: {} records, {} approx bytes, built in {:.2?}",
        idx.len(),
        idx.approx_bytes(),
        stats.elapsed
    );

    let mut group = c.benchmark_group("search");
    group.sample_size(30);
    for needle in ["a", "rs", "img", "report", "final_notes", "zzzqqqxxx"] {
        group.bench_with_input(BenchmarkId::new("substring", needle), &needle, |b, n| {
            b.iter(|| search(&idx, &Query::new(*n)).unwrap().ids.len())
        });
    }
    group.bench_function("empty-needle-files-only", |b| {
        b.iter(|| {
            search(
                &idx,
                &Query {
                    kind: KindFilter::FilesOnly,
                    ..Query::default()
                },
            )
            .unwrap()
            .ids
            .len()
        })
    });
    group.finish();

    let hits = search(&idx, &Query::new("a")).unwrap().ids;
    let mut group = c.benchmark_group("sort");
    group.sample_size(20);
    group.bench_function("top100-by-size", |b| {
        b.iter(|| {
            let mut ids = hits.clone();
            truncate_top(&idx, &mut ids, SortKey::Size, true, 100);
            sort_ids(&idx, &mut ids, SortKey::Size, true);
            ids.len()
        })
    });
    group.bench_function("full-sort-by-name", |b| {
        b.iter(|| {
            let mut ids = hits.clone();
            sort_ids(&idx, &mut ids, SortKey::Name, false);
            ids.len()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
