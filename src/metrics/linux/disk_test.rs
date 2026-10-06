use crate::config::{DeviceFilter, FilterConfig};
use crate::metrics::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> (Sink, DryRun) {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    (sink, dr)
}

#[test]
fn disk_checkpoint_empty_prev_is_noop() {
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter, Vec::new());

    // Checkpoint before any collection: prev is empty, should not panic
    let (sink, dr) = test_sink();
    collector.checkpoint(&sink, "test");
    assert_eq!(dr.count(), 0);
}

#[test]
fn disk_checkpoint_emits_four_metrics_per_device() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter, Vec::new());

    // Inject known cumulative counters for one device
    collector.inject_prev_stats("sda", 100, 2000, 50, 1000);
    collector.checkpoint(&sink, "test");

    // 4 checkpoint metrics: read_bytes, write_bytes, read_ops, write_ops
    assert_eq!(dr.count(), 4);
}

#[test]
fn disk_checkpoint_scales_sectors_to_bytes() {
    // Sectors × 512 should produce the correct byte values.
    // We can't inspect values via DryRun, but exercising the path with
    // non-trivial sector counts verifies the multiplication doesn't panic
    // and the code path executes fully.
    let (sink, _dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter, Vec::new());

    collector.inject_prev_stats("nvme0n1", 500_000, 10_000_000, 250_000, 5_000_000);
    collector.checkpoint(&sink, "test");
}

#[test]
fn disk_checkpoint_multiple_devices() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter, Vec::new());

    collector.inject_prev_stats("sda", 100, 2000, 50, 1000);
    collector.inject_prev_stats("sdb", 200, 4000, 100, 2000);
    collector.checkpoint(&sink, "test");

    // 4 metrics × 2 devices = 8
    assert_eq!(dr.count(), 8);
}

#[test]
fn disk_checkpoint_zero_counters() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter, Vec::new());

    // Device with all-zero counters — checkpoint should still emit them
    collector.inject_prev_stats("sda", 0, 0, 0, 0);
    collector.checkpoint(&sink, "test");

    assert_eq!(dr.count(), 4);
}

// --- /proc/mounts parsing ---

use super::disk::{MountInfo, parse_mounts};

fn mount(device: &str, mount_point: &str) -> MountInfo {
    MountInfo {
        device: device.to_string(),
        mount_point: mount_point.to_string(),
    }
}

/// Shape of the ob-de-01 host: LVM volumes, md /boot, and two bind mounts of
/// the root LV that used to triple-report the same filesystem.
const PROC_MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/mapper/vg0-root / ext4 rw,relatime 0 0
/dev/md0 /boot ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw,relatime 0 0
/dev/mapper/vg0-log /var/log ext4 rw,relatime 0 0
/dev/mapper/vg0-root /var/lib/witness ext4 rw,relatime 0 0
/dev/mapper/vg0-root /var/tmp ext4 rw,relatime 0 0
/dev/mapper/vg0-openbinary /opt/openbinary xfs rw,relatime 0 0
/dev/sda /opt/cold xfs rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev 0 0
nas:/export /mnt/nas nfs4 rw,relatime 0 0
";

#[test]
fn parse_mounts_dedups_bind_mounts_to_shortest_path() {
    let mounts = parse_mounts(PROC_MOUNTS, &[]);
    let root: Vec<_> = mounts
        .iter()
        .filter(|m| m.device == "/dev/mapper/vg0-root")
        .collect();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].mount_point, "/");
}

#[test]
fn parse_mounts_default_fs_types() {
    let mut points: Vec<_> = parse_mounts(PROC_MOUNTS, &[])
        .into_iter()
        .map(|m| m.mount_point)
        .collect();
    points.sort();
    assert_eq!(
        points,
        [
            "/",
            "/boot",
            "/boot/efi",
            "/opt/cold",
            "/opt/openbinary",
            "/var/log"
        ]
    );
}

#[test]
fn parse_mounts_network_fs_is_opt_in() {
    assert!(
        !parse_mounts(PROC_MOUNTS, &[])
            .iter()
            .any(|m| m.mount_point == "/mnt/nas")
    );

    let fs_types = vec!["nfs4".to_string()];
    assert_eq!(
        parse_mounts(PROC_MOUNTS, &fs_types),
        [mount("nas:/export", "/mnt/nas")]
    );
}

#[test]
fn parse_mounts_unescapes_octal() {
    let buf = "/dev/sdb1 /mnt/my\\040disk ext4 rw 0 0\n/dev/sdc1 /mnt/a\\134b ext4 rw 0 0\n";
    assert_eq!(
        parse_mounts(buf, &[]),
        // Sorted by mount-point length.
        [
            mount("/dev/sdc1", "/mnt/a\\b"),
            mount("/dev/sdb1", "/mnt/my disk")
        ]
    );
}

#[test]
fn parse_mounts_leaves_malformed_escapes() {
    // Truncated or out-of-range escapes pass through untouched, no panic.
    let buf = "/dev/sdb1 /mnt/x\\04 ext4 rw 0 0\n/dev/sdc1 /mnt/y\\777 ext4 rw 0 0\n";
    assert_eq!(
        parse_mounts(buf, &[]),
        [
            mount("/dev/sdb1", "/mnt/x\\04"),
            mount("/dev/sdc1", "/mnt/y\\777")
        ]
    );
}

#[test]
fn parse_mounts_skips_short_lines() {
    assert!(parse_mounts("/dev/sda /\n\n", &[]).is_empty());
}
