//! Open-path behaviour: load when fresh, rescan when stale.

use std::path::Path;

use everyfiles_backend_generic::GenericScanner;
use everyfiles_core::engine::{OpenOptions, OpenOutcome, RescanReason, open};
use everyfiles_core::platform::VolumeConfig;
use everyfiles_core::query::{Query, search};
use everyfiles_testkit::fixture;

fn cfg() -> VolumeConfig {
    if cfg!(windows) {
        VolumeConfig::windows()
    } else {
        VolumeConfig::unix()
    }
}

fn names(idx: &everyfiles_core::Index, q: &str) -> Vec<String> {
    let mut v: Vec<String> = search(idx, &Query::new(q))
        .unwrap()
        .ids
        .iter()
        .map(|&i| String::from_utf8_lossy(idx.name(i)).into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn second_open_loads_and_answers_identically() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    fixture::create_tree(&root, &["a/one.rs", "a/two.rs", "b/three.md", "c/"]).unwrap();
    let idx_path = tmp.path().join("cache").join("t.idx");
    let s = GenericScanner::new();

    let (i1, o1) = open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    assert!(matches!(
        o1,
        OpenOutcome::Rescanned {
            reason: RescanReason::NoIndex,
            ..
        }
    ));
    assert!(idx_path.exists());

    let (i2, o2) = open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    assert!(matches!(o2, OpenOutcome::Loaded { .. }), "{o2:?}");
    assert_eq!(i1.len(), i2.len());
    assert_eq!(names(&i1, ".rs"), names(&i2, ".rs"));
    assert_eq!(names(&i2, ".rs"), ["one.rs", "two.rs"]);
}

#[test]
fn force_rescan_is_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    fixture::create_tree(&root, &["x.txt"]).unwrap();
    let idx_path = tmp.path().join("t.idx");
    let s = GenericScanner::new();
    open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    let (_, o) = open(
        &s,
        &root,
        &idx_path,
        cfg(),
        &OpenOptions {
            force_rescan: true,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    assert!(matches!(
        o,
        OpenOutcome::Rescanned {
            reason: RescanReason::Forced,
            ..
        }
    ));
}

#[test]
fn changed_file_fails_spot_check_and_rescans() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    // One file so the spot-check must sample it.
    fixture::create_tree(&root, &["only.txt"]).unwrap();
    let idx_path = tmp.path().join("t.idx");
    let s = GenericScanner::new();
    open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();

    std::fs::write(root.join("only.txt"), b"a much longer body than before").unwrap();
    let (idx, o) = open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    match o {
        OpenOutcome::Rescanned {
            reason: RescanReason::SpotCheckFailed(p),
            ..
        } => assert!(p.ends_with("only.txt"), "{p:?}"),
        other => panic!("expected spot-check rescan, got {other:?}"),
    }
    let id = search(&idx, &Query::new("only")).unwrap().ids[0];
    assert_eq!(idx.size(id), 30);
}

#[test]
fn corrupt_file_rescans() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    fixture::create_tree(&root, &["x.txt"]).unwrap();
    let idx_path = tmp.path().join("t.idx");
    let s = GenericScanner::new();
    open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    std::fs::write(&idx_path, b"definitely not an index").unwrap();
    let (_, o) = open(&s, &root, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    assert!(
        matches!(
            o,
            OpenOutcome::Rescanned {
                reason: RescanReason::Corrupt(_) | RescanReason::Incompatible(_),
                ..
            }
        ),
        "{o:?}"
    );
}

#[test]
fn index_for_another_root_is_not_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fixture::create_tree(&a, &["in_a.txt"]).unwrap();
    fixture::create_tree(&b, &["in_b.txt"]).unwrap();
    let idx_path = tmp.path().join("shared.idx");
    let s = GenericScanner::new();
    open(&s, &a, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    let (idx, o) = open(&s, &b, &idx_path, cfg(), &OpenOptions::default()).unwrap();
    assert!(matches!(
        o,
        OpenOutcome::Rescanned {
            reason: RescanReason::Incompatible(_),
            ..
        }
    ));
    assert_eq!(names(&idx, ""), ["in_b.txt"]);
    let _ = Path::new("unused");
}
