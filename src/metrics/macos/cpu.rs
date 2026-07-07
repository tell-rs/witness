//! CPU collector — uses mach host_processor_info().
//!
//! Emits gauges (percentage, 0-100):
//! - system.cpu.user, .system, .idle
//!
//! Labels: {core: "total"} or {core: "0"}, {core: "1"}, ...
//! First tick stores baseline — no metrics emitted until second tick.

use std::collections::HashMap;

use crate::metrics::Collector;
use crate::sink::Sink;

pub struct CpuCollector {
    prev: HashMap<String, CpuTicks>,
}

/// Tick counts held as u64: the kernel reports u32 per core, but the
/// aggregate "total" sums all cores and would overflow u32 within weeks
/// on a many-core mostly-idle host.
#[derive(Clone, Default)]
struct CpuTicks {
    user: u64,
    system: u64,
    idle: u64,
    nice: u64,
}

impl CpuTicks {
    fn total(&self) -> u64 {
        self.user + self.system + self.idle + self.nice
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

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        let Some(per_cpu) = read_cpu_ticks() else {
            return;
        };

        // Aggregate total across all cores
        let mut total = CpuTicks::default();
        for ticks in &per_cpu {
            total.user += ticks.user;
            total.system += ticks.system;
            total.idle += ticks.idle;
            total.nice += ticks.nice;
        }

        emit_cpu(sink, "total", &total, &mut self.prev);

        for (i, ticks) in per_cpu.iter().enumerate() {
            let label = i.to_string();
            emit_cpu(sink, &label, ticks, &mut self.prev);
        }
    }
}

fn emit_cpu(sink: &Sink, label: &str, current: &CpuTicks, prev: &mut HashMap<String, CpuTicks>) {
    if let Some(prev_val) = prev.get(label) {
        let dt = current.total().saturating_sub(prev_val.total());
        if dt > 0 {
            let d = dt as f64;
            let labels: &[(&'static str, &str)] = &[("core", label)];

            let du = (current.user + current.nice).saturating_sub(prev_val.user + prev_val.nice);
            sink.gauge_dyn("system.cpu.user", du as f64 / d * 100.0, labels);
            sink.gauge_dyn(
                "system.cpu.system",
                current.system.saturating_sub(prev_val.system) as f64 / d * 100.0,
                labels,
            );
            sink.gauge_dyn(
                "system.cpu.idle",
                current.idle.saturating_sub(prev_val.idle) as f64 / d * 100.0,
                labels,
            );
        }
    }

    prev.insert(label.to_string(), current.clone());
}

// Mach kernel functions not wrapped by the mach2 crate.
unsafe extern "C" {
    fn host_processor_info(
        host: u32,
        flavor: i32,
        out_processor_count: *mut u32,
        out_processor_info: *mut *mut i32,
        out_processor_info_cnt: *mut u32,
    ) -> i32;
}

/// Read per-CPU tick counts via mach host_processor_info().
fn read_cpu_ticks() -> Option<Vec<CpuTicks>> {
    use mach2::kern_return::KERN_SUCCESS;

    const PROCESSOR_CPU_LOAD_INFO: i32 = 2;
    const CPU_STATE_USER: usize = 0;
    const CPU_STATE_SYSTEM: usize = 1;
    const CPU_STATE_IDLE: usize = 2;
    const CPU_STATE_NICE: usize = 3;
    const CPU_STATE_MAX: usize = 4;

    let host = super::host_port();
    let mut num_cpus: u32 = 0;
    let mut info: *mut i32 = std::ptr::null_mut();
    let mut info_count: u32 = 0;

    let ret = unsafe {
        host_processor_info(
            host,
            PROCESSOR_CPU_LOAD_INFO,
            &mut num_cpus,
            &mut info,
            &mut info_count,
        )
    };

    if ret != KERN_SUCCESS || info.is_null() {
        return None;
    }

    let mut cpus = Vec::with_capacity(num_cpus as usize);
    for i in 0..num_cpus as usize {
        let base = i * CPU_STATE_MAX;
        let ticks = unsafe {
            CpuTicks {
                user: (*info.add(base + CPU_STATE_USER) as u32).into(),
                system: (*info.add(base + CPU_STATE_SYSTEM) as u32).into(),
                idle: (*info.add(base + CPU_STATE_IDLE) as u32).into(),
                nice: (*info.add(base + CPU_STATE_NICE) as u32).into(),
            }
        };
        cpus.push(ticks);
    }

    // Free the mach-allocated buffer on every path — including num_cpus == 0.
    unsafe {
        mach2::vm::mach_vm_deallocate(
            mach2::traps::mach_task_self(),
            info as u64,
            info_count as u64 * std::mem::size_of::<i32>() as u64,
        );
    }

    if cpus.is_empty() { None } else { Some(cpus) }
}
