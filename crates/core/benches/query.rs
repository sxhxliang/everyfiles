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

    #[cfg(feature = "pinyin")]
    bench_pinyin(c, &idx);
}

/// Pinyin matching (feature `pinyin`): an index of Chinese names, where
/// every record goes through the DP, and the pure-ASCII index above, where
/// none does and the cost is the pool scan plus the high-byte pass.
#[cfg(feature = "pinyin")]
fn bench_pinyin(c: &mut Criterion, ascii: &everyfiles_core::Index) {
    use everyfiles_testkit::SyntheticScanner;

    const WORDS: &[&str] = &[
        "拼音", "搜索", "报告", "最终", "设计", "文档", "会议", "纪要", "项目", "计划", "预算",
        "合同", "发票", "照片", "视频", "音乐", "备份", "归档", "重庆", "银行", "女装", "绿色",
        "数据", "导出", "总结", "方案", "测试", "需求", "用户", "手册", "季度", "年度",
    ];
    const EXTS: &[&str] = &[
        "docx", "xlsx", "pdf", "txt", "md", "jpg", "png", "mp4", "zip",
    ];
    let total = (ascii.len() / 5).clamp(10_000, 200_000);
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let paths: Vec<String> = (0..total)
        .map(|i| {
            let w1 = WORDS[(next() % WORDS.len() as u64) as usize];
            let w2 = WORDS[(next() % WORDS.len() as u64) as usize];
            let ext = EXTS[(next() % EXTS.len() as u64) as usize];
            let name = match next() % 4 {
                0 => format!("{w1}{w2}{i}.{ext}"),
                1 => format!("{w1}-{w2}-{i}.{ext}"),
                2 => format!("{w1}{w2}Everything{i}.{ext}"),
                _ => format!("{w1} {w2} 副本({i}).{ext}"),
            };
            format!("d{:03}/{name}", i % 400)
        })
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let scanner = SyntheticScanner::from_paths("/cjk", &refs);
    let (cjk, _) = build_index(
        &scanner,
        scanner.root(),
        VolumeConfig::unix(),
        &ScanOptions::default(),
    )
    .expect("cjk scan");
    eprintln!("cjk index: {} records", cjk.len());

    let pinyin = |n: &str| Query {
        needle: n.into(),
        pinyin: true,
        ..Query::default()
    };
    let mut group = c.benchmark_group("pinyin");
    group.sample_size(30);
    for needle in [
        "py",
        "pysousuo",
        "sousuo",
        "bgzz",
        "chongqing",
        "eve",
        "zzzqqq",
    ] {
        group.bench_with_input(BenchmarkId::new("cjk", needle), &needle, |b, n| {
            b.iter(|| search(&cjk, &pinyin(n)).unwrap().ids.len())
        });
    }
    group.bench_function("cjk-exact", |b| {
        let q = Query {
            exact: true,
            ..pinyin("pysousuoeverything1.docx")
        };
        b.iter(|| search(&cjk, &q).unwrap().ids.len())
    });
    for needle in ["report", "zzzqqqxxx"] {
        group.bench_with_input(BenchmarkId::new("ascii", needle), &needle, |b, n| {
            b.iter(|| search(ascii, &pinyin(n)).unwrap().ids.len())
        });
    }
    group.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
