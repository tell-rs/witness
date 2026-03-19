//! Metric/log sink — wraps Tell for live mode, prints to stderr for dry-run.

use std::collections::HashMap;
use std::sync::Arc;
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
    tags: Arc<[(&'static str, &'static str)]>,
}

#[derive(Clone)]
enum SinkInner {
    Live(Tell),
    DryRun(DryRun),
    /// Silently discards all data. Used for the baseline tick in dry-run mode.
    Discard,
    /// Simulates a full SDK channel. `try_log` always returns false.
    #[cfg(test)]
    Full,
}

#[derive(Clone)]
pub struct DryRun {
    counter: Arc<AtomicUsize>,
}

impl Default for DryRun {
    fn default() -> Self {
        Self::new()
    }
}

impl DryRun {
    pub fn new() -> Self {
        Self {
            counter: Arc::new(AtomicUsize::new(0)),
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
            tags: leak_tags(HashMap::new()),
        }
    }

    /// Sink that always reports backpressure (`try_log` returns false).
    #[cfg(test)]
    pub fn full() -> Self {
        Self {
            inner: SinkInner::Full,
            tags: leak_tags(HashMap::new()),
        }
    }

    // --- Gauges ---

    pub fn gauge(&self, name: &'static str, value: f64, labels: &[(&'static str, &'static str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge(name, value, labels);
                } else {
                    let m = merge_static(&self.tags, labels);
                    tell.gauge(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  gauge   {}{} = {}",
                    name,
                    fmt_labels(&self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
        }
    }

    pub fn gauge_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge_dyn(name, value, labels);
                } else {
                    let m = merge_dyn(&self.tags, labels);
                    tell.gauge_dyn(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  gauge   {}{} = {}",
                    name,
                    fmt_labels(&self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
        }
    }

    // --- Counters ---

    #[allow(dead_code)] // Used by Linux cgroups collector, not compiled on macOS
    pub fn counter(&self, name: &'static str, value: f64, labels: &[(&'static str, &'static str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter(name, value, labels);
                } else {
                    let m = merge_static(&self.tags, labels);
                    tell.counter(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  counter {}{} = {}",
                    name,
                    fmt_labels(&self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
        }
    }

    pub fn counter_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter_dyn(name, value, labels);
                } else {
                    let m = merge_dyn(&self.tags, labels);
                    tell.counter_dyn(name, value, &m);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  counter {}{} = {}",
                    name,
                    fmt_labels(&self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
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
                    let m = merge_dyn(&self.tags, labels);
                    tell.counter_dyn_with_temporality(name, value, &m, temporality);
                }
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "  checkpoint {}{} = {}",
                    name,
                    fmt_labels(&self.tags, labels),
                    fmt_value(value)
                );
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
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
            #[cfg(test)]
            SinkInner::Full => {}
        }
    }

    /// Try to send a log entry, returning `false` if the SDK channel is full.
    pub fn try_log(
        &self,
        level: LogLevel,
        message: &str,
        component: Option<&str>,
        data: impl tell::IntoPayload,
    ) -> bool {
        match &self.inner {
            SinkInner::Live(tell) => tell.try_log(level, message, component, data),
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                let comp = component.unwrap_or("-");
                let msg = if message.len() > 120 {
                    &message[..120]
                } else {
                    message
                };
                eprintln!("  log     [{comp}] {msg}");
                true
            }
            SinkInner::Discard => true,
            #[cfg(test)]
            SinkInner::Full => false,
        }
    }

    // --- Lifecycle ---

    #[allow(dead_code)] // Public API — used by library consumers and tests
    pub async fn flush(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.flush().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
            #[cfg(test)]
            SinkInner::Full => Ok(()),
        }
    }

    pub async fn close(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.close().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
            #[cfg(test)]
            SinkInner::Full => Ok(()),
        }
    }
}

// --- Tag helpers ---

/// Leak tag key/value strings to `'static` lifetime. The individual strings
/// are leaked via `Box::leak` (bounded by config size, a few hundred bytes).
/// The returned `Arc` is freed when all `Sink` clones are dropped, so SIGHUP
/// reload reclaims the slice memory.
fn leak_tags(tags: HashMap<String, String>) -> Arc<[(&'static str, &'static str)]> {
    let mut pairs: Vec<(&'static str, &'static str)> = Vec::with_capacity(tags.len());
    for (k, v) in tags {
        let k: &'static str = Box::leak(k.into_boxed_str());
        let v: &'static str = Box::leak(v.into_boxed_str());
        pairs.push((k, v));
    }
    // Sort for deterministic label order
    pairs.sort_by_key(|&(k, _)| k);
    Arc::from(pairs)
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
