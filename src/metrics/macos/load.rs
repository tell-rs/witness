//! Load average collector — uses libc::getloadavg().
//!
//! Emits gauges: system.load.1, system.load.5, system.load.15, and
//! system.load.1_per_cpu = load.1 / online logical CPUs
//! (`sysconf(_SC_NPROCESSORS_ONLN)`).

use crate::metrics::Collector;
use crate::metrics::derived::load_per_cpu;
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

        if let Some(v) = online_cpus().and_then(|n| load_per_cpu(loads[0], n)) {
            sink.gauge("system.load.1_per_cpu", v, &[]);
        }
    }
}

/// Online logical CPU count; `None` when sysconf fails.
fn online_cpus() -> Option<usize> {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    usize::try_from(n).ok().filter(|&n| n > 0)
}
