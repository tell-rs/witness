//! Cgroups v2 collector — reads /sys/fs/cgroup/.
//!
//! Emits counters (delta): system.cgroup.cpu.usage_usec, .user_usec, .system_usec,
//!                         .throttled_usec, .nr_throttled
//! Emits gauges: system.cgroup.memory.usage, .limit, .anon, .file, .kernel

use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

pub struct CgroupCollector {
    cgroup_path: Option<String>,
    prev_cpu: Option<CpuStats>,
}

#[derive(Clone, Default)]
struct CpuStats {
    usage_usec: u64,
    user_usec: u64,
    system_usec: u64,
    throttled_usec: u64,
    nr_throttled: u64,
}

impl CgroupCollector {
    pub fn new() -> Self {
        Self {
            cgroup_path: detect_cgroup_path(),
            prev_cpu: None,
        }
    }
}

impl Collector for CgroupCollector {
    fn name(&self) -> &'static str {
        "cgroups"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        let Some(ref base) = self.cgroup_path else {
            return;
        };

        // CPU stats (delta counters)
        let cpu_path = format!("{base}/cpu.stat");
        if read_procfs(&cpu_path, buf).is_ok() {
            let current = parse_cpu_stat(buf);
            if let Some(prev) = &self.prev_cpu {
                let d = |c: u64, p: u64| c.saturating_sub(p) as f64;
                sink.counter(
                    "system.cgroup.cpu.usage_usec",
                    d(current.usage_usec, prev.usage_usec),
                    &[],
                );
                sink.counter(
                    "system.cgroup.cpu.user_usec",
                    d(current.user_usec, prev.user_usec),
                    &[],
                );
                sink.counter(
                    "system.cgroup.cpu.system_usec",
                    d(current.system_usec, prev.system_usec),
                    &[],
                );
                sink.counter(
                    "system.cgroup.cpu.throttled_usec",
                    d(current.throttled_usec, prev.throttled_usec),
                    &[],
                );
                sink.counter(
                    "system.cgroup.cpu.nr_throttled",
                    d(current.nr_throttled, prev.nr_throttled),
                    &[],
                );
            }
            self.prev_cpu = Some(current);
        }

        // Memory current usage
        let mem_path = format!("{base}/memory.current");
        if read_procfs(&mem_path, buf).is_ok() {
            if let Ok(bytes) = buf.trim().parse::<f64>() {
                sink.gauge("system.cgroup.memory.usage", bytes, &[]);
            }
        }

        // Memory limit
        let limit_path = format!("{base}/memory.max");
        if read_procfs(&limit_path, buf).is_ok() {
            let trimmed = buf.trim();
            if trimmed != "max" {
                if let Ok(bytes) = trimmed.parse::<f64>() {
                    sink.gauge("system.cgroup.memory.limit", bytes, &[]);
                }
            }
        }

        // Memory stat (detailed breakdown)
        let stat_path = format!("{base}/memory.stat");
        if read_procfs(&stat_path, buf).is_ok() {
            for line in buf.lines() {
                let mut parts = line.split_whitespace();
                let Some(key) = parts.next() else { continue };
                let Some(val) = parts.next().and_then(|s| s.parse::<f64>().ok()) else {
                    continue;
                };
                match key {
                    "anon" => sink.gauge("system.cgroup.memory.anon", val, &[]),
                    "file" => sink.gauge("system.cgroup.memory.file", val, &[]),
                    "kernel" => sink.gauge("system.cgroup.memory.kernel", val, &[]),
                    _ => {}
                }
            }
        }
    }
}

fn parse_cpu_stat(buf: &str) -> CpuStats {
    let mut stats = CpuStats::default();
    for line in buf.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(val) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        match key {
            "usage_usec" => stats.usage_usec = val,
            "user_usec" => stats.user_usec = val,
            "system_usec" => stats.system_usec = val,
            "throttled_usec" => stats.throttled_usec = val,
            "nr_throttled" => stats.nr_throttled = val,
            _ => {}
        }
    }
    stats
}

/// Detect the cgroup v2 path for this process.
fn detect_cgroup_path() -> Option<String> {
    // Check cgroups v2 is available
    if !std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return None;
    }

    // Read our own cgroup: "0::/path"
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in content.lines() {
        if let Some(path) = line.strip_prefix("0::") {
            let cgroup = path.trim();
            if cgroup == "/" {
                return Some("/sys/fs/cgroup".to_string());
            }
            return Some(format!("/sys/fs/cgroup{cgroup}"));
        }
    }

    None
}
