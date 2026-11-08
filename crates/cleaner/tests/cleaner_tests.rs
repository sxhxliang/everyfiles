use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use everyfiles_cleaner::{
    CleanCategory, CleanItem, SafeDeleter, SafetyGuard, SafetyLevel, ScanSummary, format_size,
};

#[test]
fn test_safety_guard_restrictions() {
    assert!(SafetyGuard::validate(&PathBuf::from("/")).is_err());
    assert!(SafetyGuard::validate(&PathBuf::from("/System")).is_err());
    assert!(SafetyGuard::validate(&PathBuf::from("/usr/bin/tar")).is_err());
    assert!(SafetyGuard::validate(&PathBuf::from("/etc/hosts")).is_err());
}

#[test]
fn test_dry_run_preserves_files() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("dummy_cache.bin");
    {
        let mut f = File::create(&file_path).unwrap();
        f.write_all(&vec![0u8; 1024 * 1024]).unwrap(); // 1MB
    }

    assert!(file_path.exists());

    // Dry-run clean
    let item = CleanItem::new(
        "test_item",
        CleanCategory::SystemJunk,
        "测试缓存",
        "临时测试文件",
        file_path.clone(),
        1024 * 1024,
        1,
        SafetyLevel::Suggested,
    );

    let report = SafeDeleter::execute(&[item], true, false, |_i, _c, _t| {});

    assert!(report.dry_run);
    assert_eq!(report.reclaimed_bytes, 1024 * 1024);
    assert_eq!(report.cleaned_items, 1);
    // File MUST still exist after dry-run
    assert!(file_path.exists());
}

#[test]
fn test_format_size_units() {
    assert_eq!(format_size(500), "500 B");
    assert_eq!(format_size(1024), "1.0 KB");
    assert_eq!(format_size(1024 * 1024 * 10), "10.00 MB");
    assert_eq!(format_size(1024 * 1024 * 1024 * 3), "3.00 GB");
}

#[test]
fn test_scan_summary_recalculate() {
    let item1 = CleanItem::new(
        "item1",
        CleanCategory::DevJunk,
        "Cargo 缓存",
        "描述",
        PathBuf::from("/tmp/cargo"),
        1000,
        1,
        SafetyLevel::Suggested,
    );
    let item2 = CleanItem::new(
        "item2",
        CleanCategory::DevJunk,
        "npm 缓存",
        "描述",
        PathBuf::from("/tmp/npm"),
        2000,
        1,
        SafetyLevel::Caution, // unchecked by default
    );

    let group = everyfiles_cleaner::CleanGroup::new(CleanCategory::DevJunk, vec![item1, item2]);
    let mut summary = ScanSummary::new(vec![group], std::time::Duration::from_millis(50));

    assert_eq!(summary.total_size, 3000);
    assert_eq!(summary.selected_size, 1000); // Only item1 is Suggested/selected

    // Toggle item2 to selected
    summary.groups[0].items[1].selected = true;
    summary.recalculate();
    assert_eq!(summary.selected_size, 3000);
}

#[test]
fn test_category_metadata() {
    for cat in CleanCategory::all() {
        assert!(!cat.title().is_empty());
        assert!(!cat.icon().is_empty());
    }
}

#[test]
fn test_duplicate_file_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let content = b"This is identical file content used for xxHash3 duplicate testing!!";

    let file1 = tmp.path().join("copy1.txt");
    let file2 = tmp.path().join("copy2.txt");
    let file3 = tmp.path().join("different.txt");

    fs::write(&file1, content).unwrap();
    // Ensure slight time difference
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&file2, content).unwrap();
    fs::write(&file3, b"Completely different content!").unwrap();

    let report = everyfiles_cleaner::DuplicateFinder::find(&[tmp.path().to_path_buf()], 10, None);

    assert_eq!(report.groups.len(), 1);
    let group = &report.groups[0];
    assert_eq!(group.files.len(), 2);
    assert_eq!(group.file_size, content.len() as u64);
    assert_eq!(group.wasted_bytes, content.len() as u64);

    // Oldest should be original, newer marked for removal
    assert!(group.files[0].is_original);
    assert!(!group.files[0].selected_for_removal);
    assert!(!group.files[1].is_original);
    assert!(group.files[1].selected_for_removal);
}

