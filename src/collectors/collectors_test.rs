use crate::collectors::{init_collectors, read_procfs};
use crate::config::SystemConfig;

// --- read_procfs ---

#[test]
fn read_procfs_reads_file_into_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_proc");
    std::fs::write(&path, "cpu 1234 5678\nmem 9999\n").unwrap();

    let mut buf = String::new();
    read_procfs(path.to_str().unwrap(), &mut buf).unwrap();
    assert_eq!(buf, "cpu 1234 5678\nmem 9999\n");
}

#[test]
fn read_procfs_clears_buffer_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_proc");
    std::fs::write(&path, "new data").unwrap();

    let mut buf = "old leftover data".to_string();
    read_procfs(path.to_str().unwrap(), &mut buf).unwrap();
    assert_eq!(buf, "new data");
}

#[test]
fn read_procfs_nonexistent_returns_error() {
    let mut buf = String::new();
    let result = read_procfs("/tmp/tell_test_nonexistent_procfs", &mut buf);
    assert!(result.is_err());
}

// --- init_collectors (platform dispatch) ---

#[test]
fn init_collectors_returns_platform_collectors() {
    let config = SystemConfig::default();
    let collectors = init_collectors(&config);
    assert!(!collectors.is_empty());
}
