use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::disk_space::{SpaceReader, SpaceStats, fs_type_allowed};
use crate::sink::{DryRun, Sink};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn stats(total: f64, free: f64, avail: f64, inodes_total: f64, inodes_free: f64) -> SpaceStats {
    SpaceStats {
        total,
        free,
        avail,
        inodes_total,
        inodes_free,
    }
}

#[test]
fn used_percent_matches_df_with_reserved_blocks() {
    // ext4, 100 GiB, 5% reserved: 90 GiB used, 10 GiB free, 5 GiB available.
    // df: Use% = 90 / (90 + 5) = 94.7%, not 90%.
    let s = stats(100.0 * GIB, 10.0 * GIB, 5.0 * GIB, 0.0, 0.0);
    assert_eq!(s.used(), 90.0 * GIB);
    let pct = s.used_percent().unwrap();
    assert!((pct - 94.736_842).abs() < 1e-4, "{pct}");
}

#[test]
fn used_percent_is_100_when_only_reserved_blocks_remain() {
    // Unprivileged writers already get ENOSPC here — must read 100%.
    let s = stats(100.0 * GIB, 5.0 * GIB, 0.0, 0.0, 0.0);
    assert_eq!(s.used_percent(), Some(100.0));
}

#[test]
fn used_percent_without_reserved_blocks() {
    let s = stats(100.0 * GIB, 25.0 * GIB, 25.0 * GIB, 0.0, 0.0);
    assert_eq!(s.used_percent(), Some(75.0));
}

#[test]
fn used_percent_empty_filesystem_is_zero() {
    let s = stats(100.0 * GIB, 100.0 * GIB, 100.0 * GIB, 0.0, 0.0);
    assert_eq!(s.used_percent(), Some(0.0));
}

#[test]
fn used_percent_none_when_no_denominator() {
    let s = stats(100.0 * GIB, 100.0 * GIB, 0.0, 0.0, 0.0);
    assert_eq!(s.used_percent(), None);
}

#[test]
fn inodes_used_percent() {
    let s = stats(GIB, GIB, GIB, 1000.0, 250.0);
    assert_eq!(s.inodes_used(), 750.0);
    assert_eq!(s.inodes_used_percent(), Some(75.0));
}

#[test]
fn inodes_percent_none_without_inode_table() {
    // btrfs / zfs report f_files = 0.
    let s = stats(GIB, GIB, GIB, 0.0, 0.0);
    assert_eq!(s.inodes_used_percent(), None);
}

#[test]
fn emit_counts_all_gauges() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    stats(100.0 * GIB, 10.0 * GIB, 5.0 * GIB, 1000.0, 250.0).emit(&sink, &[("mount", "/")]);
    // 5 space gauges + 4 inode gauges.
    assert_eq!(dr.count(), 9);
}

#[test]
fn emit_skips_inodes_without_inode_table() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    stats(100.0 * GIB, 10.0 * GIB, 5.0 * GIB, 0.0, 0.0).emit(&sink, &[("mount", "/")]);
    assert_eq!(dr.count(), 5);
}

#[test]
fn read_root_returns_consistent_stats() {
    let s = SpaceStats::read("/").expect("statvfs on / succeeds");
    assert!(s.total > 0.0);
    assert!(s.free <= s.total);
    assert!(s.avail <= s.free);
}

#[test]
fn read_missing_path_is_none() {
    assert!(SpaceStats::read("/definitely/not/a/mount/point").is_none());
}

#[test]
fn fs_type_defaults_when_unconfigured() {
    let defaults = ["ext4", "xfs"];
    assert!(fs_type_allowed("ext4", &[], &defaults));
    assert!(!fs_type_allowed("nfs4", &[], &defaults));
}

