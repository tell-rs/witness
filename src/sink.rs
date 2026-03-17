//! Metric/log sink — wraps Tell for live mode, prints to stderr for dry-run.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use tell::{LogLevel, Tell, Temporality};

/// Output sink for metrics and logs.
///
/// In live mode, delegates to the Tell SDK client.
/// In dry-run mode, prints human-readable output to stderr.
/// Global tags from config are merged into every metric's labels.
#[derive(Clone)]
pub struct Sink {
    inner: SinkInner,
    tags: &'static [(&'static str, &'static str)],
}

#[derive(Clone)]
enum SinkInner {
    Live(Tell),
    DryRun(DryRun),
    /// Silently discards all data. Used for the baseline tick in dry-run mode.
    Discard,
}

#[derive(Clone)]
pub struct DryRun {
    counter: &'static AtomicUsize,
}

static DRY_RUN_COUNT: AtomicUsize = AtomicUsize::new(0);

impl Default for DryRun {
    fn default() -> Self {
        Self::new()
    }
}

impl DryRun {
    pub fn new() -> Self {
        DRY_RUN_COUNT.store(0, Ordering::Relaxed);
        Self {
            counter: &DRY_RUN_COUNT,
        }
    }

    pub fn count(&self) -> usize {
        self.counter.load(Ordering::Relaxed)
    }
}

impl Sink {
    pub fn live(tell: Tell, tags: HashMap<String, String>) -> Self {
        Self {
            inner: SinkInner::Live(tell),
            tags: leak_tags(tags),
        }
    }

    pub fn dry_run(dr: DryRun, tags: HashMap<String, String>) -> Self {
        Self {
            inner: SinkInner::DryRun(dr),
            tags: leak_tags(tags),
        }
    }

    pub fn discard() -> Self {
        Self {
            inner: SinkInner::Discard,
            tags: &[],
        }
    }

    // --- Gauges ---

    pub fn gauge(&self, name: &'static str, value: f64, labels: &[(&'static str, &'static str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge(name, value, labels);
                } else {
                    let m = merge_static(self.tags, labels);
                    tell.gauge(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  gauge   {}{} = {}",
                    name,
                    fmt_labels(self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
        }
    }

    pub fn gauge_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge_dyn(name, value, labels);
                } else {
                    let m = merge_dyn(self.tags, labels);
                    tell.gauge_dyn(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  gauge   {}{} = {}",
                    name,
                    fmt_labels(self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
        }
    }

    // --- Counters ---

    pub fn counter(&self, name: &'static str, value: f64, labels: &[(&'static str, &'static str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter(name, value, labels);
                } else {
                    let m = merge_static(self.tags, labels);
                    tell.counter(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  counter {}{} = {}",
                    name,
                    fmt_labels(self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
        }
    }

    pub fn counter_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter_dyn(name, value, labels);
                } else {
                    let m = merge_dyn(self.tags, labels);
                    tell.counter_dyn(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  counter {}{} = {}",
                    name,
                    fmt_labels(self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
        }
    }

    pub fn counter_dyn_with_temporality(
        &self,
        name: &'static str,
        value: f64,
        labels: &[(&'static str, &str)],
        temporality: Temporality,
    ) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter_dyn_with_temporality(name, value, labels, temporality);
                } else {
                    let m = merge_dyn(self.tags, labels);
                    tell.counter_dyn_with_temporality(name, value, &m, temporality);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  checkpoint {}{} = {}",
                    name,
                    fmt_labels(self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
        }
    }

    // --- Logs ---

    pub fn log(
        &self,
        level: LogLevel,
        message: &str,
        component: Option<&str>,
        data: impl tell::IntoPayload,
    ) {
        match &self.inner {
            SinkInner::Live(tell) => tell.log(level, message, component, data),
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                let comp = component.unwrap_or("-");
                let msg = if message.len() > 120 {
                    &message[..120]
                } else {
                    message
                };
                eprintln!("  log     [{comp}] {msg}");
            }
            SinkInner::Discard => {}
        }
    }

    // --- Lifecycle ---

    pub async fn flush(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.flush().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
        }
    }

    pub async fn close(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.close().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
        }
    }
}

// --- Tag helpers ---

fn leak_tags(tags: HashMap<String, String>) -> &'static [(&'static str, &'static str)] {
    if tags.is_empty() {
        return &[];
    }
    let mut pairs: Vec<(&'static str, &'static str)> = Vec::with_capacity(tags.len());
    for (k, v) in tags {
        let k: &'static str = Box::leak(k.into_boxed_str());
        let v: &'static str = Box::leak(v.into_boxed_str());
        pairs.push((k, v));
    }
    // Sort for deterministic label order
    pairs.sort_by_key(|&(k, _)| k);
    Box::leak(pairs.into_boxed_slice())
}

fn merge_static(
    tags: &[(&'static str, &'static str)],
    labels: &[(&'static str, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    let mut m = Vec::with_capacity(tags.len() + labels.len());
    m.extend_from_slice(tags);
    m.extend_from_slice(labels);
    m
}

fn merge_dyn<'a>(
    tags: &[(&'static str, &'static str)],
    labels: &[(&'static str, &'a str)],
) -> Vec<(&'static str, &'a str)> {
    let mut m = Vec::with_capacity(tags.len() + labels.len());
    for &(k, v) in tags {
        m.push((k, v));
    }
    m.extend_from_slice(labels);
    m
}

// --- Formatting ---

fn fmt_value(v: f64) -> String {
    if v.abs() >= 1_000_000_000.0 {
        format!("{:.2}G", v / 1_000_000_000.0)
    } else if v.abs() >= 1_000_000.0 {
        format!("{:.2}M", v / 1_000_000.0)
    } else if v.abs() >= 10_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else if v == v.floor() {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

fn fmt_labels(tags: &[(&str, &str)], labels: &[(&str, &str)]) -> String {
    if tags.is_empty() && labels.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = tags
        .iter()
        .chain(labels.iter())
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    format!("{{{}}}", inner.join(","))
}
