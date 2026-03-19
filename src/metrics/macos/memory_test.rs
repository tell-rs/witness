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
