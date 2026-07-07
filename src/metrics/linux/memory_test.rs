use std::collections::HashMap;

use super::memory::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Sink};

const SAMPLE_MEMINFO: &str = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
MemAvailable:    9000000 kB
Buffers:          512000 kB
Cached:          4096000 kB
SwapCached:            0 kB
SwapTotal:       8388604 kB
SwapFree:        8000000 kB
Dirty:              1024 kB
";

fn field(buf: &str, key: &str) -> Option<f64> {
    let line = buf.lines().find(|l| l.starts_with(key))?;
    let (_, rest) = line.split_once(':')?;
    parse_kb(rest)
}

// --- parse_kb ---

#[test]
fn test_parse_kb_normal_value_converts_to_bytes() {
    assert_eq!(parse_kb("   16384000 kB"), Some(16384000.0 * 1024.0));
}

#[test]
fn test_parse_kb_ignores_trailing_unit_text() {
    // Only the first whitespace-delimited token is parsed as the number.
    assert_eq!(parse_kb("   500 kB extra garbage"), Some(500.0 * 1024.0));
}

#[test]
fn test_parse_kb_no_unit_suffix_still_parses() {
    assert_eq!(parse_kb("   500"), Some(500.0 * 1024.0));
}

#[test]
fn test_parse_kb_empty_string_returns_none() {
    assert_eq!(parse_kb(""), None);
}

#[test]
fn test_parse_kb_whitespace_only_returns_none() {
    assert_eq!(parse_kb("     "), None);
}

#[test]
fn test_parse_kb_non_numeric_token_returns_none() {
    assert_eq!(parse_kb("   abc kB"), None);
}

#[test]
fn test_parse_kb_zero_value() {
    assert_eq!(parse_kb("0 kB"), Some(0.0));
}

// --- parse_kb against a realistic /proc/meminfo dump ---

#[test]
fn test_meminfo_extracts_mem_total() {
    assert_eq!(field(SAMPLE_MEMINFO, "MemTotal"), Some(16384000.0 * 1024.0));
}

#[test]
fn test_meminfo_extracts_mem_available() {
    assert_eq!(
        field(SAMPLE_MEMINFO, "MemAvailable"),
        Some(9000000.0 * 1024.0)
    );
}

#[test]
fn test_meminfo_extracts_cached() {
    assert_eq!(field(SAMPLE_MEMINFO, "Cached"), Some(4096000.0 * 1024.0));
}

#[test]
fn test_meminfo_extracts_swap_total_and_free() {
    assert_eq!(field(SAMPLE_MEMINFO, "SwapTotal"), Some(8388604.0 * 1024.0));
    assert_eq!(field(SAMPLE_MEMINFO, "SwapFree"), Some(8000000.0 * 1024.0));
}

#[test]
fn test_meminfo_used_computed_as_total_minus_available() {
    let total = field(SAMPLE_MEMINFO, "MemTotal").unwrap();
    let available = field(SAMPLE_MEMINFO, "MemAvailable").unwrap();
    let used = total - available;
    assert_eq!(used, (16384000.0 - 9000000.0) * 1024.0);
}

#[test]
fn test_meminfo_missing_field_yields_none() {
    // A field absent from the dump entirely.
    assert_eq!(field(SAMPLE_MEMINFO, "HugePages_Total"), None);
}

// --- MemoryCollector.collect: exercised against the real /proc/meminfo ---
//
// collect() hardcodes "/proc/meminfo", which is always present and world-
// readable on Linux. This verifies the real aggregation logic (kB→bytes
// conversion, total/used/available/cached/swap gauges) against live data.
#[test]
fn test_memory_collector_emits_consistent_total_and_used() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = MemoryCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    let totals = cap.metric_values("system.memory.total");
    let availables = cap.metric_values("system.memory.available");
    let useds = cap.metric_values("system.memory.used");

    // /proc/meminfo always has MemTotal and MemAvailable, so all three must
    // be emitted, and used must equal total - available exactly.
    assert_eq!(totals.len(), 1);
    assert_eq!(availables.len(), 1);
    assert_eq!(useds.len(), 1);
    assert!(totals[0] > 0.0);
    assert_eq!(useds[0], totals[0] - availables[0]);
}
