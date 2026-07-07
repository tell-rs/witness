use std::collections::HashMap;

use super::cpu::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Recorded, Sink};

fn parse_line(line: &str) -> CpuTimes {
    let mut parts = line.split_whitespace();
    parse_cpu_line(&mut parts)
}

// --- parse_cpu_line: aggregate "cpu " line ---

#[test]
fn test_parse_cpu_line_aggregate_real_proc_stat_fields() {
    // Real /proc/stat aggregate line (name field already consumed upstream).
    let times = parse_line("10132153 290696 3084719 46828483 16683 0 25195 0 175628 0");
    assert_eq!(times.user, 10132153);
    assert_eq!(times.nice, 290696);
    assert_eq!(times.system, 3084719);
    assert_eq!(times.idle, 46828483);
    assert_eq!(times.iowait, 16683);
    assert_eq!(times.irq, 0);
    assert_eq!(times.softirq, 25195);
    assert_eq!(times.steal, 0);
    // guest/guest_nice (fields 9-10) are never consumed by parse_cpu_line.
    let expected_total: u64 = 10132153 + 290696 + 3084719 + 46828483 + 16683 + 25195;
    assert_eq!(times.total(), expected_total);
}

#[test]
fn test_parse_cpu_line_percore_fields() {
    // Realistic "cpuN" per-core line.
    let times = parse_line("1266519 36337 385589 5853560 2085 0 3149 0 21953 0");
    assert_eq!(times.user, 1266519);
    assert_eq!(times.nice, 36337);
    assert_eq!(times.system, 385589);
    assert_eq!(times.idle, 5853560);
    assert_eq!(times.iowait, 2085);
    assert_eq!(times.softirq, 3149);
}

#[test]
fn test_parse_cpu_line_truncated_defaults_missing_to_zero() {
    // Only 3 of 8 fields present.
    let times = parse_line("100 200 300");
    assert_eq!(times.user, 100);
    assert_eq!(times.nice, 200);
    assert_eq!(times.system, 300);
    assert_eq!(times.idle, 0);
    assert_eq!(times.iowait, 0);
    assert_eq!(times.irq, 0);
    assert_eq!(times.softirq, 0);
    assert_eq!(times.steal, 0);
    assert_eq!(times.total(), 600);
}

#[test]
fn test_parse_cpu_line_garbage_tokens_default_to_zero() {
    // Non-numeric tokens fall back to 0 via unwrap_or(0), never panic.
    let times = parse_line("abc def 300 ghi 500 jkl mno pqr");
    assert_eq!(times.user, 0);
    assert_eq!(times.nice, 0);
    assert_eq!(times.system, 300);
    assert_eq!(times.idle, 0);
    assert_eq!(times.iowait, 500);
    assert_eq!(times.irq, 0);
    assert_eq!(times.softirq, 0);
    assert_eq!(times.steal, 0);
}

#[test]
fn test_parse_cpu_line_empty_input_all_zero() {
    let times = parse_line("");
    assert_eq!(times.total(), 0);
    assert_eq!(times.user, 0);
    assert_eq!(times.steal, 0);
}

#[test]
fn test_parse_cpu_line_negative_number_token_defaults_to_zero() {
    // u64 can't represent a negative — parse() fails, falls back to 0.
    let times = parse_line("-5 100 200 300 400 500 600 700");
    assert_eq!(times.user, 0);
    assert_eq!(times.nice, 100);
}

#[test]
fn test_cpu_times_total_sums_all_eight_fields() {
    let times = CpuTimes {
        user: 1,
        nice: 2,
        system: 3,
        idle: 4,
        iowait: 5,
        irq: 6,
        softirq: 7,
        steal: 8,
    };
    assert_eq!(times.total(), 36);
}

#[test]
fn test_cpu_times_total_saturating_wide_values() {
    let times = CpuTimes {
        user: u64::MAX / 8,
        nice: u64::MAX / 8,
        system: u64::MAX / 8,
        idle: u64::MAX / 8,
        iowait: u64::MAX / 8,
        irq: u64::MAX / 8,
        softirq: u64::MAX / 8,
        steal: u64::MAX / 8,
    };
    // Sum stays within u64 range for these inputs — just verify no panic
    // and a sane (large) result.
    assert!(times.total() > 0);
}

// --- CpuCollector.collect: first-tick baseline suppression ---
//
// collect() reads the hardcoded "/proc/stat" path, so we can't inject a
// fixture. On any real Linux host (where these `#[cfg(target_os = "linux")]`
// tests actually run) that path exists and is world-readable, so this
// exercises the real code path. Either way — file present or not — the very
// first collection has an empty `prev` map, so it must never emit metrics.
#[test]
fn test_cpu_collector_first_tick_emits_no_metrics() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = CpuCollector::new();
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    assert!(
        cap.events().is_empty(),
        "first tick must only seed baseline state, not emit metrics"
    );
}

#[test]
fn test_cpu_collector_second_tick_values_are_valid_percentages() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = CpuCollector::new();
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);
    collector.collect(&sink, "test-host", &mut buf);

    // Whether or not the counters advanced between calls, any percentage
    // emitted must be a finite value in [0, 100] and carry a "core" label.
    for event in cap.events() {
        if let Recorded::Metric {
            name,
            value,
            labels,
            ..
        } = event
        {
            assert!(name.starts_with("system.cpu."), "unexpected metric {name}");
            assert!(value.is_finite());
            assert!(
                (0.0..=100.0).contains(&value),
                "{name} out of range: {value}"
            );
            assert!(labels.iter().any(|(k, _)| k == "core"));
        }
    }
}
