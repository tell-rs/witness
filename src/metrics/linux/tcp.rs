//! TCP connection state collector — reads /proc/net/tcp and /proc/net/tcp6.
//!
//! Emits gauges: system.tcp.connections  label {state: "established"|"listen"|...}

use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

pub struct TcpCollector;

impl Collector for TcpCollector {
    fn name(&self) -> &'static str {
        "tcp"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        let mut counts = [0u32; 12]; // states 0x00..0x0B

        if read_procfs("/proc/net/tcp", buf).is_ok() {
            count_states(buf, &mut counts);
        }

        if read_procfs("/proc/net/tcp6", buf).is_ok() {
            count_states(buf, &mut counts);
        }

        for (hex, name) in STATE_NAMES {
            let count = counts[*hex as usize];
            if count > 0 {
                sink.gauge("system.tcp.connections", count as f64, &[("state", name)]);
            }
        }
    }
}

fn count_states(buf: &str, counts: &mut [u32; 12]) {
    for line in buf.lines().skip(1) {
        // Fields: sl local_address rem_address st ...
        let mut parts = line.split_whitespace();
        let Some(_sl) = parts.next() else { continue };
        let Some(_local) = parts.next() else { continue };
        let Some(_remote) = parts.next() else {
            continue;
        };
        let Some(st) = parts.next() else { continue };

        if let Ok(state) = u8::from_str_radix(st, 16) {
            if (state as usize) < counts.len() {
                counts[state as usize] += 1;
            }
        }
    }
}

const STATE_NAMES: &[(u8, &str)] = &[
    (0x01, "established"),
    (0x02, "syn_sent"),
    (0x03, "syn_recv"),
    (0x04, "fin_wait1"),
    (0x05, "fin_wait2"),
    (0x06, "time_wait"),
    (0x07, "close"),
    (0x08, "close_wait"),
    (0x09, "last_ack"),
    (0x0A, "listen"),
    (0x0B, "closing"),
];
