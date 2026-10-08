//! CPU collector — reads /proc/stat.
//!
//! Emits gauges (percentage, 0-100):
//! - system.cpu.user, .system, .idle, .iowait, .steal
//!   Labels: {core: "total"} or {core: "0"}, {core: "1"}, ...
//! - system.cpu.busy_percent = 100 - idle, all CPUs only ({core: "total"});
//!   iowait and steal count as busy.
//!
//! And an unlabeled gauge: system.cpu.count (online logical CPUs, the
//! number of cpuN rows in /proc/stat).
//!
//! First tick stores baseline — no metrics emitted until second tick.

use std::collections::HashMap;

use crate::metrics::derived::cpu_busy_percent;
use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

pub struct CpuCollector {
    prev: HashMap<String, CpuTimes>,
}

#[derive(Clone, Default)]
pub(crate) struct CpuTimes {
    pub(crate) user: u64,
    pub(crate) nice: u64,
    pub(crate) system: u64,
    pub(crate) idle: u64,
    pub(crate) iowait: u64,
    pub(crate) irq: u64,
    pub(crate) softirq: u64,
    pub(crate) steal: u64,
}

impl CpuTimes {
    /// Kernel time: system + irq + softirq (reported as system.cpu.system).
    fn kernel(&self) -> u64 {
        self.system + self.irq + self.softirq
    }

    pub(crate) fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
}

impl CpuCollector {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
        }
    }
}

impl Collector for CpuCollector {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        if read_procfs("/proc/stat", buf).is_err() {
            return;
        }

        let mut cores = 0usize;
        let mut had_baseline = false;
        for line in buf.lines() {
            let mut parts = line.split_whitespace();
            let Some(cpu_name) = parts.next() else {
                continue;
            };
            let Some(label) = cpu_label(cpu_name) else {
                continue;
            };
            if label != "total" {
                cores += 1;
            }

            let current = parse_cpu_line(&mut parts);
            if let Some(prev_val) = self.prev.get_mut(cpu_name) {
                emit_deltas(sink, label, prev_val, &current);
                *prev_val = current;
                had_baseline = true;
            } else {
                self.prev.insert(cpu_name.to_string(), current);
            }
        }

        // Same first-tick rule as the percentages: nothing until a baseline.
        if had_baseline && cores > 0 {
            sink.gauge("system.cpu.count", cores as f64, &[]);
        }
    }
}

/// Map a /proc/stat row name to its `core` label: `cpu` → `total`,
/// `cpuN` → `N`. Any other row (intr, ctxt, ...) yields `None`.
fn cpu_label(cpu_name: &str) -> Option<&str> {
    let suffix = cpu_name.strip_prefix("cpu")?;
    if suffix.is_empty() {
        return Some("total");
    }
    suffix.bytes().all(|b| b.is_ascii_digit()).then_some(suffix)
}

/// Emit the interval percentages for one CPU row from two samples.
///
/// `system.cpu.busy_percent` (100 - idle) is emitted only for the
/// all-CPU row (`core="total"`), keeping one series per host.
fn emit_deltas(sink: &Sink, label: &str, prev: &CpuTimes, current: &CpuTimes) {
    let dt = current.total().saturating_sub(prev.total());
    if dt == 0 {
        return;
    }
    let d = dt as f64;
    let labels: &[(&'static str, &str)] = &[("core", label)];
    let pct = |cur: u64, old: u64| cur.saturating_sub(old) as f64 / d * 100.0;

    let idle = pct(current.idle, prev.idle);
    let user = pct(current.user + current.nice, prev.user + prev.nice);
    let system = pct(current.kernel(), prev.kernel());
    sink.gauge_dyn("system.cpu.user", user, labels);
    sink.gauge_dyn("system.cpu.system", system, labels);
    sink.gauge_dyn("system.cpu.idle", idle, labels);
    let iowait = pct(current.iowait, prev.iowait);
    sink.gauge_dyn("system.cpu.iowait", iowait, labels);
    let steal = pct(current.steal, prev.steal);
    sink.gauge_dyn("system.cpu.steal", steal, labels);

    if label == "total" {
        if let Some(busy) = cpu_busy_percent(idle) {
            sink.gauge_dyn("system.cpu.busy_percent", busy, labels);
        }
    }
}

pub(crate) fn parse_cpu_line(parts: &mut std::str::SplitWhitespace<'_>) -> CpuTimes {
    let p = |parts: &mut std::str::SplitWhitespace<'_>| -> u64 {
        parts.next().and_then(|s| s.parse().ok()).unwrap_or(0)
    };
    CpuTimes {
        user: p(parts),
        nice: p(parts),
        system: p(parts),
        idle: p(parts),
        iowait: p(parts),
        irq: p(parts),
        softirq: p(parts),
        steal: p(parts),
    }
}
