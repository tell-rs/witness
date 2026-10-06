//! Disk collector — reads /proc/diskstats + statvfs() for space.
//!
//! Emits counters (delta): system.disk.read_bytes, .write_bytes, .read_ops, .write_ops
//! Emits space gauges per filesystem — see `metrics::disk_space`.
//!
//! Each device is reported once, at its shortest mount point: bind mounts
//! (`/var/lib/foo` on the same LV as `/`) would otherwise duplicate the series.

use std::collections::{HashMap, HashSet};

use tell::Temporality;

use crate::config::DeviceFilter;
use crate::metrics::disk_space::{SpaceReader, fs_type_allowed};
use crate::metrics::{Collector, read_procfs};
use crate::sink::Sink;

const SECTOR_SIZE: f64 = 512.0;

/// Local filesystems reported by default. Network filesystems (nfs, cifs,
/// ceph, fuse.*) are opt-in via `disk_fs_types`; a hung server is skipped
/// after `STATVFS_TIMEOUT` rather than stalling the tick.
const DEFAULT_FS_TYPES: &[&str] = &[
    "ext4", "ext3", "ext2", "xfs", "btrfs", "zfs", "vfat", "exfat", "ntfs", "ntfs3", "f2fs",
];

pub struct DiskCollector {
    prev: HashMap<String, DiskStats>,
    mounts: Vec<MountInfo>,
    filter: DeviceFilter,
    fs_types: Vec<String>,
    tick_count: u32,
    space: SpaceReader,
}

#[derive(Clone, Default)]
struct DiskStats {
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) struct MountInfo {
    pub(crate) device: String,
    pub(crate) mount_point: String,
}

impl DiskCollector {
    pub fn new(filter: DeviceFilter, fs_types: Vec<String>) -> Self {
        Self {
            prev: HashMap::new(),
            mounts: Vec::new(),
            filter,
            fs_types,
            tick_count: 0,
            space: SpaceReader::default(),
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
        if self.tick_count.is_multiple_of(30) {
            if read_procfs("/proc/mounts", buf).is_ok() {
                self.mounts = parse_mounts(buf, &self.fs_types);
                self.space
                    .retain_mounts(self.mounts.iter().map(|m| m.mount_point.as_str()));
            }
        }
        self.tick_count = self.tick_count.wrapping_add(1);

        collect_disk_space(sink, &mut self.space, &self.mounts);
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
        // /proc/diskstats: major minor NAME reads _ sectors_read _ writes _
        // sectors_written ... — parse off the iterator, no per-line Vec.
        let mut it = line.split_whitespace();
        let Some(name) = it.nth(2) else { continue };
        if !filter.allows(name) {
            continue;
        }

        // Fields 4..=14 relative to the line (11 tokens after the name).
        let mut counters = [0u64; 11];
        let mut n = 0;
        for (i, tok) in it.take(11).enumerate() {
            counters[i] = tok.parse().unwrap_or(0);
            n = i + 1;
        }
        if n < 11 {
            continue;
        }

        let current = DiskStats {
            reads_completed: counters[0],
            sectors_read: counters[2],
            writes_completed: counters[4],
            sectors_written: counters[6],
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

fn collect_disk_space(sink: &Sink, space: &mut SpaceReader, mounts: &[MountInfo]) {
    for mount in mounts {
        let Some(stats) = space.read(&mount.mount_point) else {
            continue;
        };
        let labels: &[(&'static str, &str)] =
            &[("mount", &mount.mount_point), ("device", &mount.device)];
        stats.emit(sink, labels);
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

/// Parse /proc/mounts into the mounts to report: allowed filesystem types,
/// one entry per device (shortest mount point wins).
pub(crate) fn parse_mounts(buf: &str, fs_types: &[String]) -> Vec<MountInfo> {
    let mut mounts: Vec<MountInfo> = buf
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (Some(device), Some(mount_point), Some(fs)) = (it.next(), it.next(), it.next())
            else {
                return None;
            };
            if !fs_type_allowed(fs, fs_types, DEFAULT_FS_TYPES) {
                return None;
            }
            Some(MountInfo {
                device: unescape_mount_field(device),
                mount_point: unescape_mount_field(mount_point),
            })
        })
        .collect();

    // Stable sort keeps /proc/mounts order among equal lengths.
    mounts.sort_by_key(|m| m.mount_point.len());
    let mut seen = HashSet::new();
    mounts.retain(|m| seen.insert(m.device.clone()));
    mounts
}

/// Decode the octal escapes the kernel writes in /proc/mounts fields
/// (`\040` space, `\011` tab, `\012` newline, `\134` backslash).
fn unescape_mount_field(field: &str) -> String {
    if !field.contains('\\') {
        return field.to_string();
    }
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // First digit 0-3 keeps the value within a byte (max \377).
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'3').contains(&bytes[i + 1])
            && bytes[i + 2..=i + 3]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let v = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
