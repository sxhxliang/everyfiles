//! Runs only on Linux (CI): proves `getdents64` enumeration against `std`,
//! then the full differential test against a naive walk.
#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use everyfiles_backend_linux::LinuxScanner;
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

const TREE: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "docs/README.md",
    "empty/",
    "deep/a/b/c/d.txt",
    "UPPER.TXT",
    "dot/.hidden",
    "名前/ファイル.md",
    "spaces in name.txt",
];

#[test]
fn read_dir_matches_std_names_and_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(&root, &["main.rs", "sub/", "名前.md", ".hidden"]).unwrap();
    std::os::unix::fs::symlink("main.rs", root.join("link.rs")).unwrap();

    let s = LinuxScanner::new();
    assert_eq!(s.probe(&root).unwrap().active, "getdents64");
    let mut got = Vec::new();
    s.read_dir(&root, &mut got).unwrap();
    let got: BTreeMap<Vec<u8>, _> = got
        .into_iter()
        .map(|e| (e.name.to_vec(), (e.kind, e.size, e.native_id)))
        .collect();

    let mut want = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        let ft = entry.file_type().unwrap();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            Kind::Symlink
        };
        use std::os::unix::fs::MetadataExt;
        want.insert(
            entry.file_name().as_encoded_bytes().to_vec(),
            (kind, entry.metadata().unwrap().ino()),
        );
    }
    assert_eq!(got.len(), want.len());
    for (name, (kind, ino)) in &want {
        let g = &got[name];
        assert_eq!(g.0, *kind, "kind of {}", String::from_utf8_lossy(name));
        assert_eq!(g.1, None, "names-only mode reports no size");
        assert_eq!(g.2, Some(*ino));
    }
}

#[test]
fn with_stat_matches_std_sizes_and_mtimes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(&root, &["a.txt", "bb.txt", "sub/"]).unwrap();
    let s = LinuxScanner::new().with_stat(true);
    assert_eq!(s.probe(&root).unwrap().active, "getdents64+statx");
    let mut got = Vec::new();
    s.read_dir(&root, &mut got).unwrap();
    for e in got {
        let p = root.join(std::ffi::OsStr::from_encoded_bytes_unchecked_wrapper(
            &e.name,
        ));
        let md = std::fs::symlink_metadata(&p).unwrap();
        assert_eq!(e.size, Some(md.len()), "{p:?}");
        assert_eq!(
            e.mtime,
            Some(system_time_to_ns(md.modified().unwrap())),
            "{p:?}"
        );
    }
}

/// Tiny shim so the test above reads clearly; names here are valid UTF-8.
trait Wrap {
    fn from_encoded_bytes_unchecked_wrapper(b: &[u8]) -> &std::ffi::OsStr;
}
impl Wrap for std::ffi::OsStr {
    fn from_encoded_bytes_unchecked_wrapper(b: &[u8]) -> &std::ffi::OsStr {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b)
    }
}

#[test]
fn full_index_matches_naive_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fixture::create_tree(&root, TREE).unwrap();
    std::os::unix::fs::symlink("src/main.rs", root.join("link.rs")).unwrap();

    let expected = oracle::naive_scan(&root).unwrap();
    for with_stat in [false, true] {
        let s = LinuxScanner::new().with_stat(with_stat);
        let (idx, stats) =
            build_index(&s, &root, VolumeConfig::unix(), &ScanOptions::default()).unwrap();
        assert_eq!(stats.errors, 0);
        let want: BTreeSet<PathBuf> = expected.keys().cloned().collect();
        assert_eq!(live_paths(&idx), want, "with_stat={with_stat}");
        for id in 1..idx.len() as Id {
            let p = idx.resolve_path(id);
            let o = &expected[&p];
            assert_eq!(idx.is_dir(id), o.kind == Kind::Dir, "{p:?}");
            if with_stat {
                if o.kind == Kind::File {
                    assert_eq!(idx.size(id), o.size, "size of {p:?}");
                }
                assert_eq!(idx.mtime(id), o.mtime, "mtime of {p:?}");
            }
        }
    }
}

#[test]
fn volume_id_is_heuristic_and_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let s = LinuxScanner::new();
    let a = s.volume_id(&root).unwrap();
    let b = s.volume_id(&root).unwrap();
    assert_eq!(a.kind, VolumeIdKind::LinuxHeuristic);
    assert_eq!(a, b);
    assert_eq!(a.bytes.len(), 24);
}

mod watcher {
    use everyfiles_backend_linux::InotifyWatcher;
    use everyfiles_core::watch::{PollOutcome, WatchEvent, Watcher};
    use std::time::{Duration, Instant};

    fn wait_for(w: &mut dyn Watcher, want: &std::path::Path) -> bool {
        let start = Instant::now();
        let mut buf = Vec::new();
        while start.elapsed() < Duration::from_secs(10) {
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
    fn delivers_events_including_in_new_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("existing")).unwrap();
        let mut w = InotifyWatcher::new();
        assert_eq!(w.probe(&root).unwrap().active, "inotify");
        w.start(&root, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let f = root.join("existing/a.txt");
        std::fs::write(&f, b"a").unwrap();
        assert!(wait_for(&mut w, &f), "event in a pre-existing subdir");

        let d = root.join("newdir");
        std::fs::create_dir(&d).unwrap();
        assert!(wait_for(&mut w, &d), "directory creation");
        std::thread::sleep(Duration::from_millis(100));
        let g = d.join("inside.txt");
        std::fs::write(&g, b"g").unwrap();
        assert!(
            wait_for(&mut w, &g),
            "event inside a directory created after start"
        );

        let (watched, unwatched) = w.coverage();
        assert!(watched >= 3, "root, existing, newdir");
        assert_eq!(unwatched, 0);
        w.stop().unwrap();
    }
}

mod fanotify_root {
    use everyfiles_backend_linux::{FanotifyWatcher, LinuxWatcher};
    use everyfiles_core::watch::{PollOutcome, WatchEvent, Watcher};
    use std::time::{Duration, Instant};

    /// Needs root; CI runs `sudo cargo test -- --ignored root_`.
    #[test]
    #[ignore]
    fn root_fanotify_delivers_events_and_auto_tier_picks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        FanotifyWatcher::availability(&root).expect("running as root on a recent kernel");
        let mut w = LinuxWatcher::new();
        assert_eq!(w.probe(&root).unwrap().active, "fanotify");
        w.start(&root, None).unwrap();
        assert!(w.capabilities().needs_privilege);
        std::thread::sleep(Duration::from_millis(200));
        let f = root.join("deep.txt");
        std::fs::write(&f, b"x").unwrap();
        let start = Instant::now();
        let mut seen = false;
        let mut buf = Vec::new();
        while start.elapsed() < Duration::from_secs(10) && !seen {
            buf.clear();
            if let Ok(PollOutcome::Events(_)) = w.poll(Duration::from_millis(200), &mut buf) {
                seen = buf
                    .iter()
                    .any(|e| matches!(e, WatchEvent::Path { path, .. } if path == &f));
            }
        }
        assert!(seen, "fanotify create event resolved to a path");
        w.stop().unwrap();
    }
}
