//! Metric/log sink — wraps Tell for live mode, prints to stderr for dry-run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

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
    /// Records every emission for value-level test assertions.
    #[cfg(test)]
    Capture(Capture),
}

/// Test sink that records `(kind, name, value, labels)` for every metric and
/// the message/service for every log, so tests can assert real output instead
/// of just "didn't panic".
#[cfg(test)]
#[derive(Clone, Default)]
pub struct Capture {
    events: Arc<std::sync::Mutex<Vec<Recorded>>>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Metric {
        kind: &'static str,
        name: &'static str,
        value: f64,
        labels: Vec<(String, String)>,
    },
    Log {
        message: String,
        service: Option<String>,
    },
}

#[cfg(test)]
impl Capture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<Recorded> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Values of every metric emitted under `name`, in emission order.
    pub fn metric_values(&self, name: &str) -> Vec<f64> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Recorded::Metric { name: n, value, .. } if n == name => Some(value),
                _ => None,
            })
            .collect()
    }

    fn record_metric(
        &self,
        kind: &'static str,
        name: &'static str,
        value: f64,
        tags: &[(&'static str, &'static str)],
        labels: &[(&'static str, &str)],
    ) {
        let labels = tags
            .iter()
            .map(|&(k, v)| (k.to_string(), v.to_string()))
            .chain(labels.iter().map(|&(k, v)| (k.to_string(), v.to_string())))
            .collect();
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Recorded::Metric {
                kind,
                name,
                value,
                labels,
            });
    }

    fn record_log(&self, message: &str, service: Option<&str>) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Recorded::Log {
                message: message.to_string(),
                service: service.map(str::to_string),
            });
    }
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

    /// Sink that records every emission for value-level test assertions.
    #[cfg(test)]
    pub fn capture(cap: Capture, tags: HashMap<String, String>) -> Self {
        Self {
            inner: SinkInner::Capture(cap),
            tags: leak_tags(tags),
        }
    }

    /// The merged global tag slice (test-only, for interning assertions).
    #[cfg(test)]
    pub fn tags_for_test(&self) -> &[(&'static str, &'static str)] {
        &self.tags
    }

    // --- Gauges ---

    pub fn gauge(&self, name: &'static str, value: f64, labels: &[(&'static str, &'static str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge(name, value, labels);
                } else {
                    with_merged(&self.tags, labels, |m| tell.gauge(name, value, m));
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
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_metric("gauge", name, value, &self.tags, labels),
        }
    }

    pub fn gauge_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.gauge_dyn(name, value, labels);
                } else {
                    with_merged(&self.tags, labels, |m| tell.gauge_dyn(name, value, m));
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
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_metric("gauge", name, value, &self.tags, labels),
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
                    with_merged(&self.tags, labels, |m| tell.counter(name, value, m));
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
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_metric("counter", name, value, &self.tags, labels),
        }
    }

    pub fn counter_dyn(&self, name: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        match &self.inner {
            SinkInner::Live(tell) => {
                if self.tags.is_empty() {
                    tell.counter_dyn(name, value, labels);
                } else {
                    with_merged(&self.tags, labels, |m| tell.counter_dyn(name, value, m));
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
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_metric("counter", name, value, &self.tags, labels),
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
                    with_merged(&self.tags, labels, |m| {
                        tell.counter_dyn_with_temporality(name, value, m, temporality)
                    });
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
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_metric("checkpoint", name, value, &self.tags, labels),
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
                dry_run_log(level, component.unwrap_or("-"), message, data);
            }
            SinkInner::Discard => {}
            #[cfg(test)]
            SinkInner::Full => {}
            #[cfg(test)]
            SinkInner::Capture(c) => c.record_log(message, None),
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
                dry_run_log(level, component.unwrap_or("-"), message, data);
                true
            }
            SinkInner::Discard => true,
            #[cfg(test)]
            SinkInner::Full => false,
            #[cfg(test)]
            SinkInner::Capture(c) => {
                c.record_log(message, None);
                true
            }
        }
    }

    /// Try to send a log entry with per-entry service override.
    pub fn try_log_with_service(
        &self,
        level: LogLevel,
        message: &str,
        component: Option<&str>,
        service: Option<&str>,
        data: impl tell::IntoPayload,
    ) -> bool {
        match &self.inner {
            SinkInner::Live(tell) => {
                tell.try_log_with_service(level, message, component, service, data)
            }
            SinkInner::DryRun(dr) => {
                dr.counter.fetch_add(1, Ordering::Relaxed);
                dry_run_log(level, service.unwrap_or("-"), message, data);
                true
            }
            SinkInner::Discard => true,
            #[cfg(test)]
            SinkInner::Full => false,
            #[cfg(test)]
            SinkInner::Capture(c) => {
                c.record_log(message, service);
                true
            }
        }
    }

    /// Fire-and-forget variant of [`try_log_with_service`].
    pub fn log_with_service(
        &self,
        level: LogLevel,
        message: &str,
        component: Option<&str>,
        service: Option<&str>,
        data: impl tell::IntoPayload,
    ) {
        let _ = self.try_log_with_service(level, message, component, service, data);
    }

    // --- Lifecycle ---

    #[allow(dead_code)] // Public API — used by library consumers and tests
    pub async fn flush(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.flush().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
            #[cfg(test)]
            SinkInner::Full | SinkInner::Capture(_) => Ok(()),
        }
    }

    pub async fn close(&self) -> Result<(), tell::TellError> {
        match &self.inner {
            SinkInner::Live(tell) => tell.close().await,
            SinkInner::DryRun(_) | SinkInner::Discard => Ok(()),
            #[cfg(test)]
            SinkInner::Full | SinkInner::Capture(_) => Ok(()),
        }
    }
}

