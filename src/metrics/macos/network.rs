//! Network collector — uses getifaddrs() with AF_LINK.
//!
//! Emits counters (delta) per interface:
//! system.net.bytes_recv, .bytes_sent, .packets_recv, .packets_sent,
//! .errors_recv, .errors_sent, .drops_recv, .drops_sent

use std::collections::HashMap;

use tell::Temporality;

use crate::config::DeviceFilter;
use crate::metrics::Collector;
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

impl Collector for NetworkCollector {
    fn name(&self) -> &'static str {
        "network"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        let Some(ifaces) = read_if_stats() else {
            return;
        };

        for (iface, current) in &ifaces {
            if !self.filter.allows(iface) {
                continue;
            }

            if let Some(prev) = self.prev.get_mut(iface.as_str()) {
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
                *prev = current.clone();
            } else {
                self.prev.insert(iface.clone(), current.clone());
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

/// Read per-interface stats via getifaddrs() + AF_LINK sockaddr_dl data.
/// Skips interfaces that have never had any traffic since boot (cumulative
/// bytes == 0), so they never enter the collector's tracking map.
fn read_if_stats() -> Option<Vec<(String, NetStats)>> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return None;
    }

    let mut result: Vec<(String, NetStats)> = Vec::new();
    let mut cursor = ifap;

    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        cursor = ifa.ifa_next;

        let addr = ifa.ifa_addr;
        if addr.is_null() {
            continue;
        }

        // Only AF_LINK entries carry interface-level counters
        if unsafe { (*addr).sa_family } as i32 != libc::AF_LINK {
            continue;
        }

        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();

        // ifa_data points to struct if_data on macOS
        if ifa.ifa_data.is_null() {
            continue;
        }

        let data = ifa.ifa_data.cast::<IfData>();
        let d = unsafe { &*data };

        // Skip interfaces that have never carried any traffic since boot.
        // Once an interface gets its first packet, it enters tracking and
        // stays tracked even during idle periods (emitting zero deltas).
        if d.ifi_ibytes == 0 && d.ifi_obytes == 0 {
            continue;
        }

        result.push((
            name,
            NetStats {
                rx_bytes: d.ifi_ibytes as u64,
                rx_packets: d.ifi_ipackets as u64,
                rx_errors: d.ifi_ierrors as u64,
                rx_drops: d.ifi_iqdrops as u64,
                tx_bytes: d.ifi_obytes as u64,
                tx_packets: d.ifi_opackets as u64,
                tx_errors: d.ifi_oerrors as u64,
                tx_drops: 0, // macOS doesn't track outbound drops in if_data
            },
        ));
    }

    unsafe { libc::freeifaddrs(ifap) };
    Some(result)
}

/// Subset of macOS struct if_data fields we need.
/// Matches the layout in <net/if_var.h>.
#[repr(C)]
#[allow(non_camel_case_types)]
struct IfData {
    ifi_type: u8,
    ifi_typelen: u8,
    ifi_physical: u8,
    ifi_addrlen: u8,
    ifi_hdrlen: u8,
    ifi_recvquota: u8,
    ifi_xmitquota: u8,
    ifi_unused1: u8,
    ifi_mtu: u32,
    ifi_metric: u32,
    ifi_baudrate: u32,
    ifi_ipackets: u32,
    ifi_ierrors: u32,
    ifi_opackets: u32,
    ifi_oerrors: u32,
    ifi_collisions: u32,
    ifi_ibytes: u32,
    ifi_obytes: u32,
    ifi_imcasts: u32,
    ifi_omcasts: u32,
    ifi_iqdrops: u32,
    ifi_noproto: u32,
    ifi_recvtiming: u32,
    ifi_xmittiming: u32,
    // We don't need remaining fields.
}
