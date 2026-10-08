//! Memory collector — reads /proc/meminfo.
//!
//! Emits gauges (bytes): system.memory.total, .available, .used, .cached, .swap_used
//! and gauges (percentage, 0-100): system.memory.used_percent, and
//! system.memory.swap_used_percent when the host has swap (SwapTotal > 0).
//! See `metrics::derived` for the exact definitions.

use crate::metrics::derived::{memory_used_percent, swap_used_percent};
use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

pub struct MemoryCollector;

impl Collector for MemoryCollector {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        if read_procfs("/proc/meminfo", buf).is_err() {
            return;
        }

        let mut mem_total: Option<f64> = None;
        let mut mem_available: Option<f64> = None;
        let mut cached: Option<f64> = None;
        let mut swap_total: Option<f64> = None;
        let mut swap_free: Option<f64> = None;

        for line in buf.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            let kb = parse_kb(rest);
            match key {
                "MemTotal" => mem_total = kb,
                "MemAvailable" => mem_available = kb,
                "Cached" => cached = kb,
                "SwapTotal" => swap_total = kb,
                "SwapFree" => swap_free = kb,
                _ => {}
            }
        }

        if let Some(total) = mem_total {
            sink.gauge("system.memory.total", total, &[]);
        }
        if let Some(avail) = mem_available {
            sink.gauge("system.memory.available", avail, &[]);
        }
        if let (Some(total), Some(avail)) = (mem_total, mem_available) {
            sink.gauge("system.memory.used", total - avail, &[]);
            if let Some(pct) = memory_used_percent(total, avail) {
                sink.gauge("system.memory.used_percent", pct, &[]);
            }
        }
        if let Some(c) = cached {
            sink.gauge("system.memory.cached", c, &[]);
        }
        if let (Some(st), Some(sf)) = (swap_total, swap_free) {
            sink.gauge("system.memory.swap_used", st - sf, &[]);
            if let Some(pct) = swap_used_percent(st, st - sf) {
                sink.gauge("system.memory.swap_used_percent", pct, &[]);
            }
        }
    }
}

pub(crate) fn parse_kb(s: &str) -> Option<f64> {
    let num: f64 = s.split_whitespace().next()?.parse().ok()?;
    Some(num * 1024.0)
}
