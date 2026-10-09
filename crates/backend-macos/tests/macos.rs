//! Runs only on macOS: proves the `getattrlistbulk` parser against the real
//! filesystem, then the full differential test against a naive walk.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::path::PathBuf;

use everyfiles_backend_macos::MacScanner;
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::scan::{
    Kind, ScanOptions, Scanner, VolumeIdKind, build_index, system_time_to_ns,
};
use everyfiles_core::{Id, Index};
use everyfiles_testkit::{fixture, oracle};

fn live_paths(idx: &Index) -> std::collections::BTreeSet<PathBuf> {
    (1..idx.len() as Id)
        .filter(|&i| !idx.is_tombstone(i))
        .map(|i| idx.resolve_path(i))
        .collect()
}

/// The layout-proof test: parsed names, kinds, sizes, and mtimes must equal
/// what `std` reports for the same directory.
#[test]
fn read_dir_matches_std_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(
        &root,
        &[
            "main.rs",
            "lib.rs",
            "UPPER.TXT",
            "sub/",
            "empty/",
            "名前.md",
            "spaces in name.txt",
            ".hidden",
        ],
    )
    .unwrap();
    std::os::unix::fs::symlink("main.rs", root.join("link.rs")).unwrap();

    let s = MacScanner::new();
    assert_eq!(s.probe(&root).unwrap().active, "getattrlistbulk");

    let mut got = Vec::new();
    s.read_dir(&root, &mut got).unwrap();
    let got: BTreeMap<Vec<u8>, _> = got
        .into_iter()
        .map(|e| (e.name.to_vec(), (e.kind, e.size, e.mtime, e.native_id)))
        .collect();

    let mut want = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        let md = entry.metadata().unwrap();
        let ft = md.file_type();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            Kind::Symlink
        };
        want.insert(
            entry.file_name().as_encoded_bytes().to_vec(),
            (
                kind,
                md.len(),
                md.modified().map(system_time_to_ns).unwrap(),
                {
                    use std::os::unix::fs::MetadataExt;
                    md.ino()
                },
            ),
        );
    }
    assert_eq!(got.len(), want.len(), "entry count");
    for (name, (kind, size, mtime, ino)) in &want {
        let g = got
            .get(name)
            .unwrap_or_else(|| panic!("missing {}", String::from_utf8_lossy(name)));
        assert_eq!(g.0, *kind, "kind of {name:?}");
        if *kind == Kind::Dir {
            assert_eq!(g.1, None, "dirs report no data length");
        } else {
            assert_eq!(g.1, Some(*size), "size of {name:?}");
        }
        assert_eq!(g.2, Some(*mtime), "mtime of {name:?}");
        assert_eq!(g.3, Some(*ino), "inode of {name:?}");
    }
}

/// Forces more than one `getattrlistbulk` call for a single directory.
#[test]
fn large_directory_spans_multiple_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let n = 30_000;
    for i in 0..n {
        std::fs::write(root.join(format!("file-{i:05}.dat")), b"x").unwrap();
    }
    let mut got = Vec::new();
    MacScanner::new().read_dir(&root, &mut got).unwrap();
    assert_eq!(got.len(), n);
    let mut names: Vec<_> = got.iter().map(|e| e.name.to_vec()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), n, "no duplicates across call boundaries");
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
            "UPPER.TXT",
            "dot/.hidden",
            "名前/ファイル.md",
            "spaces in name.txt",
        ],
    )
    .unwrap();
    std::os::unix::fs::symlink("src/main.rs", root.join("link.rs")).unwrap();

    let expected = oracle::naive_scan(&root).unwrap();
    let (idx, stats) = build_index(
        &MacScanner::new(),
        &root,
        VolumeConfig::unix(),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.errors, 0);
    let want: std::collections::BTreeSet<PathBuf> = expected.keys().cloned().collect();
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
fn volume_uuid_is_stable_and_shared_within_a_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let s = MacScanner::new();
    let a = s.volume_id(&root).unwrap();
    let b = s.volume_id(&sub).unwrap();
    assert_eq!(a.kind, VolumeIdKind::MacUuid);
    assert_eq!(a.bytes.len(), 16);
    assert_eq!(a, b, "same volume, same identity");
    assert_ne!(a.bytes, [0u8; 16]);
}

