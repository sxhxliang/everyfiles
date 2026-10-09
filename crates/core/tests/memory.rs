//! Turns the memory arithmetic into a test.
//!
//! A counting global allocator measures the bytes an index actually holds and
//! checks them against the computed cost: 33 bytes of metadata per record plus
//! both name pools. This is the test that catches an accidental per-record
//! heap allocation. It is alone in this binary so nothing else allocates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use everyfiles_core::index::{BLOCK, POOL_BLOCK};
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::scan::{ScanOptions, build_index};
use everyfiles_testkit::GeneratedScanner;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size(), Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        // SAFETY: `p` was returned by `alloc` with this layout.
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            LIVE.fetch_add(new, Relaxed);
            LIVE.fetch_sub(l.size(), Relaxed);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn index_memory_matches_the_arithmetic() {
    let g = GeneratedScanner::with_approx_total("/m", 1_000_000);
    let before = LIVE.load(Relaxed);
    let (idx, _) = build_index(
        &g,
        Path::new("/m"),
        VolumeConfig::unix(),
        &ScanOptions::default(),
    )
    .unwrap();
    let used = LIVE.load(Relaxed) - before;

    let n = idx.len();
    let names = idx.name_pool_bytes() + idx.fold_pool_bytes();
    let expected = 33 * n + names;
    // Block granularity: each array may hold one partially filled block, and
    // each pool one partially filled 1 MiB block.
    let slack = BLOCK * 33 + 2 * POOL_BLOCK;

    eprintln!(
        "records {n}  measured {used}  expected {expected}  slack {slack}  reported {}",
        idx.approx_bytes()
    );
    assert!(
        used >= expected,
        "index uses less than the arithmetic says: {used} < {expected}"
    );
    assert!(
        used <= expected + slack,
        "index uses more than arithmetic + block slack: {used} > {expected} + {slack}"
    );
    // The self-report must be honest too.
    let reported = idx.approx_bytes();
    assert!(
        reported.abs_diff(used) <= used / 20,
        "approx_bytes {reported} is off from measured {used} by more than 5%"
    );
}
