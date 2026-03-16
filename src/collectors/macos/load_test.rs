use crate::collectors::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn load_returns_values() {
    let sink = test_sink();
    let mut collector = super::load::LoadCollector;
    let mut buf = String::new();

    // Should not panic
    collector.collect(&sink, "test", &mut buf);
}
