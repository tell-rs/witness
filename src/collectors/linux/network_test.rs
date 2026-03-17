use crate::collectors::Collector;
use crate::config::{DeviceFilter, FilterConfig};
use crate::sink::{DryRun, Sink};

fn test_sink() -> (Sink, DryRun) {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    (sink, dr)
}

#[test]
fn network_checkpoint_empty_prev_is_noop() {
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    // Checkpoint before any collection: prev is empty, should not panic
    let (sink, dr) = test_sink();
    collector.checkpoint(&sink, "test");
    assert_eq!(dr.count(), 0);
}

#[test]
fn network_checkpoint_emits_four_metrics_per_interface() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    // Inject known cumulative counters for one interface
    collector.inject_prev_stats("eth0", 1_000_000, 5000, 500_000, 2500);
    collector.checkpoint(&sink, "test");

    // 4 checkpoint metrics: bytes_recv, bytes_sent, packets_recv, packets_sent
    assert_eq!(dr.count(), 4);
}

#[test]
fn network_checkpoint_multiple_interfaces() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    collector.inject_prev_stats("eth0", 1_000_000, 5000, 500_000, 2500);
    collector.inject_prev_stats("eth1", 2_000_000, 10_000, 1_000_000, 5000);
    collector.checkpoint(&sink, "test");

    // 4 metrics × 2 interfaces = 8
    assert_eq!(dr.count(), 8);
}

#[test]
fn network_checkpoint_zero_counters() {
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    // Interface with all-zero counters — checkpoint should still emit them
    collector.inject_prev_stats("eth0", 0, 0, 0, 0);
    collector.checkpoint(&sink, "test");

    assert_eq!(dr.count(), 4);
}

#[test]
fn network_checkpoint_large_cumulative_values() {
    // High-traffic interface — verify no overflow or panic with large values
    let (sink, dr) = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    collector.inject_prev_stats(
        "eth0",
        500_000_000_000, // ~500 GB received
        300_000_000,     // 300M packets
        200_000_000_000, // ~200 GB sent
        150_000_000,     // 150M packets
    );
    collector.checkpoint(&sink, "test");

    assert_eq!(dr.count(), 4);
}
