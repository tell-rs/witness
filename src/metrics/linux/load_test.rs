use std::collections::HashMap;

use super::load::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Recorded, Sink};

// load.rs has no extracted pure parse function — the three-float parse is
// inlined directly in `collect()`, which hardcodes the "/proc/loadavg" path.
// These tests exercise the real Collector against the real file, which is
// always present and world-readable on any Linux host these tests run on.
// If it were ever unreadable, `read_procfs` fails and `collect()` returns
// early with zero emissions — both branches are covered by the assertions
// below without flakiness.

#[test]
fn test_load_collector_collect_does_not_panic() {
    let sink = Sink::capture(Capture::new(), HashMap::new());
    let mut collector = LoadCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);
}

#[test]
fn test_load_collector_emits_at_most_three_gauges_named_load_1_5_15() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = LoadCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    let events = cap.events();
    // /proc/loadavg has 5 whitespace fields ("0.52 0.58 0.59 1/1234 5678")
    // but the collector only ever reads the first three — the running/total
    // process count and last pid must never surface as metrics.
    assert!(events.len() <= 3, "unexpected extra metrics: {events:?}");

    let mut seen_names: Vec<&str> = Vec::new();
    for event in &events {
        if let Recorded::Metric { name, value, .. } = event {
            assert!(
                matches!(*name, "system.load.1" | "system.load.5" | "system.load.15"),
                "unexpected metric name: {name}"
            );
            assert!(value.is_finite());
            assert!(*value >= 0.0, "load average must not be negative: {value}");
            seen_names.push(name);
        }
    }
    // No duplicate metric names — each of the three loads emitted at most once.
    let unique: std::collections::HashSet<_> = seen_names.iter().collect();
    assert_eq!(unique.len(), seen_names.len());
}

#[test]
fn test_load_collector_gauges_carry_no_labels() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = LoadCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    for event in cap.events() {
        if let Recorded::Metric { labels, .. } = event {
            assert!(
                labels.is_empty(),
                "load metrics should be host-global, unlabeled"
            );
        }
    }
}