mod watcher {
    use everyfiles_backend_macos::{FsEventsWatcher, MacScanner};
    use everyfiles_core::arc_swap::ArcSwap;
    use everyfiles_core::engine::{Updater, WatchOptions, watch_loop};
    use everyfiles_core::platform::VolumeConfig;
    use everyfiles_core::query::{Query, search};
    use everyfiles_core::scan::{ScanOptions, VolumeId, build_index};
    use everyfiles_core::watch::{PollOutcome, ResumeToken, WatchEvent, Watcher};
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn wait_for_path(w: &mut dyn Watcher, want: &Path, deadline: Duration) -> bool {
        let start = Instant::now();
        let mut buf = Vec::new();
        while start.elapsed() < deadline {
            buf.clear();
            if let Ok(PollOutcome::Events(_)) = w.poll(Duration::from_millis(200), &mut buf)
                && buf
                    .iter()
                    .any(|e| matches!(e, WatchEvent::Path { path, .. } if path == want))
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn delivers_file_events_with_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut w = FsEventsWatcher::new().with_latency(0.05);
        assert!(w.capabilities().resumable);
        assert_eq!(w.probe(&root).unwrap().active, "fsevents+resume");
        w.start(&root, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let f = root.join("created.txt");
        std::fs::write(&f, b"hello").unwrap();
        assert!(
            wait_for_path(&mut w, &f, Duration::from_secs(10)),
            "create event"
        );

        std::fs::remove_file(&f).unwrap();
        assert!(
            wait_for_path(&mut w, &f, Duration::from_secs(10)),
            "remove event"
        );

        let tok = w.resume_token();
        assert_eq!(tok.0.len(), 24);
        w.stop().unwrap();
    }

    #[test]
    fn foreign_token_is_stale_and_own_token_replays() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut w = FsEventsWatcher::new().with_latency(0.05);
        let bogus = ResumeToken(vec![0xEE; 24]);
        assert!(matches!(
            w.start(&root, Some(&bogus)),
            Err(everyfiles_core::Error::TokenStale)
        ));

        // Take a token, make a change while NOT watching, then resume from it:
        // the change must be replayed.
        w.start(&root, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let tok = w.resume_token();
        w.stop().unwrap();
        let f = root.join("offline.txt");
        std::fs::write(&f, b"x").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let mut w2 = FsEventsWatcher::new().with_latency(0.05);
        w2.start(&root, Some(&tok)).unwrap();
        assert!(
            wait_for_path(&mut w2, &f, Duration::from_secs(10)),
            "replayed offline event"
        );
        w2.stop().unwrap();
    }

    #[test]
    fn end_to_end_index_converges_through_the_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        let s = MacScanner::new();
        let (idx, _) =
            build_index(&s, &root, VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        let mut u = Updater::new(idx, &s);
        let mut w = FsEventsWatcher::new().with_latency(0.05);
        w.start(&root, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("sub/deep/new.rs"), b"fn main(){}").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();

        let published = ArcSwap::from_pointee(u.snapshot());
        let opts = WatchOptions {
            debounce: Duration::from_millis(300),
            once: true,
            ..WatchOptions::default()
        };
        let mut reports = Vec::new();
        watch_loop(
            &mut u,
            &mut w,
            &published,
            None,
            &VolumeId::none(),
            &opts,
            |r| reports.push(r.clone()),
        )
        .unwrap();
        w.stop().unwrap();
        assert_eq!(reports.len(), 1);
        let snap = published.load();
        let names = |q: &str| -> Vec<String> {
            search(&snap, &Query::new(q))
                .unwrap()
                .ids
                .iter()
                .map(|&i| String::from_utf8_lossy(snap.name(i)).into_owned())
                .collect()
        };
        assert_eq!(names("new.rs"), ["new.rs"]);
        assert!(
            names("a.txt").is_empty(),
            "removed file is gone from the snapshot"
        );
        assert_eq!(names("deep"), ["deep"]);
    }
}
