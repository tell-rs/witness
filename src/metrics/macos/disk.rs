//! Disk collector — uses statvfs() for space.
//!
//! Emits space gauges per filesystem — see `metrics::disk_space`.
//!
//! Only reports `/` and `/Volumes/*` mounts. Deduplicates mounts that share
//! the same underlying APFS container (same total + free bytes).

use std::collections::HashSet;

use crate::config::DeviceFilter;
use crate::metrics::Collector;
use crate::metrics::disk_space::{SpaceReader, fs_type_allowed};
use crate::sink::Sink;

/// Local filesystems reported by default. Network shares (smbfs, nfs, afpfs)
/// are opt-in via `disk_fs_types`; a hung server is skipped after
/// `STATVFS_TIMEOUT` rather than stalling the tick.
const DEFAULT_FS_TYPES: &[&str] = &["apfs", "hfs", "msdos", "exfat", "ufs", "zfs"];

pub struct DiskCollector {
    mounts: Vec<MountInfo>,
    filter: DeviceFilter,
    fs_types: Vec<String>,
    tick_count: u32,
    space: SpaceReader,
}

struct MountInfo {
    device: String,
    mount_point: String,
}

impl DiskCollector {
    pub fn new(filter: DeviceFilter, fs_types: Vec<String>) -> Self {
        Self {
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

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        // Refresh mounts every 30 ticks
        if self.tick_count.is_multiple_of(30) {
            self.mounts = discover_mounts(&self.filter, &self.fs_types);
            self.space
                .retain_mounts(self.mounts.iter().map(|m| m.mount_point.as_str()));
        }
        self.tick_count = self.tick_count.wrapping_add(1);

        collect_disk_space(sink, &mut self.space, &self.mounts);
    }
}

fn collect_disk_space(sink: &Sink, space: &mut SpaceReader, mounts: &[MountInfo]) {
    // Deduplicate APFS container-shared volumes: multiple mounts can report
    // identical (total, free) because they share the same physical container.
    // Only emit the first one (shortest mount path wins from discover_mounts).
    let mut seen: HashSet<(u64, u64)> = HashSet::new();

    for mount in mounts {
        let Some(stats) = space.read(&mount.mount_point) else {
            continue;
        };
        if !seen.insert((stats.total.to_bits(), stats.free.to_bits())) {
            continue;
        }
        let labels: &[(&'static str, &str)] =
            &[("mount", &mount.mount_point), ("device", &mount.device)];
        stats.emit(sink, labels);
    }
}

/// Discover mounted filesystems. Only reports mounts at `/` or under `/Volumes/`
/// (external drives, network shares). Sorted by mount path length so shortest
/// path wins during APFS dedup.
fn discover_mounts(filter: &DeviceFilter, fs_types: &[String]) -> Vec<MountInfo> {
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

            if !fs_type_allowed(&fstype, fs_types, DEFAULT_FS_TYPES) {
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
