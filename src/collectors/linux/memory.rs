//! Memory collector — reads /proc/meminfo.
//!
//! Emits gauges (bytes): system.memory.total, .available, .used, .cached, .swap_used

use crate::collectors::{Collector, read_procfs};
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
        }
        if let Some(c) = cached {
            sink.gauge("system.memory.cached", c, &[]);
        }
        if let (Some(st), Some(sf)) = (swap_total, swap_free) {
            sink.gauge("system.memory.swap_used", st - sf, &[]);
        }
    }
}

fn parse_kb(s: &str) -> Option<f64> {
    let num: f64 = s.trim().split_whitespace().next()?.parse().ok()?;
    Some(num * 1024.0)
}
