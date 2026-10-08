use crate::metrics::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn memory_collects_without_panic() {
    let sink = test_sink();
    let mut collector = super::memory::MemoryCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);
}

#[test]
fn test_memory_used_percent_matches_used_over_total() {
    use crate::sink::Capture;

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), Default::default());
    let mut collector = super::memory::MemoryCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);

    let totals = cap.metric_values("system.memory.total");
    let useds = cap.metric_values("system.memory.used");
    let pcts = cap.metric_values("system.memory.used_percent");
    assert_eq!(pcts.len(), 1);
    assert!((0.0..=100.0).contains(&pcts[0]));
    assert!((pcts[0] - useds[0] / totals[0] * 100.0).abs() < 1e-9);
    for pct in cap.metric_values("system.memory.swap_used_percent") {
        assert!((0.0..=100.0).contains(&pct));
    }
}