#[test]
fn test_space_lens_exploration() {
    let tmp = tempfile::tempdir().unwrap();
    let sub1 = tmp.path().join("large_dir");
    let sub2 = tmp.path().join("small_dir");
    fs::create_dir(&sub1).unwrap();
    fs::create_dir(&sub2).unwrap();

    fs::write(sub1.join("big.bin"), vec![0u8; 10000]).unwrap();
    fs::write(sub2.join("small.bin"), vec![0u8; 1000]).unwrap();

    let (tot, nodes) = everyfiles_cleaner::SpaceLens::explore(tmp.path());
    assert!(tot >= 11000);
    assert_eq!(nodes.len(), 2);
    // Sorted descending: large_dir first
    assert_eq!(nodes[0].name, "large_dir");
    assert_eq!(nodes[1].name, "small_dir");
    assert!(nodes[0].percent > nodes[1].percent);
}

#[test]
fn test_app_uninstaller_inspection() {
    let tmp = tempfile::tempdir().unwrap();
    let app_bundle = tmp.path().join("TestMockApp.app");
    let contents = app_bundle.join("Contents");
    fs::create_dir_all(&contents).unwrap();

    let plist_content = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.test.mockapp</string>
    <key>CFBundleName</key>
    <string>TestMockApp</string>
    <key>CFBundleShortVersionString</key>
    <string>1.2.3</string>
</dict>
</plist>"#;
    fs::write(contents.join("Info.plist"), plist_content).unwrap();
    fs::write(contents.join("mock_binary"), vec![0u8; 5000]).unwrap();

    // Mock ~/Library
    let mock_home = tmp.path().join("user_home");
    let app_support = mock_home.join("Library/Application Support/com.test.mockapp");
    fs::create_dir_all(&app_support).unwrap();
    fs::write(app_support.join("data.db"), vec![0u8; 15000]).unwrap();

    let caches = mock_home.join("Library/Caches/com.test.mockapp");
    fs::create_dir_all(&caches).unwrap();
    fs::write(caches.join("cache.dat"), vec![0u8; 8000]).unwrap();

    let app =
        everyfiles_cleaner::AppUninstaller::inspect_app(&app_bundle, Some(&mock_home)).unwrap();

    assert_eq!(app.bundle_id, "com.test.mockapp");
    assert_eq!(app.version, "1.2.3");
    assert_eq!(app.name, "TestMockApp");
    // Bundle + App Support + Caches = 3 items found
    assert_eq!(app.associated_files.len(), 3);
    assert!(app.total_size_bytes >= 28000);
}

#[test]
fn test_dormant_project_artifact_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let rust_proj = tmp.path().join("my_rust_app");
    fs::create_dir_all(&rust_proj).unwrap();
    fs::write(
        rust_proj.join("Cargo.toml"),
        "[package]\nname = \"my_rust_app\"\n",
    )
    .unwrap();
    let target_dir = rust_proj.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("app_binary"), vec![0u8; 10000]).unwrap();

    let node_proj = tmp.path().join("my_node_app");
    fs::create_dir_all(&node_proj).unwrap();
    fs::write(
        node_proj.join("package.json"),
        "{\"name\": \"my_node_app\"}",
    )
    .unwrap();
    let nm_dir = node_proj.join("node_modules");
    fs::create_dir_all(&nm_dir).unwrap();
    fs::write(nm_dir.join("package.js"), vec![0u8; 20000]).unwrap();

    let artifacts = everyfiles_cleaner::DeveloperManager::scan_dormant_artifacts(
        &[tmp.path().to_path_buf()],
        0, // 0 days idle for testing
        1000,
    );

    assert_eq!(artifacts.len(), 2);
    let kinds: Vec<_> = artifacts.iter().map(|a| a.artifact_kind).collect();
    assert!(kinds.contains(&everyfiles_cleaner::ArtifactKind::RustTarget));
    assert!(kinds.contains(&everyfiles_cleaner::ArtifactKind::NodeModules));
}

#[test]
fn test_git_repository_scanner() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("git_repo");
    let git_dir = repo_dir.join(".git/objects");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("packfile"), vec![0u8; 5000]).unwrap();

    let repos =
        everyfiles_cleaner::DeveloperManager::scan_git_repositories(&[tmp.path().to_path_buf()]);
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name, "git_repo");
    assert!(repos[0].git_dir_size >= 5000);
}
