//! Load average collector — uses libc::getloadavg().
//!
//! Emits gauges: system.load.1, system.load.5, system.load.15

use crate::metrics::Collector;
use crate::sink::Sink;

pub struct LoadCollector;

impl Collector for LoadCollector {
    fn name(&self) -> &'static str {
        "load"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        let mut loads = [0.0f64; 3];
        let ret = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        if ret < 3 {
            return;
        }

        sink.gauge("system.load.1", loads[0], &[]);
        sink.gauge("system.load.5", loads[1], &[]);
        sink.gauge("system.load.15", loads[2], &[]);
    }
}
