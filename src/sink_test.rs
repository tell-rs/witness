use std::collections::HashMap;

use crate::sink::{Capture, DryRun, Recorded, Sink};
use tell::{LogLevel, Temporality};

fn dry_sink() -> Sink {
    Sink::dry_run(DryRun::new(), HashMap::new())
}

fn dry_sink_with_tags() -> Sink {
    let mut tags = HashMap::new();
    tags.insert("env".to_string(), "production".to_string());
    tags.insert("region".to_string(), "us-west-2".to_string());
    Sink::dry_run(DryRun::new(), tags)
}

fn no_payload() -> Option<String> {
    None
}

// --- Gauge ---

#[test]
fn gauge_no_labels() {
    dry_sink().gauge("test.metric", 42.0, &[]);
}

#[test]
fn gauge_with_labels() {
    dry_sink().gauge("test.cpu", 75.5, &[("host", "web-01"), ("core", "0")]);
}

#[test]
fn gauge_dyn() {
    let device = "en0".to_string();
    dry_sink().gauge_dyn("test.net", 1000.0, &[("iface", device.as_str())]);
}

// --- Counter ---

#[test]
fn counter_no_labels() {
    dry_sink().counter("test.bytes", 1024.0, &[]);
}

#[test]
fn counter_with_labels() {
    dry_sink().counter("test.packets", 100.0, &[("dir", "rx")]);
}

#[test]
fn counter_dyn() {
    let host = "db-01".to_string();
    dry_sink().counter_dyn("test.ops", 55.0, &[("host", host.as_str())]);
}

// --- Log ---

#[test]
fn log_with_component() {
    dry_sink().log(LogLevel::Info, "hello world", Some("test"), no_payload());
}

#[test]
fn log_without_component() {
    dry_sink().log(LogLevel::Error, "something broke", None, no_payload());
}

#[test]
fn log_long_message_truncated() {
    let long_msg = "x".repeat(200);
    dry_sink().log(LogLevel::Warning, &long_msg, Some("long"), no_payload());
}

// --- fmt_value branches ---

#[test]
fn gauge_value_billions() {
    dry_sink().gauge("test.big", 5_000_000_000.0, &[]);
}

#[test]
fn gauge_value_millions() {
    dry_sink().gauge("test.mid", 2_500_000.0, &[]);
}

#[test]
fn gauge_value_thousands() {
    dry_sink().gauge("test.k", 15_000.0, &[]);
}

#[test]
fn gauge_value_integer() {
    dry_sink().gauge("test.int", 7.0, &[]);
}

#[test]
fn gauge_value_fractional() {
    dry_sink().gauge("test.frac", 2.78, &[]);
}

// --- Mixed calls ---

#[test]
fn mixed_calls() {
    let sink = dry_sink();
    sink.gauge("a", 1.0, &[]);
    sink.counter("b", 2.0, &[]);
    sink.log(LogLevel::Info, "c", None, no_payload());
    sink.gauge_dyn("d", 3.0, &[]);
    sink.counter_dyn("e", 4.0, &[]);
}

// --- Flush / close ---

#[tokio::test]
async fn flush_dry_run_succeeds() {
    assert!(dry_sink().flush().await.is_ok());
}

#[tokio::test]
async fn close_dry_run_succeeds() {
    assert!(dry_sink().close().await.is_ok());
}

// --- Discard variant ---

#[test]
fn discard_gauge() {
    Sink::discard().gauge("x", 1.0, &[]);
}

#[test]
fn discard_gauge_dyn() {
    Sink::discard().gauge_dyn("x", 1.0, &[("k", "v")]);
}

#[test]
fn discard_counter() {
    Sink::discard().counter("x", 1.0, &[]);
}

#[test]
fn discard_counter_dyn() {
    Sink::discard().counter_dyn("x", 1.0, &[("k", "v")]);
}

#[test]
fn discard_log() {
    Sink::discard().log(LogLevel::Info, "msg", Some("c"), no_payload());
}

#[tokio::test]
async fn discard_flush() {
    assert!(Sink::discard().flush().await.is_ok());
}

#[tokio::test]
async fn discard_close() {
    assert!(Sink::discard().close().await.is_ok());
}

