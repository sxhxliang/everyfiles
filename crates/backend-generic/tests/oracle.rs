//! Differential test: the generic backend's index must equal a naive walk.

use std::collections::BTreeSet;
use std::path::PathBuf;

use everyfiles_backend_generic::GenericScanner;
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::scan::{Kind, ScanOptions, build_index};
use everyfiles_core::{Id, Index};
use everyfiles_testkit::{fixture, oracle};

fn live_paths(idx: &Index) -> BTreeSet<PathBuf> {
    (1..idx.len() as Id)
        .filter(|&i| !idx.is_tombstone(i))
        .map(|i| idx.resolve_path(i))
        .collect()
}

fn volume_config() -> VolumeConfig {
    if cfg!(windows) {
        VolumeConfig::windows()
    } else {
        VolumeConfig::unix()
    }
}

#[test]
fn generic_backend_matches_naive_walk() {
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
    #[cfg(unix)]
    std::os::unix::fs::symlink("src/main.rs", root.join("link.rs")).unwrap();

    let expected = oracle::naive_scan(&root).unwrap();
    let (idx, stats) = build_index(
        &GenericScanner::new(),
        &root,
        volume_config(),
        &ScanOptions::default(),
    )
    .unwrap();

    let want: BTreeSet<PathBuf> = expected.keys().cloned().collect();
    assert_eq!(live_paths(&idx), want);
    assert_eq!(stats.errors, 0);

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
fn root_must_be_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let err = build_index(
        &GenericScanner::new(),
        &f,
        volume_config(),
        &ScanOptions::default(),
    )
    .expect_err("file root is rejected");
    assert!(
        matches!(err, everyfiles_core::Error::NotADirectory(_)),
        "{err}"
    );
}
