//! Load average collector — reads /proc/loadavg.
//!
//! Emits gauges: system.load.1, system.load.5, system.load.15

use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

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

        if let Some(v) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
            sink.gauge("system.load.1", v, &[]);
        }
        if let Some(v) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
            sink.gauge("system.load.5", v, &[]);
        }
        if let Some(v) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
            sink.gauge("system.load.15", v, &[]);
        }
    }
}
