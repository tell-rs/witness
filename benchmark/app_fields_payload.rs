//! Criterion benchmark for `app_fields_payload`.
//!
//! Measures the per-journal-entry isolated cost of the extras-field helper
//! added when witness learned to forward structured journal fields as Tell
//! log properties: filter `_*` fields, lowercase keys, build a
//! `serde_json::Value::Object`.
//!
//! Not end-to-end — just this one function. For pipeline throughput use
//! `benchmark/bench_throughput.rs`.
#![allow(clippy::missing_docs_in_private_items)]

use std::collections::HashMap;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::Value;

use witness::logs::journal::app_fields_payload;

// ---------------------------------------------------------------------------
// Entry shapes, matching journal entries the real pipeline sees:
// - empty: no app fields, only systemd-trusted (filtered)
// - typical: a fail2ban-rs ban event (5 app fields + some systemd noise)
// - heavy: maxmind-enriched ban (9 app fields + systemd noise)
// ---------------------------------------------------------------------------

fn extras_empty() -> HashMap<String, Value> {
    [
        ("_PID".to_string(), Value::String("123".to_string())),
        (
            "_SYSTEMD_UNIT".to_string(),
            Value::String("foo.service".to_string()),
        ),
        ("_HOSTNAME".to_string(), Value::String("host".to_string())),
        (
            "__REALTIME_TIMESTAMP".to_string(),
            Value::String("1712000000".to_string()),
        ),
    ]
    .into_iter()
    .collect()
}

fn extras_typical() -> HashMap<String, Value> {
    [
        ("IP".to_string(), Value::String("1.2.3.4".to_string())),
        ("JAIL".to_string(), Value::String("sshd".to_string())),
        ("BAN_TIME".to_string(), Value::String("3600".to_string())),
        ("BAN_COUNT".to_string(), Value::String("1".to_string())),
        ("REASON".to_string(), Value::String("threshold".to_string())),
        ("_PID".to_string(), Value::String("42".to_string())),
        (
            "_SYSTEMD_UNIT".to_string(),
            Value::String("fail2ban-rs.service".to_string()),
        ),
    ]
    .into_iter()
    .collect()
}

fn extras_heavy() -> HashMap<String, Value> {
    [
        ("IP".to_string(), Value::String("1.2.3.4".to_string())),
        ("JAIL".to_string(), Value::String("sshd".to_string())),
        ("BAN_TIME".to_string(), Value::String("3600".to_string())),
        ("BAN_COUNT".to_string(), Value::String("3".to_string())),
        ("REASON".to_string(), Value::String("threshold".to_string())),
        (
            "MAXMIND_ASN".to_string(),
            Value::String("AS15169 Google LLC".to_string()),
        ),
        (
            "MAXMIND_COUNTRY".to_string(),
            Value::String("US".to_string()),
        ),
        (
            "MAXMIND_CITY".to_string(),
            Value::String("Mountain View".to_string()),
        ),
        ("PHASE".to_string(), Value::String("startup".to_string())),
        ("_PID".to_string(), Value::String("42".to_string())),
        (
            "_SYSTEMD_UNIT".to_string(),
            Value::String("fail2ban-rs.service".to_string()),
        ),
        ("_HOSTNAME".to_string(), Value::String("api".to_string())),
    ]
    .into_iter()
    .collect()
}

// ---------------------------------------------------------------------------
// Benches — each clones the HashMap per iteration because the function
// takes it by value. The clone is part of the real cost too: in the live
// pipeline, a fresh HashMap is built per journal entry.
// ---------------------------------------------------------------------------

fn bench_app_fields(c: &mut Criterion) {
    let mut g = c.benchmark_group("app_fields_payload");

    let empty = extras_empty();
    g.bench_function("empty_only_systemd", |b| {
        b.iter(|| app_fields_payload(black_box(empty.clone())));
    });

    let typical = extras_typical();
    g.bench_function("typical_5_app_fields", |b| {
        b.iter(|| app_fields_payload(black_box(typical.clone())));
    });

    let heavy = extras_heavy();
    g.bench_function("heavy_9_app_fields", |b| {
        b.iter(|| app_fields_payload(black_box(heavy.clone())));
    });

    g.finish();
}

criterion_group!(benches, bench_app_fields);
criterion_main!(benches);
