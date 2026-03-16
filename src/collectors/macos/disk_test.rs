use crate::collectors::Collector;
use crate::config::{DeviceFilter, FilterConfig};
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
