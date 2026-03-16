//! Disk collector — uses statvfs() for space.
//!
//! Emits gauges: system.disk.total_bytes, .used_bytes, .free_bytes
//! Emits gauges: system.disk.inodes_total, .inodes_used, .inodes_free
//!
//! Only reports `/` and `/Volumes/*` mounts. Deduplicates mounts that share
//! the same underlying APFS container (same total + free bytes).

use std::collections::HashSet;
use std::ffi::CString;
use std::mem::MaybeUninit;

use crate::collectors::Collector;
use crate::config::DeviceFilter;
use crate::sink::Sink;

pub struct DiskCollector {
    mounts: Vec<MountInfo>,
    filter: DeviceFilter,
    tick_count: u32,
}

struct MountInfo {
    device: String,
    mount_point: String,
}

impl DiskCollector {
    pub fn new(filter: DeviceFilter) -> Self {
        Self {
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

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        // Refresh mounts every 30 ticks
        if self.tick_count % 30 == 0 {
            self.mounts = discover_mounts(&self.filter);
        }
        self.tick_count = self.tick_count.wrapping_add(1);

        collect_disk_space(sink, &self.mounts);
    }
}

fn collect_disk_space(sink: &Sink, mounts: &[MountInfo]) {
    // Deduplicate APFS container-shared volumes: multiple mounts can report
    // identical (total, free) because they share the same physical container.
    // Only emit the first one (shortest mount path wins from discover_mounts).
    let mut seen: HashSet<(libc::fsblkcnt_t, libc::fsblkcnt_t)> = HashSet::new();

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

        // Skip if we already emitted a mount with identical space (APFS dedup)
        let key = (stat.f_blocks, stat.f_bfree);
        if !seen.insert(key) {
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

/// Discover mounted filesystems. Only reports mounts at `/` or under `/Volumes/`
/// (external drives, network shares). Sorted by mount path length so shortest
/// path wins during APFS dedup.
fn discover_mounts(filter: &DeviceFilter) -> Vec<MountInfo> {
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }

    let mut stats: Vec<libc::statfs> = Vec::with_capacity(count as usize);
    let buf_size = count as libc::c_int * std::mem::size_of::<libc::statfs>() as libc::c_int;
    let actual = unsafe { libc::getfsstat(stats.as_mut_ptr(), buf_size, libc::MNT_NOWAIT) };
    if actual <= 0 {
        return Vec::new();
    }
    unsafe { stats.set_len(actual as usize) };

    let mut mounts: Vec<MountInfo> = stats
        .iter()
        .filter_map(|fs| {
            let fstype =
                unsafe { std::ffi::CStr::from_ptr(fs.f_fstypename.as_ptr()) }.to_string_lossy();

            // Only real filesystems
            if !matches!(
                fstype.as_ref(),
                "apfs" | "hfs" | "msdos" | "exfat" | "ufs" | "zfs"
            ) {
                return None;
            }

            let device = unsafe { std::ffi::CStr::from_ptr(fs.f_mntfromname.as_ptr()) }
                .to_string_lossy()
                .into_owned();

            let mount_point = unsafe { std::ffi::CStr::from_ptr(fs.f_mntonname.as_ptr()) }
                .to_string_lossy()
                .into_owned();

            // Only report root and external/network drives
            if mount_point != "/" && !mount_point.starts_with("/Volumes/") {
                return None;
            }

            // Apply device filter on the device basename
            let dev_name = device.rsplit('/').next().unwrap_or(&device);
            if !filter.allows(dev_name) {
                return None;
            }

            Some(MountInfo {
                device,
                mount_point,
            })
        })
        .collect();

    // Sort by mount path length — shortest first so `/` wins during dedup
    mounts.sort_by_key(|m| m.mount_point.len());
    mounts
}
