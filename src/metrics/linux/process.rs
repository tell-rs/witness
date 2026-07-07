//! Process collector — scans /proc/{pid}/stat for top N processes.
//!
//! Emits gauges:
//! - system.process.cpu_percent  label {pid, name}
//! - system.process.memory_rss   label {pid, name}

use std::collections::HashMap;
use std::time::Instant;

use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

pub struct ProcessCollector {
    top_n: usize,
    prev: HashMap<u32, ProcTimes>,
    page_size: u64,
    /// Jiffies per second (`_SC_CLK_TCK`), for jiffie → percent conversion.
    clk_tck: f64,
    /// When the previous collection ran, for delta normalization.
    last_collect: Option<Instant>,
}

#[derive(Clone)]
struct ProcTimes {
    utime: u64,
    stime: u64,
}

struct ProcSnapshot {
    pid: u32,
    name: String,
    rss_bytes: u64,
    cpu_delta: u64,
}

impl ProcessCollector {
    pub fn new(top_n: usize) -> Self {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            top_n: top_n.max(1),
            prev: HashMap::new(),
            page_size,
            clk_tck: if clk_tck > 0 { clk_tck as f64 } else { 100.0 },
            last_collect: None,
        }
    }
}

impl Collector for ProcessCollector {
    fn name(&self) -> &'static str {
        "process"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };

        // Jiffie deltas are normalized by the actual elapsed time so the
        // emitted value is a true percentage, independent of tick interval.
        let elapsed = self
            .last_collect
            .replace(Instant::now())
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let mut snapshots: Vec<ProcSnapshot> = Vec::new();
        let mut seen_pids = std::collections::HashSet::<u32>::new();

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Only numeric directories (PIDs)
            let Ok(pid) = name_str.parse::<u32>() else {
                continue;
            };

            let stat_path = format!("/proc/{pid}/stat");
            if read_procfs(&stat_path, buf).is_err() {
                continue;
            }

            let Some(proc) = parse_proc_stat(buf, pid, self.page_size) else {
                continue;
            };
            seen_pids.insert(pid);

            // Compute CPU delta
            let cpu_delta = if let Some(prev) = self.prev.get(&pid) {
                (proc.utime + proc.stime).saturating_sub(prev.utime + prev.stime)
            } else {
                0
            };

            self.prev.insert(
                pid,
                ProcTimes {
                    utime: proc.utime,
                    stime: proc.stime,
                },
            );

            snapshots.push(ProcSnapshot {
                pid: proc.pid,
                name: proc.name,
                rss_bytes: proc.rss_bytes,
                cpu_delta,
            });
        }

        // Clean stale PIDs
        self.prev.retain(|pid, _| seen_pids.contains(pid));

        // Top N by CPU
        snapshots.sort_unstable_by(|a, b| b.cpu_delta.cmp(&a.cpu_delta));
        if elapsed > 0.0 {
            for proc in snapshots.iter().take(self.top_n) {
                if proc.cpu_delta == 0 {
                    break;
                }
                let percent = proc.cpu_delta as f64 / self.clk_tck / elapsed * 100.0;
                let pid_str = proc.pid.to_string();
                let labels: &[(&'static str, &str)] = &[("pid", &pid_str), ("name", &proc.name)];
                sink.gauge_dyn("system.process.cpu_percent", percent, labels);
            }
        }

        // Top N by RSS
        snapshots.sort_unstable_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes));
        for proc in snapshots.iter().take(self.top_n) {
            if proc.rss_bytes == 0 {
                break;
            }
            let pid_str = proc.pid.to_string();
            let labels: &[(&'static str, &str)] = &[("pid", &pid_str), ("name", &proc.name)];
            sink.gauge_dyn("system.process.memory_rss", proc.rss_bytes as f64, labels);
        }
    }
}

pub(crate) struct ParsedProc {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) utime: u64,
    pub(crate) stime: u64,
    pub(crate) rss_bytes: u64,
}

/// Parse /proc/{pid}/stat. The comm field is in parentheses and may contain
/// spaces and ')'. Find the LAST ')' to correctly delimit it.
pub(crate) fn parse_proc_stat(buf: &str, pid: u32, page_size: u64) -> Option<ParsedProc> {
    // Format: "pid (comm) state ppid ..."
    let open = buf.find('(')?;
    let close = buf.rfind(')')?;
    if close <= open {
        return None;
    }

    let name = buf[open + 1..close].to_string();

    // Fields after the closing ')' are space-separated, starting at index 2 (state)
    if close + 2 >= buf.len() {
        return None;
    }
    let rest = &buf[close + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();

    // utime = field 11 (0-indexed from after comm), stime = field 12
    // rss = field 21
    if fields.len() < 22 {
        return None;
    }

    let utime: u64 = fields[11].parse().ok()?;
    let stime: u64 = fields[12].parse().ok()?;
    let rss: u64 = fields[21].parse().ok()?;

    Some(ParsedProc {
        pid,
        name,
        utime,
        stime,
        rss_bytes: rss * page_size,
    })
}
