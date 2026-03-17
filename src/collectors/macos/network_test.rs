use crate::collectors::Collector;
use crate::config::{DeviceFilter, FilterConfig};
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn network_collects_without_panic() {
    let sink = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo0"]);
    let mut collector = super::network::NetworkCollector::new(filter);
    let mut buf = String::new();

    // First tick: stores baseline
    collector.collect(&sink, "test", &mut buf);
    // Second tick: should emit deltas
    collector.collect(&sink, "test", &mut buf);
}

#[test]
fn network_checkpoint_empty_prev_is_noop() {
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo0"]);
    let mut collector = super::network::NetworkCollector::new(filter);

    // Checkpoint before any collection: prev is empty, should not panic
    collector.checkpoint(&Sink::discard(), "test");
}

#[test]
fn network_checkpoint_after_baseline() {
    let sink = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo0"]);
    let mut collector = super::network::NetworkCollector::new(filter);
    let mut buf = String::new();

    // First collect populates prev
    collector.collect(&sink, "test", &mut buf);
    // Checkpoint should emit cumulative values without panicking
    collector.checkpoint(&sink, "test");
}

#[test]
fn network_checkpoint_after_multiple_collects() {
    let sink = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &["lo0"]);
    let mut collector = super::network::NetworkCollector::new(filter);
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);
    collector.collect(&sink, "test", &mut buf);
    collector.collect(&sink, "test", &mut buf);
    // Checkpoint after multiple deltas should work
    collector.checkpoint(&sink, "test");
}
