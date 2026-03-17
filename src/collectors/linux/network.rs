//! Network collector — reads /proc/net/dev.
//!
//! /proc/net/dev fields per interface (16 total):
//! Receive:  bytes packets errs drop fifo frame compressed multicast
//!           [0]   [1]    [2]  [3]  [4]  [5]   [6]        [7]
//! Transmit: bytes packets errs drop fifo colls carrier compressed
//!           [8]   [9]    [10] [11] [12] [13]  [14]     [15]
//!
//! Emits counters (delta) per interface:
//! system.net.bytes_recv, .bytes_sent, .packets_recv, .packets_sent,
//! .errors_recv, .errors_sent, .drops_recv, .drops_sent

use std::collections::HashMap;

use tell::Temporality;

use crate::collectors::{Collector, read_procfs};
use crate::config::DeviceFilter;
use crate::sink::Sink;

pub struct NetworkCollector {
    prev: HashMap<String, NetStats>,
    filter: DeviceFilter,
}

#[derive(Clone, Default)]
struct NetStats {
    rx_bytes: u64,
    rx_packets: u64,
    rx_errors: u64,
    rx_drops: u64,
    tx_bytes: u64,
    tx_packets: u64,
    tx_errors: u64,
    tx_drops: u64,
}

impl NetworkCollector {
    pub fn new(filter: DeviceFilter) -> Self {
        Self {
            prev: HashMap::new(),
            filter,
        }
    }
}

#[cfg(test)]
impl NetworkCollector {
    pub fn inject_prev_stats(
        &mut self,
        iface: &str,
        rx_bytes: u64,
        rx_packets: u64,
        tx_bytes: u64,
        tx_packets: u64,
    ) {
        self.prev.insert(
            iface.to_string(),
            NetStats {
                rx_bytes,
                rx_packets,
                rx_errors: 0,
                rx_drops: 0,
                tx_bytes,
                tx_packets,
                tx_errors: 0,
                tx_drops: 0,
            },
        );
    }
}

impl Collector for NetworkCollector {
    fn name(&self) -> &'static str {
        "network"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        if read_procfs("/proc/net/dev", buf).is_err() {
            return;
        }

        for line in buf.lines() {
            let Some((iface, stats)) = line.split_once(':') else {
                continue;
            };
            let iface = iface.trim();

            if !self.filter.allows(iface) {
                continue;
            }

            let parts: Vec<u64> = stats
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() < 12 {
                continue;
            }

            let current = NetStats {
                rx_bytes: parts[0],
                rx_packets: parts[1],
                rx_errors: parts[2],
                rx_drops: parts[3],
                tx_bytes: parts[8],
                tx_packets: parts[9],
                tx_errors: parts[10],
                tx_drops: parts[11],
            };

            if let Some(prev) = self.prev.get_mut(iface) {
                let labels: &[(&'static str, &str)] = &[("interface", iface)];
                let d = |c: u64, p: u64| c.saturating_sub(p) as f64;
                sink.counter_dyn(
                    "system.net.bytes_recv",
                    d(current.rx_bytes, prev.rx_bytes),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.bytes_sent",
                    d(current.tx_bytes, prev.tx_bytes),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.packets_recv",
                    d(current.rx_packets, prev.rx_packets),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.packets_sent",
                    d(current.tx_packets, prev.tx_packets),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.errors_recv",
                    d(current.rx_errors, prev.rx_errors),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.errors_sent",
                    d(current.tx_errors, prev.tx_errors),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.drops_recv",
                    d(current.rx_drops, prev.rx_drops),
                    labels,
                );
                sink.counter_dyn(
                    "system.net.drops_sent",
                    d(current.tx_drops, prev.tx_drops),
                    labels,
                );
                *prev = current;
            } else {
                self.prev.insert(iface.to_string(), current);
            }
        }
    }

    fn checkpoint(&mut self, sink: &Sink, _hostname: &str) {
        for (iface, stats) in &self.prev {
            let labels: &[(&'static str, &str)] = &[("interface", iface)];
            sink.counter_dyn_with_temporality(
                "system.net.bytes_recv",
                stats.rx_bytes as f64,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.net.bytes_sent",
                stats.tx_bytes as f64,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.net.packets_recv",
                stats.rx_packets as f64,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.net.packets_sent",
                stats.tx_packets as f64,
                labels,
                Temporality::Cumulative,
            );
        }
    }
}
