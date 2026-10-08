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

#[test]
fn test_cpu_second_tick_emits_count_and_total_busy_percent() {
    use crate::sink::{Capture, Recorded};

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), Default::default());
    let mut collector = super::cpu::CpuCollector::new();
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);
    assert!(cap.metric_values("system.cpu.count").is_empty());
    std::thread::sleep(std::time::Duration::from_millis(50));
    collector.collect(&sink, "test", &mut buf);

    let counts = cap.metric_values("system.cpu.count");
    assert_eq!(counts.len(), 1);
    assert!(counts[0] >= 1.0);

    for event in cap.events() {
        let Recorded::Metric {
            name,
            value,
            labels,
            ..
        } = event
        else {
            continue;
        };
        if name == "system.cpu.busy_percent" {
            assert_eq!(labels, vec![("core".to_string(), "total".to_string())]);
            assert!((0.0..=100.0).contains(&value));
        }
    }
}
