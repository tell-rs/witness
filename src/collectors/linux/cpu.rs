//! CPU collector — reads /proc/stat.
//!
//! Emits gauges (percentage, 0-100):
//! - system.cpu.user, .system, .idle, .iowait, .steal
//! Labels: {core: "total"} or {core: "0"}, {core: "1"}, ...
//! First tick stores baseline — no metrics emitted until second tick.

use std::collections::HashMap;

use crate::collectors::{Collector, read_procfs};
use crate::sink::Sink;

pub struct CpuCollector {
    prev: HashMap<String, CpuTimes>,
}

#[derive(Clone, Default)]
struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTimes {
    fn total(&self) -> u64 {
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

        for line in buf.lines() {
            if !line.starts_with("cpu") {
                continue;
            }

            let mut parts = line.split_whitespace();
            let Some(cpu_name) = parts.next() else {
                continue;
            };

            let is_total = cpu_name == "cpu";
            let is_core = cpu_name.len() > 3
                && cpu_name.as_bytes()[..3] == *b"cpu"
                && cpu_name[3..].bytes().all(|b| b.is_ascii_digit());

            if !is_total && !is_core {
                continue;
            }

            let current = parse_cpu_line(&mut parts);
            let label: &str = if is_total { "total" } else { &cpu_name[3..] };

            if let Some(prev_val) = self.prev.get_mut(cpu_name) {
                let dt = current.total().saturating_sub(prev_val.total());
                if dt > 0 {
                    let d = dt as f64;
                    let labels: &[(&'static str, &str)] = &[("core", label)];

                    let du =
                        (current.user + current.nice).saturating_sub(prev_val.user + prev_val.nice);
                    sink.gauge_dyn("system.cpu.user", du as f64 / d * 100.0, labels);

                    let ds = (current.system + current.irq + current.softirq)
                        .saturating_sub(prev_val.system + prev_val.irq + prev_val.softirq);
                    sink.gauge_dyn("system.cpu.system", ds as f64 / d * 100.0, labels);

                    sink.gauge_dyn(
                        "system.cpu.idle",
                        current.idle.saturating_sub(prev_val.idle) as f64 / d * 100.0,
                        labels,
                    );
                    sink.gauge_dyn(
                        "system.cpu.iowait",
                        current.iowait.saturating_sub(prev_val.iowait) as f64 / d * 100.0,
                        labels,
                    );
                    sink.gauge_dyn(
                        "system.cpu.steal",
                        current.steal.saturating_sub(prev_val.steal) as f64 / d * 100.0,
                        labels,
                    );
                }
                *prev_val = current;
            } else {
                self.prev.insert(cpu_name.to_string(), current);
            }
        }
    }
}

fn parse_cpu_line(parts: &mut std::str::SplitWhitespace<'_>) -> CpuTimes {
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
