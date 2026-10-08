//! Load average collector — reads /proc/loadavg.
//!
//! Emits gauges: system.load.1, system.load.5, system.load.15, and
//! system.load.1_per_cpu = load.1 / online logical CPUs (counted from
//! /sys/devices/system/cpu/online; omitted when that cannot be read).

use crate::metrics::derived::{count_cpu_list, load_per_cpu};
use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

/// Kernel list of online logical CPUs, e.g. `0-7`.
const CPU_ONLINE_PATH: &str = "/sys/devices/system/cpu/online";

pub struct LoadCollector;

impl Collector for LoadCollector {
    fn name(&self) -> &'static str {
        "load"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        if read_procfs("/proc/loadavg", buf).is_err() {
            return;
        }

        let mut parts = buf.split_whitespace();
        let load1 = parts.next().and_then(|s| s.parse::<f64>().ok());

        if let Some(v) = load1 {
            sink.gauge("system.load.1", v, &[]);
        }
        if let Some(v) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
            sink.gauge("system.load.5", v, &[]);
        }
        if let Some(v) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
            sink.gauge("system.load.15", v, &[]);
        }

        let Some(load1) = load1 else { return };
        if let Some(v) = online_cpus(buf).and_then(|n| load_per_cpu(load1, n)) {
            sink.gauge("system.load.1_per_cpu", v, &[]);
        }
    }
}

/// Online logical CPU count, reusing `buf`; `None` when unreadable.
fn online_cpus(buf: &mut String) -> Option<usize> {
    read_procfs(CPU_ONLINE_PATH, buf).ok()?;
    count_cpu_list(buf)
}
