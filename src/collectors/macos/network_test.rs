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