// --- Tag helpers ---

/// Intern table for tag strings. The SDK label type requires `&'static str`,
/// so tag strings must be leaked — interning bounds total leakage to the set
/// of distinct strings ever seen, making repeated SIGHUP reloads free.
/// Cold path only: touched at Sink construction, never per metric.
static TAG_INTERN: LazyLock<Mutex<HashSet<&'static str>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn intern(s: String) -> &'static str {
    let mut set = TAG_INTERN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match set.get(s.as_str()) {
        Some(&existing) => existing,
        None => {
            let leaked: &'static str = Box::leak(s.into_boxed_str());
            set.insert(leaked);
            leaked
        }
    }
}

/// Convert config tags to `'static` label pairs via the intern table.
fn leak_tags(tags: HashMap<String, String>) -> Arc<[(&'static str, &'static str)]> {
    let mut pairs: Vec<(&'static str, &'static str)> = tags
        .into_iter()
        .map(|(k, v)| (intern(k), intern(v)))
        .collect();
    // Sort for deterministic label order
    pairs.sort_by_key(|&(k, _)| k);
    Arc::from(pairs)
}

/// Call `f` with global tags prepended to `labels`.
///
/// Hot path: uses a fixed stack buffer for typical label counts and only
/// falls back to a heap `Vec` beyond 16 total pairs.
fn with_merged<'a, R>(
    tags: &[(&'static str, &'static str)],
    labels: &[(&'static str, &'a str)],
    f: impl FnOnce(&[(&'static str, &'a str)]) -> R,
) -> R {
    const STACK: usize = 16;
    let total = tags.len() + labels.len();
    if total <= STACK {
        let mut buf: [(&'static str, &'a str); STACK] = [("", ""); STACK];
        buf[..tags.len()].copy_from_slice(tags);
        buf[tags.len()..total].copy_from_slice(labels);
        f(&buf[..total])
    } else {
        let mut v = Vec::with_capacity(total);
        v.extend_from_slice(tags);
        v.extend_from_slice(labels);
        f(&v)
    }
}

// --- Formatting ---

/// Render one log line for dry-run output, showing the resolved severity,
/// service/component tag, truncated message, and any structured payload. The
/// payload is materialized here (dry-run only) via [`tell::IntoPayload`] so
/// operators can confirm severity + structure before shipping for real.
fn dry_run_log(level: LogLevel, tag: &str, message: &str, data: impl tell::IntoPayload) {
    let lvl = format!("{level:?}");
    let msg = &message[..message.floor_char_boundary(120)];
    match data.into_payload() {
        Some(bytes) if bytes != b"{}" => {
            eprintln!(
                "  log     {lvl:<9} [{tag}] {msg}  {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        _ => eprintln!("  log     {lvl:<9} [{tag}] {msg}"),
    }
}

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
