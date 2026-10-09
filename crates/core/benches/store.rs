//! Persistence at scale: how long a 1M-record index takes to save, to load
//! (zero-copy mmap plus bounds checks), and to checksum.

use criterion::{Criterion, criterion_group, criterion_main};
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::scan::{ScanOptions, VolumeId, build_index};
use everyfiles_core::store::{SaveMeta, load, save};
use everyfiles_testkit::GeneratedScanner;

fn bench_store(c: &mut Criterion) {
    let total: u64 = std::env::var("EVERYFILES_BENCH_RECORDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let g = GeneratedScanner::with_approx_total("/bench", total);
    let (idx, _) = build_index(&g, g.root(), VolumeConfig::unix(), &ScanOptions::default())
        .expect("generated scan");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.idx");
    let meta = SaveMeta {
        volume_id: &VolumeId::none(),
        resume_token: &[],
    };
    let st = save(&idx, &path, &meta).unwrap();
    eprintln!(
        "index: {} records, file {} bytes, first save {:.2?}",
        idx.len(),
        st.bytes,
        st.elapsed
    );

    let mut group = c.benchmark_group("store");
    group.sample_size(20);
    group.bench_function("load_1m", |b| {
        b.iter(|| {
            let l = load(&path, VolumeConfig::unix()).unwrap();
            (l.index.len(), l.index.mapped_bytes())
        })
    });
    group.bench_function("load_1m_and_verify_payload", |b| {
        b.iter(|| {
            let l = load(&path, VolumeConfig::unix()).unwrap();
            l.verify_payload()
        })
    });
    group.sample_size(10);
    group.bench_function("save_1m", |b| {
        b.iter(|| save(&idx, &path, &meta).unwrap().bytes)
    });
    group.finish();
}

criterion_group!(benches, bench_store);
criterion_main!(benches);