// --- Global tags ---

#[test]
fn gauge_with_tags_no_labels() {
    dry_sink_with_tags().gauge("test.cpu", 50.0, &[]);
}

#[test]
fn gauge_with_tags_and_labels() {
    dry_sink_with_tags().gauge("test.cpu", 50.0, &[("core", "0")]);
}

#[test]
fn gauge_dyn_with_tags() {
    let core = "1".to_string();
    dry_sink_with_tags().gauge_dyn("test.cpu", 50.0, &[("core", core.as_str())]);
}

#[test]
fn counter_with_tags() {
    dry_sink_with_tags().counter("test.bytes", 1024.0, &[("device", "sda")]);
}

#[test]
fn counter_dyn_with_tags() {
    let iface = "eth0".to_string();
    dry_sink_with_tags().counter_dyn("test.net", 500.0, &[("iface", iface.as_str())]);
}

// --- Checkpoint (counter_dyn_with_temporality) ---

#[test]
fn checkpoint_counter_dyn_with_temporality() {
    let device = "sda".to_string();
    dry_sink().counter_dyn_with_temporality(
        "system.disk.read_bytes",
        1_000_000.0,
        &[("device", device.as_str())],
        Temporality::Cumulative,
    );
}

#[test]
fn checkpoint_counter_dyn_with_temporality_and_tags() {
    let iface = "en0".to_string();
    dry_sink_with_tags().counter_dyn_with_temporality(
        "system.net.bytes_recv",
        5_000_000.0,
        &[("interface", iface.as_str())],
        Temporality::Cumulative,
    );
}

#[test]
fn discard_counter_dyn_with_temporality() {
    Sink::discard().counter_dyn_with_temporality(
        "system.net.bytes_recv",
        1.0,
        &[("interface", "lo")],
        Temporality::Cumulative,
    );
}

// --- Capture sink ---

#[test]
fn test_capture_records_metric_with_merged_tags() {
    let cap = Capture::new();
    let mut tags = HashMap::new();
    tags.insert("env".to_string(), "prod".to_string());
    let sink = Sink::capture(cap.clone(), tags);

    sink.gauge("test.metric", 42.0, &[("core", "0")]);

    let events = cap.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        Recorded::Metric {
            kind,
            name,
            value,
            labels,
        } => {
            assert_eq!(*kind, "gauge");
            assert_eq!(*name, "test.metric");
            assert_eq!(*value, 42.0);
            assert!(labels.contains(&("env".to_string(), "prod".to_string())));
            assert!(labels.contains(&("core".to_string(), "0".to_string())));
        }
        other => panic!("expected metric, got {other:?}"),
    }
}

#[test]
fn test_capture_metric_values_by_name() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());

    sink.counter("net.bytes", 100.0, &[]);
    sink.counter("net.bytes", 250.0, &[]);
    sink.gauge("cpu.idle", 90.0, &[]);

    assert_eq!(cap.metric_values("net.bytes"), vec![100.0, 250.0]);
    assert_eq!(cap.metric_values("cpu.idle"), vec![90.0]);
    assert!(cap.metric_values("missing").is_empty());
}

#[test]
fn test_capture_records_log_service() {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), HashMap::new());

    assert!(sink.try_log_with_service(
        tell::LogLevel::Info,
        "hello",
        None,
        Some("sshd"),
        None::<()>,
    ));

    assert_eq!(
        cap.events(),
        vec![Recorded::Log {
            message: "hello".to_string(),
            service: Some("sshd".to_string()),
        }]
    );
}

// --- Tag interning ---

#[test]
fn test_leak_tags_interned_across_reloads() {
    let mut tags = HashMap::new();
    tags.insert("intern_test_key".to_string(), "intern_test_val".to_string());

    let a = Sink::dry_run(DryRun::new(), tags.clone());
    let b = Sink::dry_run(DryRun::new(), tags);

    // Same distinct strings → same leaked pointers, no growth per reload.
    let (ka, va) = a.tags_for_test()[0];
    let (kb, vb) = b.tags_for_test()[0];
    assert!(std::ptr::eq(ka, kb));
    assert!(std::ptr::eq(va, vb));
}
