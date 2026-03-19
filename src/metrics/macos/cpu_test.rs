use crate::metrics::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn cpu_collects_without_panic() {
    let sink = test_sink();
    let mut collector = super::cpu::CpuCollector::new();
    let mut buf = String::new();

    // First tick: stores baseline
    collector.collect(&sink, "test", &mut buf);
    // Second tick: should emit deltas
    collector.collect(&sink, "test", &mut buf);
}
