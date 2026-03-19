use crate::config::{DeviceFilter, FilterConfig};
use crate::metrics::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn disk_collects_without_panic() {
    let sink = test_sink();
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter);
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);
}

#[test]
fn disk_checkpoint_is_noop() {
    let filter = DeviceFilter::new(&FilterConfig::default(), &[]);
    let mut collector = super::disk::DiskCollector::new(filter);
    let mut buf = String::new();

    // Collect once so collector is initialized
    collector.collect(&Sink::discard(), "test", &mut buf);

    // macOS disk collector is gauges-only — checkpoint uses default no-op
    collector.checkpoint(&Sink::discard(), "test");
}
