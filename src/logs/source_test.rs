use super::source::{next_backoff, write_checkpoint};
use std::time::Duration;

#[test]
fn test_next_backoff_doubles() {
    assert_eq!(
        next_backoff(Duration::from_secs(1), Duration::from_secs(30)),
        Duration::from_secs(2)
    );
    assert_eq!(
        next_backoff(Duration::from_secs(8), Duration::from_secs(30)),
        Duration::from_secs(16)
    );
}

#[test]
fn test_next_backoff_clamps_to_max() {
    assert_eq!(
        next_backoff(Duration::from_secs(20), Duration::from_secs(30)),
        Duration::from_secs(30)
    );
    assert_eq!(
        next_backoff(Duration::from_secs(30), Duration::from_secs(30)),
        Duration::from_secs(30)
    );
}

#[test]
fn test_write_checkpoint_atomic_and_no_tmp_left() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("checkpoint");
    write_checkpoint(&path, b"first");
    assert_eq!(std::fs::read(&path).expect("read"), b"first");

    // The tmp sibling must not linger after a successful rename.
    let tmp = path.with_extension("tmp");
    assert!(!tmp.exists(), "tmp file should be renamed away");

    // Overwriting replaces the content atomically.
    write_checkpoint(&path, b"second");
    assert_eq!(std::fs::read(&path).expect("read"), b"second");
}

#[test]
fn test_write_checkpoint_unwritable_dir_is_noop() {
    // A path under a nonexistent directory cannot be created; the helper must
    // swallow the error, not panic, and leave nothing behind.
    let path = std::path::Path::new("/nonexistent-witness-dir-xyz/checkpoint");
    write_checkpoint(path, b"data");
    assert!(!path.exists());
}

#[test]
fn test_write_checkpoint_target_is_a_directory_is_noop_no_panic() {
    // The final `rename` fails when the destination is an existing directory
    // (not a plain file); the helper must swallow that error too, not panic,
    // and must not touch the directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("checkpoint");
    std::fs::create_dir(&path).expect("mkdir");

    write_checkpoint(&path, b"data");

    assert!(path.is_dir(), "the existing directory must be left intact");
}
