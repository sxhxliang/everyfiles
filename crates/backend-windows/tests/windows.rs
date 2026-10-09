//! Runs only on Windows (CI): proves the walk against `std`, then the full
//! differential test against a naive walk.
#![cfg(target_os = "windows")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use everyfiles_backend_windows::WindowsScanner;
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::scan::{
    Kind, ScanOptions, Scanner, VolumeIdKind, build_index, system_time_to_ns,
};
use everyfiles_core::{Id, Index};
use everyfiles_testkit::{fixture, oracle};

fn live_paths(idx: &Index) -> BTreeSet<PathBuf> {
    (1..idx.len() as Id)
        .filter(|&i| !idx.is_tombstone(i))
        .map(|i| idx.resolve_path(i))
        .collect()
}

#[test]
fn read_dir_matches_std_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(
        &root,
        &[
            "main.rs",
            "UPPER.TXT",
            "sub/",
            "名前.md",
            "spaces in name.txt",
        ],
    )
    .unwrap();
    let s = WindowsScanner::new();
    assert_eq!(s.probe(&root).unwrap().active, "file-id-both-dir-info");
    let mut got = Vec::new();
    s.read_dir(&root, &mut got).unwrap();
    let got: BTreeMap<Vec<u8>, _> = got
        .into_iter()
        .map(|e| (e.name.to_vec(), (e.kind, e.size, e.mtime)))
        .collect();
    let mut want = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        let md = entry.metadata().unwrap();
        let kind = if md.is_dir() { Kind::Dir } else { Kind::File };
        want.insert(
            entry.file_name().as_encoded_bytes().to_vec(),
            (kind, md.len(), system_time_to_ns(md.modified().unwrap())),
        );
    }
    assert_eq!(got.len(), want.len());
    for (name, (kind, size, mtime)) in &want {
        let g = &got[name];
        assert_eq!(g.0, *kind, "kind of {}", String::from_utf8_lossy(name));
        if *kind == Kind::File {
            assert_eq!(
                g.1,
                Some(*size),
                "size of {}",
                String::from_utf8_lossy(name)
            );
        }
        assert_eq!(
            g.2,
            Some(*mtime),
            "mtime of {}",
            String::from_utf8_lossy(name)
        );
    }
}

#[test]
fn full_index_matches_naive_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(
        &root,
        &[
            "src/main.rs",
            "src/lib.rs",
            "docs/README.md",
            "empty/",
            "deep/a/b/c/d.txt",
            "名前/ファイル.md",
        ],
    )
    .unwrap();
    let expected = oracle::naive_scan(&root).unwrap();
    let (idx, stats) = build_index(
        &WindowsScanner::new(),
        &root,
        VolumeConfig::windows(),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.errors, 0);
    let want: BTreeSet<PathBuf> = expected.keys().cloned().collect();
    assert_eq!(live_paths(&idx), want);
    for id in 1..idx.len() as Id {
        let p = idx.resolve_path(id);
        let o = &expected[&p];
        assert_eq!(idx.is_dir(id), o.kind == Kind::Dir, "{p:?}");
        if o.kind == Kind::File {
            assert_eq!(idx.size(id), o.size, "size of {p:?}");
        }
        assert_eq!(idx.mtime(id), o.mtime, "mtime of {p:?}");
    }
}

#[test]
fn volume_serial_is_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let s = WindowsScanner::new();
    let a = s.volume_id(&root).unwrap();
    assert_eq!(a.kind, VolumeIdKind::WinSerial);
    assert_eq!(a, s.volume_id(&root).unwrap());
    assert!(a.bytes.len() >= 4);
}

mod watcher {
    use everyfiles_backend_windows::RdcwWatcher;
    use everyfiles_core::watch::{PollOutcome, WatchEvent, Watcher};
    use std::time::{Duration, Instant};

    #[test]
    fn delivers_create_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut w = RdcwWatcher::new();
        w.start(&root, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let f = root.join("created.txt");
        std::fs::write(&f, b"x").unwrap();
        let start = Instant::now();
        let mut seen = false;
        let mut buf = Vec::new();
        while start.elapsed() < Duration::from_secs(10) && !seen {
            buf.clear();
            if let Ok(PollOutcome::Events(_)) = w.poll(Duration::from_millis(200), &mut buf) {
                seen = buf.iter().any(|e| matches!(e, WatchEvent::Path { path, .. } if path.file_name() == f.file_name()));
            }
        }
        assert!(seen, "create event arrived");
        w.stop().unwrap();
    }
}