#[test]
fn fs_type_configured_list_replaces_defaults() {
    let defaults = ["ext4", "xfs"];
    let configured = vec!["ext4".to_string(), "nfs4".to_string()];
    assert!(fs_type_allowed("nfs4", &configured, &defaults));
    assert!(fs_type_allowed("ext4", &configured, &defaults));
    assert!(!fs_type_allowed("xfs", &configured, &defaults));
}

const TEST_TIMEOUT: Duration = Duration::from_millis(50);

/// A reader whose "/hung" mount blocks until the returned sender fires (or is
/// dropped); every other mount answers immediately. Counts calls per run.
fn gated_reader() -> (
    impl Fn(&str) -> Option<SpaceStats> + Send + Sync + 'static,
    mpsc::Sender<()>,
    Arc<AtomicUsize>,
) {
    let (release, gate) = mpsc::channel::<()>();
    let gate: Mutex<Receiver<()>> = Mutex::new(gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let reader = move |mount: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        if mount == "/hung" {
            let _ = gate.lock().map(|g| g.recv());
        }
        Some(stats(GIB, GIB / 2.0, GIB / 2.0, 0.0, 0.0))
    };
    (reader, release, calls)
}

/// Poll until the stuck read finishes and the mount is read again.
fn read_until_some<F>(space: &mut SpaceReader<F>, mount: &str) -> Option<SpaceStats>
where
    F: Fn(&str) -> Option<SpaceStats> + Send + Sync + 'static,
{
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(s) = space.read(mount) {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn reader_passes_normal_reads_through() {
    let (reader, _release, calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert_eq!(space.read("/").map(|s| s.total), Some(GIB));
    assert_eq!(space.read("/").map(|s| s.total), Some(GIB));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn reader_passes_failures_through() {
    let mut space = SpaceReader::with_reader(|_: &str| None, TEST_TIMEOUT);
    assert!(space.read("/").is_none());
}

#[test]
fn reader_skips_mount_on_timeout() {
    let (reader, _release, _calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    let start = Instant::now();
    assert!(space.read("/hung").is_none());
    assert!(start.elapsed() >= TEST_TIMEOUT);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn reader_does_not_respawn_while_stuck() {
    let (reader, _release, calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert!(space.read("/hung").is_none());
    let start = Instant::now();
    for _ in 0..5 {
        assert!(space.read("/hung").is_none());
    }
    // Skipped without waiting out the timeout, and no new thread spawned.
    assert!(start.elapsed() < TEST_TIMEOUT);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn reader_stuck_mount_does_not_block_others() {
    let (reader, _release, _calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert!(space.read("/hung").is_none());
    assert!(space.read("/").is_some());
}

#[test]
fn reader_reads_again_once_stuck_call_returns() {
    let (reader, release, calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert!(space.read("/hung").is_none());
    assert!(space.read("/hung").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Closing the gate unblocks the stuck call and every later one.
    drop(release);
    assert!(read_until_some(&mut space, "/hung").is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn reader_recovers_after_panicking_read() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let reader = move |_: &str| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("first read panics");
        }
        Some(stats(GIB, GIB, GIB, 0.0, 0.0))
    };
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert!(space.read("/").is_none());
    assert!(read_until_some(&mut space, "/").is_some());
}

#[test]
fn default_reader_reads_root() {
    let mut space = SpaceReader::default();
    assert!(space.read("/").is_some_and(|s| s.total > 0.0));
}

#[test]
fn retain_drops_unmounted_but_keeps_stuck() {
    let (reader, _release, _calls) = gated_reader();
    let mut space = SpaceReader::with_reader(reader, TEST_TIMEOUT);
    assert!(space.read("/").is_some());
    assert!(space.read("/gone").is_some());
    assert!(space.read("/hung").is_none());
    assert_eq!(space.tracked_mounts(), 3);

    space.retain_mounts(["/"]);
    // "/gone" dropped; "/hung" kept while its read is still outstanding.
    assert_eq!(space.tracked_mounts(), 2);
    assert!(space.read("/hung").is_none());
}
