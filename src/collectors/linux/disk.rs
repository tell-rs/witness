//! Disk collector — reads /proc/diskstats + statvfs() for space.
//!
//! Emits counters (delta): system.disk.read_bytes, .write_bytes, .read_ops, .write_ops
//! Emits gauges: system.disk.total_bytes, .used_bytes, .free_bytes
//! Emits gauges: system.disk.inodes_total, .inodes_used, .inodes_free

use std::collections::HashMap;
use std::ffi::CString;
use std::mem::MaybeUninit;

use tell::Temporality;

use crate::collectors::{Collector, read_procfs};
use crate::config::DeviceFilter;
use crate::sink::Sink;

const SECTOR_SIZE: f64 = 512.0;

pub struct DiskCollector {
    prev: HashMap<String, DiskStats>,
    mounts: Vec<MountInfo>,
    filter: DeviceFilter,
    tick_count: u32,
}

#[derive(Clone, Default)]
struct DiskStats {
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
}

struct MountInfo {
    device: String,
    mount_point: String,
}

impl DiskCollector {
    pub fn new(filter: DeviceFilter) -> Self {
        Self {
            prev: HashMap::new(),
            mounts: Vec::new(),
            filter,
            tick_count: 0,
        }
    }
}

impl Collector for DiskCollector {
    fn name(&self) -> &'static str {
        "disk"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        // Disk I/O stats
        if read_procfs("/proc/diskstats", buf).is_ok() {
            collect_diskstats(sink, &mut self.prev, buf, &self.filter);
        }

        // Refresh mounts every 30 ticks (~5 min at 10s interval)
        if self.tick_count % 30 == 0 {
            if read_procfs("/proc/mounts", buf).is_ok() {
                self.mounts = parse_mounts(buf);
            }
        }
        self.tick_count = self.tick_count.wrapping_add(1);

        collect_disk_space(sink, &self.mounts);
    }

    fn checkpoint(&mut self, sink: &Sink, _hostname: &str) {
        for (device, stats) in &self.prev {
            let labels: &[(&'static str, &str)] = &[("device", device)];
            sink.counter_dyn_with_temporality(
                "system.disk.read_bytes",
                stats.sectors_read as f64 * SECTOR_SIZE,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.disk.write_bytes",
                stats.sectors_written as f64 * SECTOR_SIZE,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.disk.read_ops",
                stats.reads_completed as f64,
                labels,
                Temporality::Cumulative,
            );
            sink.counter_dyn_with_temporality(
                "system.disk.write_ops",
                stats.writes_completed as f64,
                labels,
                Temporality::Cumulative,
            );
        }
    }
}

fn collect_diskstats(
    sink: &Sink,
    prev: &mut HashMap<String, DiskStats>,
    buf: &str,
    filter: &DeviceFilter,
) {
    for line in buf.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 14 {
            continue;
        }

        let name = parts[2];
        if !filter.allows(name) {
            continue;
        }

        let current = DiskStats {
            reads_completed: parts[3].parse().unwrap_or(0),
            sectors_read: parts[5].parse().unwrap_or(0),
            writes_completed: parts[7].parse().unwrap_or(0),
            sectors_written: parts[9].parse().unwrap_or(0),
        };

        if let Some(p) = prev.get_mut(name) {
            let labels: &[(&'static str, &str)] = &[("device", name)];
            let dr = current.sectors_read.saturating_sub(p.sectors_read) as f64 * SECTOR_SIZE;
            let dw = current.sectors_written.saturating_sub(p.sectors_written) as f64 * SECTOR_SIZE;
            let dro = current.reads_completed.saturating_sub(p.reads_completed) as f64;
            let dwo = current.writes_completed.saturating_sub(p.writes_completed) as f64;
            if dr > 0.0 || dw > 0.0 || dro > 0.0 || dwo > 0.0 {
                sink.counter_dyn("system.disk.read_bytes", dr, labels);
                sink.counter_dyn("system.disk.write_bytes", dw, labels);
                sink.counter_dyn("system.disk.read_ops", dro, labels);
                sink.counter_dyn("system.disk.write_ops", dwo, labels);
            }
            *p = current;
        } else {
            prev.insert(name.to_string(), current);
        }
    }
}

fn collect_disk_space(sink: &Sink, mounts: &[MountInfo]) {
    for mount in mounts {
        let Ok(c_path) = CString::new(mount.mount_point.as_bytes()) else {
            continue;
        };

        let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if ret != 0 {
            continue;
        }

        let stat = unsafe { stat.assume_init() };
        let bs = stat.f_frsize as f64;
        let total = stat.f_blocks as f64 * bs;
        let free = stat.f_bfree as f64 * bs;
        if total == 0.0 {
            continue;
        }

        let labels: &[(&'static str, &str)] =
            &[("mount", &mount.mount_point), ("device", &mount.device)];
        sink.gauge_dyn("system.disk.total_bytes", total, labels);
        sink.gauge_dyn("system.disk.used_bytes", total - free, labels);
        sink.gauge_dyn("system.disk.free_bytes", free, labels);

        let inodes_total = stat.f_files as f64;
        let inodes_free = stat.f_ffree as f64;
        if inodes_total > 0.0 {
            sink.gauge_dyn("system.disk.inodes_total", inodes_total, labels);
            sink.gauge_dyn(
                "system.disk.inodes_used",
                inodes_total - inodes_free,
                labels,
            );
            sink.gauge_dyn("system.disk.inodes_free", inodes_free, labels);
        }
    }
}

#[cfg(test)]
impl DiskCollector {
    pub fn inject_prev_stats(
        &mut self,
        device: &str,
        reads_completed: u64,
        sectors_read: u64,
        writes_completed: u64,
        sectors_written: u64,
    ) {
        self.prev.insert(
            device.to_string(),
            DiskStats {
                reads_completed,
                sectors_read,
                writes_completed,
                sectors_written,
            },
        );
    }
}

fn parse_mounts(buf: &str) -> Vec<MountInfo> {
    buf.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return None;
            }
            let fs = parts[2];
            if !matches!(
                fs,
                "ext4" | "ext3" | "ext2" | "xfs" | "btrfs" | "zfs" | "vfat" | "ntfs" | "f2fs"
            ) {
                return None;
            }
            Some(MountInfo {
                device: parts[0].to_string(),
                mount_point: parts[1].to_string(),
            })
        })
        .collect()
}
