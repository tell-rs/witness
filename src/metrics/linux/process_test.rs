use std::collections::HashMap;
use std::time::Duration;

use super::process::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Recorded, Sink};

const PAGE_SIZE: u64 = 4096;

/// Build a realistic `/proc/{pid}/stat` line. `extra_after_rss` lets a test
/// truncate the trailing fields to exercise the length-guard branch.
fn stat_line(pid: u32, comm: &str, utime: u64, stime: u64, rss: u64) -> String {
    format!(
        "{pid} ({comm}) S 1 1234 1234 0 -1 4194304 100 0 5 0 {utime} {stime} 0 0 20 0 1 0 12345 135168 {rss}"
    )
}

// --- parse_proc_stat: success paths ---

#[test]
fn test_parse_proc_stat_normal_line() {
    let buf = stat_line(1234, "bash", 1500, 500, 250);
    let proc = parse_proc_stat(&buf, 1234, PAGE_SIZE).unwrap_or_else(|| panic!("expected Some"));
    assert_eq!(proc.pid, 1234);
    assert_eq!(proc.name, "bash");
    assert_eq!(proc.utime, 1500);
    assert_eq!(proc.stime, 500);
    assert_eq!(proc.rss_bytes, 250 * PAGE_SIZE);
}

#[test]
fn test_parse_proc_stat_comm_with_spaces_and_parens() {
    // Real-world example: kernel thread / tmux-style names that contain
    // both spaces and embedded parentheses inside the comm field.
    let comm = "tmux: server";
    let buf = format!("42 ({comm}) S 1 1 1 0 -1 0 0 0 0 0 10 20 0 0 20 0 1 0 0 0 100");
    let proc = parse_proc_stat(&buf, 42, PAGE_SIZE).unwrap_or_else(|| panic!("expected Some"));
    assert_eq!(proc.name, "tmux: server");
    assert_eq!(proc.utime, 10);
    assert_eq!(proc.stime, 20);
    assert_eq!(proc.rss_bytes, 100 * PAGE_SIZE);
}

#[test]
fn test_parse_proc_stat_comm_containing_close_paren() {
    // rfind(')') must pick the LAST ')' so a comm like "a(b)c" (rendered as
    // "(a(b)c)") is captured whole, not truncated at the first ')'.
    let buf = "99 (a(b)c) S 1 1 1 0 -1 0 0 0 0 0 5 6 0 0 20 0 1 0 0 0 7";
    let proc = parse_proc_stat(buf, 99, PAGE_SIZE).unwrap_or_else(|| panic!("expected Some"));
    assert_eq!(proc.name, "a(b)c");
    assert_eq!(proc.utime, 5);
    assert_eq!(proc.stime, 6);
    assert_eq!(proc.rss_bytes, 7 * PAGE_SIZE);
}

#[test]
fn test_parse_proc_stat_zero_rss_converts_to_zero_bytes() {
    let buf = stat_line(1, "init", 0, 0, 0);
    let proc = parse_proc_stat(&buf, 1, PAGE_SIZE).unwrap_or_else(|| panic!("expected Some"));
    assert_eq!(proc.rss_bytes, 0);
}

#[test]
fn test_parse_proc_stat_large_rss_scales_with_page_size() {
    let buf = stat_line(2, "big", 1, 1, 1_000_000);
    let proc = parse_proc_stat(&buf, 2, PAGE_SIZE).unwrap_or_else(|| panic!("expected Some"));
    assert_eq!(proc.rss_bytes, 1_000_000 * PAGE_SIZE);
}

// --- parse_proc_stat: error / garbage paths ---

#[test]
fn test_parse_proc_stat_no_opening_paren_returns_none() {
    assert!(parse_proc_stat("1234 bash) S 1 1", 1234, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_no_closing_paren_returns_none() {
    assert!(parse_proc_stat("1234 (bash S 1 1", 1234, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_close_before_open_returns_none() {
    // ')' appears earlier in the buffer than '(' — rfind/find combination
    // must reject rather than slice with a reversed range.
    let buf = "1234 )bash(";
    assert!(parse_proc_stat(buf, 1234, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_nothing_after_close_paren_returns_none() {
    assert!(parse_proc_stat("(x)", 1, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_too_few_trailing_fields_returns_none() {
    // Only 3 fields after comm — far short of the 22 required to reach rss.
    assert!(parse_proc_stat("1234 (bash) S 1 1234", 1234, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_exactly_21_fields_is_still_too_few() {
    // fields.len() must be >= 22 to safely index rss at position 21. This
    // buffer has all fields through vsize (21 tokens) but is missing rss —
    // one field short of the required minimum.
    let buf = "1 (x) S 1 1234 1234 0 -1 4194304 100 0 5 0 1500 500 0 0 20 0 1 0 12345 135168";
    assert!(parse_proc_stat(buf, 1, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_non_numeric_utime_returns_none() {
    let buf = "1 (x) S 1 1 1 0 -1 0 0 0 0 0 NOTANUM 20 0 0 20 0 1 0 0 0 100";
    assert!(parse_proc_stat(buf, 1, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_non_numeric_rss_returns_none() {
    let buf = "1 (x) S 1 1 1 0 -1 0 0 0 0 0 10 20 0 0 20 0 1 0 0 0 NOTANUM";
    assert!(parse_proc_stat(buf, 1, PAGE_SIZE).is_none());
}

#[test]
fn test_parse_proc_stat_empty_input_returns_none() {
    assert!(parse_proc_stat("", 1, PAGE_SIZE).is_none());
}

// --- ProcessCollector.collect: real /proc smoke test ---
//
// collect() enumerates every numeric entry under the hardcoded "/proc" path,
// which is always present on Linux. This exercises the full top-N selection
// and CPU/RSS gauge emission against live process data without needing a
// fixture, and asserts the invariants the implementation promises.
#[test]
fn test_process_collector_respects_top_n_and_emits_valid_labels() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = ProcessCollector::new(5);
    let mut buf = String::new();

    // First tick seeds `last_collect` / `prev` state — cpu_percent needs a
    // nonzero elapsed duration, so give the second tick a little time to pass.
    collector.collect(&sink, "test-host", &mut buf);
    std::thread::sleep(Duration::from_millis(30));
    collector.collect(&sink, "test-host", &mut buf);

    let mut rss_count = 0;
    let mut cpu_count = 0;
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
        assert!(value.is_finite());
        assert!(value >= 0.0, "{name} must not be negative: {value}");

        let pid_label = labels
            .iter()
            .find(|(k, _)| k == "pid")
            .unwrap_or_else(|| panic!("{name} missing pid label"));
        assert!(
            pid_label.1.parse::<u32>().is_ok(),
            "pid label must be numeric: {}",
            pid_label.1
        );
        assert!(labels.iter().any(|(k, _)| k == "name"));

        match name {
            "system.process.memory_rss" => rss_count += 1,
            "system.process.cpu_percent" => cpu_count += 1,
            other => panic!("unexpected metric name: {other}"),
        }
    }

    assert!(rss_count <= 5, "top_n=5 must cap memory_rss emissions");
    assert!(cpu_count <= 5, "top_n=5 must cap cpu_percent emissions");
}

#[test]
fn test_process_collector_top_n_is_clamped_to_at_least_one() {
    // `top_n.max(1)` in `new()` — constructing with 0 must not panic and the
    // collector must still be usable.
    let mut collector = ProcessCollector::new(0);
    let sink = Sink::capture(Capture::new(), HashMap::new());
    let mut buf = String::new();
    collector.collect(&sink, "test-host", &mut buf);
}
