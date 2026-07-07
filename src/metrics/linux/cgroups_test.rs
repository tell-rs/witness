use std::collections::HashMap;

use tempfile::TempDir;

use super::cgroups::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Sink};

// --- parse_cpu_stat ---

#[test]
fn test_parse_cpu_stat_all_fields_present() {
    let input = "\
usage_usec 500000
user_usec 300000
system_usec 200000
nr_periods 12
nr_throttled 3
throttled_usec 4500
";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 500000);
    assert_eq!(stats.user_usec, 300000);
    assert_eq!(stats.system_usec, 200000);
    assert_eq!(stats.nr_throttled, 3);
    assert_eq!(stats.throttled_usec, 4500);
}

#[test]
fn test_parse_cpu_stat_missing_fields_default_to_zero() {
    let stats = parse_cpu_stat("usage_usec 100\n");
    assert_eq!(stats.usage_usec, 100);
    assert_eq!(stats.user_usec, 0);
    assert_eq!(stats.system_usec, 0);
    assert_eq!(stats.nr_throttled, 0);
    assert_eq!(stats.throttled_usec, 0);
}

#[test]
fn test_parse_cpu_stat_empty_input() {
    let stats = parse_cpu_stat("");
    assert_eq!(stats.usage_usec, 0);
    assert_eq!(stats.user_usec, 0);
    assert_eq!(stats.system_usec, 0);
}

#[test]
fn test_parse_cpu_stat_malformed_value_skipped() {
    let input = "\
usage_usec not_a_number
user_usec 42
";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 0);
    assert_eq!(stats.user_usec, 42);
}

#[test]
fn test_parse_cpu_stat_key_with_no_value_skipped() {
    let input = "usage_usec\nsystem_usec 999\n";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 0);
    assert_eq!(stats.system_usec, 999);
}

#[test]
fn test_parse_cpu_stat_unknown_keys_ignored() {
    let input = "\
usage_usec 10
some_future_field 20
user_usec 5
";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 10);
    assert_eq!(stats.user_usec, 5);
}

// --- CgroupCollector.collect against a fixture cgroup directory ---
//
// `cgroup_path` and `prev_cpu` were widened to `pub(crate)` so tests can
// construct a collector pointed at a tempdir fixture instead of relying on
// `detect_cgroup_path()`'s hardcoded "/sys/fs/cgroup" + "/proc/self/cgroup"
// lookup, which isn't fixture-friendly.

fn fixture_collector(dir: &TempDir) -> CgroupCollector {
    CgroupCollector {
        cgroup_path: Some(dir.path().to_string_lossy().into_owned()),
        prev_cpu: None,
    }
}

fn write(dir: &TempDir, name: &str, contents: &str) {
    std::fs::write(dir.path().join(name), contents)
        .unwrap_or_else(|e| panic!("failed to write fixture {name}: {e}"));
}

#[test]
fn test_cgroup_collector_no_path_is_noop() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = CgroupCollector {
        cgroup_path: None,
        prev_cpu: None,
    };
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    assert!(cap.events().is_empty());
}

#[test]
fn test_cgroup_collector_first_tick_emits_memory_but_no_cpu_counters() {
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    write(
        &dir,
        "cpu.stat",
        "usage_usec 1000\nuser_usec 600\nsystem_usec 400\n",
    );
    write(&dir, "memory.current", "104857600\n");
    write(&dir, "memory.max", "max\n");
    write(
        &dir,
        "memory.stat",
        "anon 50000000\nfile 20000000\nkernel 1000000\n",
    );

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = fixture_collector(&dir);
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    // No previous CPU snapshot yet — no counter metrics.
    assert!(cap.metric_values("system.cgroup.cpu.usage_usec").is_empty());

    // Memory gauges don't depend on delta state — emitted on tick one.
    assert_eq!(
        cap.metric_values("system.cgroup.memory.usage"),
        vec![104857600.0]
    );
    assert_eq!(
        cap.metric_values("system.cgroup.memory.anon"),
        vec![50000000.0]
    );
    assert_eq!(
        cap.metric_values("system.cgroup.memory.file"),
        vec![20000000.0]
    );
    assert_eq!(
        cap.metric_values("system.cgroup.memory.kernel"),
        vec![1000000.0]
    );

    // "max" sentinel means unlimited — no limit gauge at all.
    assert!(cap.metric_values("system.cgroup.memory.limit").is_empty());
}

#[test]
fn test_cgroup_collector_numeric_memory_limit_emits_gauge() {
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    write(&dir, "cpu.stat", "usage_usec 1\n");
    write(&dir, "memory.current", "1000\n");
    write(&dir, "memory.max", "2147483648\n");
    write(&dir, "memory.stat", "anon 1\nfile 1\n");

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = fixture_collector(&dir);
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    assert_eq!(
        cap.metric_values("system.cgroup.memory.limit"),
        vec![2147483648.0]
    );
}

#[test]
fn test_cgroup_collector_second_tick_emits_cpu_deltas() {
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    write(
        &dir,
        "cpu.stat",
        "usage_usec 1000\nuser_usec 600\nsystem_usec 400\n",
    );
    write(&dir, "memory.current", "1000\n");
    write(&dir, "memory.max", "max\n");
    write(&dir, "memory.stat", "anon 1\nfile 1\n");

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = fixture_collector(&dir);
    let mut buf = String::new();

    // First tick: seeds prev_cpu.
    collector.collect(&sink, "test-host", &mut buf);

    // Advance the counters and collect again.
    write(
        &dir,
        "cpu.stat",
        "usage_usec 1500\nuser_usec 900\nsystem_usec 600\n",
    );
    collector.collect(&sink, "test-host", &mut buf);

    assert_eq!(
        cap.metric_values("system.cgroup.cpu.usage_usec"),
        vec![500.0]
    );
    assert_eq!(
        cap.metric_values("system.cgroup.cpu.user_usec"),
        vec![300.0]
    );
    assert_eq!(
        cap.metric_values("system.cgroup.cpu.system_usec"),
        vec![200.0]
    );
}

#[test]
fn test_cgroup_collector_cpu_counters_never_go_negative_on_reset() {
    // If the cgroup's cpu.stat counters ever reset (e.g. cgroup recreated),
    // saturating_sub must clamp the delta to zero rather than underflow.
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    write(
        &dir,
        "cpu.stat",
        "usage_usec 5000\nuser_usec 3000\nsystem_usec 2000\n",
    );
    write(&dir, "memory.current", "1000\n");
    write(&dir, "memory.max", "max\n");
    write(&dir, "memory.stat", "anon 1\nfile 1\n");

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = fixture_collector(&dir);
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    write(
        &dir,
        "cpu.stat",
        "usage_usec 100\nuser_usec 50\nsystem_usec 25\n",
    );
    collector.collect(&sink, "test-host", &mut buf);

    assert_eq!(cap.metric_values("system.cgroup.cpu.usage_usec"), vec![0.0]);
    assert_eq!(cap.metric_values("system.cgroup.cpu.user_usec"), vec![0.0]);
    assert_eq!(
        cap.metric_values("system.cgroup.cpu.system_usec"),
        vec![0.0]
    );
}

#[test]
fn test_cgroup_collector_missing_files_do_not_panic() {
    // Empty directory — none of cpu.stat/memory.* exist.
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = fixture_collector(&dir);
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    assert!(cap.events().is_empty());
}
