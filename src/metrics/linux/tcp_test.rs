use std::collections::HashMap;

use super::tcp::*;
use crate::metrics::Collector;
use crate::sink::{Capture, Sink};

const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

fn line(state_hex: &str) -> String {
    format!(
        "   0: 0100007F:0277 00000000:0000 {state_hex} 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0"
    )
}

#[test]
fn test_count_states_established_connection() {
    let buf = format!("{HEADER}\n{}\n", line("01"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts[0x01], 1);
    assert_eq!(counts.iter().sum::<u32>(), 1);
}

#[test]
fn test_count_states_listen_connection() {
    let buf = format!("{HEADER}\n{}\n", line("0A"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts[0x0A], 1);
}

#[test]
fn test_count_states_multiple_of_same_state_accumulate() {
    let buf = format!("{HEADER}\n{}\n{}\n{}\n", line("01"), line("01"), line("01"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts[0x01], 3);
}

#[test]
fn test_count_states_mixed_states() {
    let buf = format!(
        "{HEADER}\n{}\n{}\n{}\n{}\n",
        line("01"), // established
        line("0A"), // listen
        line("06"), // time_wait
        line("0A"), // listen
    );
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts[0x01], 1);
    assert_eq!(counts[0x0A], 2);
    assert_eq!(counts[0x06], 1);
    assert_eq!(counts.iter().sum::<u32>(), 4);
}

#[test]
fn test_count_states_out_of_range_state_is_ignored() {
    // 0x0C (12) parses fine as hex but is >= counts.len() (12) — must not
    // panic on out-of-bounds and must not be counted anywhere.
    let buf = format!("{HEADER}\n{}\n", line("0C"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_far_out_of_range_state_is_ignored() {
    let buf = format!("{HEADER}\n{}\n", line("FF"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_non_hex_state_skipped() {
    let buf = format!("{HEADER}\n{}\n", line("ZZ"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_malformed_line_missing_state_field() {
    let malformed = "   0: 0100007F:0277 00000000:0000";
    let buf = format!("{HEADER}\n{malformed}\n");
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_empty_body_only_header() {
    let mut counts = [0u32; 12];
    count_states(HEADER, &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_completely_empty_input() {
    let mut counts = [0u32; 12];
    count_states("", &mut counts);
    assert_eq!(counts.iter().sum::<u32>(), 0);
}

#[test]
fn test_count_states_accumulates_across_calls_like_tcp_and_tcp6() {
    // The real collector calls count_states once for /proc/net/tcp and once
    // for /proc/net/tcp6 against the same counts array.
    let tcp_buf = format!("{HEADER}\n{}\n", line("01"));
    let tcp6_buf = format!("{HEADER}\n{}\n{}\n", line("01"), line("0A"));

    let mut counts = [0u32; 12];
    count_states(&tcp_buf, &mut counts);
    count_states(&tcp6_buf, &mut counts);

    assert_eq!(counts[0x01], 2);
    assert_eq!(counts[0x0A], 1);
}

#[test]
fn test_count_states_first_line_always_skipped_as_header() {
    // Even a bogus header line is skipped unconditionally (`.skip(1)`), so a
    // data-shaped first line never gets counted.
    let buf = format!("{}\n{}\n", line("01"), line("01"));
    let mut counts = [0u32; 12];
    count_states(&buf, &mut counts);
    // Only the second line is counted since the first is treated as header.
    assert_eq!(counts[0x01], 1);
}

// --- TcpCollector.collect: exercised against real host files when present ---
//
// collect() hardcodes "/proc/net/tcp" and "/proc/net/tcp6" so we exercise the
// real files on the host these tests run on. Both are guaranteed world-
// readable on any Linux system; if somehow unreadable, `read_procfs` fails
// silently and no metrics are emitted — either way this must never panic.
#[test]
fn test_tcp_collector_collect_emits_only_known_states_with_positive_counts() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());
    let mut collector = TcpCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test-host", &mut buf);

    for value in cap.metric_values("system.tcp.connections") {
        assert!(value > 0.0, "zero-count states should never be emitted");
        assert!(value.is_finite());
    }
}
