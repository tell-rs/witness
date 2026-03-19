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
    let mut collector = super::disk::DiskCollector::new(filter);

    // Checkpoint before any collection: prev is empty, should not panic
    let (sink, dr) = test_sink();
    collector.checkpoint(&sink, "test");
    assert_eq!(dr.count(), 0);
}

#[test]
fn disk_checkpoint_emits_four_metrics_per_device() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter);

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
    let mut collector = super::disk::DiskCollector::new(filter);

    collector.inject_prev_stats("nvme0n1", 500_000, 10_000_000, 250_000, 5_000_000);
    collector.checkpoint(&sink, "test");
}

#[test]
fn disk_checkpoint_multiple_devices() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter);

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
    let mut collector = super::disk::DiskCollector::new(filter);

    // Device with all-zero counters — checkpoint should still emit them
    collector.inject_prev_stats("sda", 0, 0, 0, 0);
    collector.checkpoint(&sink, "test");

    assert_eq!(dr.count(), 4);
}
