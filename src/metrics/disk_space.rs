//! Filesystem space — shared by the Linux and macOS disk collectors.
//!
//! Emits gauges per mount: system.disk.total_bytes, .used_bytes, .free_bytes,
//! .available_bytes, .used_percent, and system.disk.inodes_total,
//! .inodes_used, .inodes_free, .inodes_used_percent.
//!
//! `free_bytes` includes root-reserved blocks (`f_bfree`); `available_bytes`
//! is what an unprivileged process can still write (`f_bavail`, df's "Avail").
//! `used_percent` matches df's "Use%": used / (used + available), so a
//! filesystem whose reserved blocks are all that's left reads 100%.
//!
//! `SpaceReader` bounds each `statvfs()` so a hung network mount is skipped
//! instead of stalling every collector.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use tracing::{info, warn};

use crate::sink::Sink;

/// How long one mount gets to answer `statvfs()` before it is skipped.
pub const STATVFS_TIMEOUT: Duration = Duration::from_secs(2);

/// Raw `statvfs()` fields, already scaled to bytes where applicable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceStats {
    pub total: f64,
    pub free: f64,
    pub avail: f64,
    pub inodes_total: f64,
    pub inodes_free: f64,
}

impl SpaceStats {
    /// `statvfs()` the mount point. `None` on failure or a zero-size
    /// filesystem (pseudo filesystems that slipped through the type filter).
    ///
    /// Blocks until the filesystem answers — on a hung network mount that is
    /// forever. Collectors go through `SpaceReader`, which bounds the wait.
    pub fn read(mount_point: &str) -> Option<Self> {
        let c_path = CString::new(mount_point.as_bytes()).ok()?;
        let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        // SAFETY: c_path is a valid NUL-terminated string and stat points to
        // writable memory of the right size; statvfs initializes it on success.
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if ret != 0 {
            return None;
        }
        let stat = unsafe { stat.assume_init() };
        let bs = stat.f_frsize as f64;
        let s = Self {
            total: stat.f_blocks as f64 * bs,
            free: stat.f_bfree as f64 * bs,
            avail: stat.f_bavail as f64 * bs,
            inodes_total: stat.f_files as f64,
            inodes_free: stat.f_ffree as f64,
        };
        (s.total > 0.0).then_some(s)
    }

    pub fn used(&self) -> f64 {
        (self.total - self.free).max(0.0)
    }

    /// df's Use%: used / (used + available). `None` when both are zero.
    pub fn used_percent(&self) -> Option<f64> {
        let used = self.used();
        let denom = used + self.avail;
        (denom > 0.0).then(|| (used / denom * 100.0).clamp(0.0, 100.0))
    }

    pub fn inodes_used(&self) -> f64 {
        (self.inodes_total - self.inodes_free).max(0.0)
    }

    /// `None` for filesystems without a fixed inode table (btrfs, zfs report 0).
    pub fn inodes_used_percent(&self) -> Option<f64> {
        (self.inodes_total > 0.0)
            .then(|| (self.inodes_used() / self.inodes_total * 100.0).clamp(0.0, 100.0))
    }

    pub fn emit(&self, sink: &Sink, labels: &[(&'static str, &str)]) {
        sink.gauge_dyn("system.disk.total_bytes", self.total, labels);
        sink.gauge_dyn("system.disk.used_bytes", self.used(), labels);
        sink.gauge_dyn("system.disk.free_bytes", self.free, labels);
        sink.gauge_dyn("system.disk.available_bytes", self.avail, labels);
        if let Some(pct) = self.used_percent() {
            sink.gauge_dyn("system.disk.used_percent", pct, labels);
        }

        if self.inodes_total > 0.0 {
            sink.gauge_dyn("system.disk.inodes_total", self.inodes_total, labels);
            sink.gauge_dyn("system.disk.inodes_used", self.inodes_used(), labels);
            sink.gauge_dyn("system.disk.inodes_free", self.inodes_free, labels);
            if let Some(pct) = self.inodes_used_percent() {
                sink.gauge_dyn("system.disk.inodes_used_percent", pct, labels);
            }
        }
    }
}

/// Bounded, per-mount `statvfs()`.
///
/// Each read runs on a short-lived thread and is awaited with a timeout. A
/// thread spawn per mount per tick costs microseconds — noise next to the
/// tick interval — and keeps the design free of worker lifecycles. A call
/// that times out is abandoned: a thread stuck in uninterruptible sleep can't
/// be killed, so it is left to finish on its own, and the mount is skipped
/// until it does. At most one thread per mount is ever outstanding.
pub struct SpaceReader<F = fn(&str) -> Option<SpaceStats>> {
    read: Arc<F>,
    timeout: Duration,
    mounts: HashMap<String, MountState>,
}

#[derive(Default)]
struct MountState {
    in_flight: Arc<AtomicBool>,
    /// Set once a timeout has been logged; cleared when the mount answers.
    warned: bool,
}

/// Clears the in-flight flag when the read thread exits, even on panic.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Default for SpaceReader {
    fn default() -> Self {
        Self::with_reader(SpaceStats::read, STATVFS_TIMEOUT)
    }
}

impl<F> SpaceReader<F>
where
    F: Fn(&str) -> Option<SpaceStats> + Send + Sync + 'static,
{
    pub fn with_reader(read: F, timeout: Duration) -> Self {
        Self {
            read: Arc::new(read),
            timeout,
            mounts: HashMap::new(),
        }
    }

    /// Drop state for mounts no longer reported (call after a mount refresh)
    /// so mount churn can't grow the map. A mount with a read still stuck is
    /// kept: forgetting it would allow a second thread for the same mount.
    pub fn retain_mounts<'a>(&mut self, current: impl IntoIterator<Item = &'a str>) {
        let current: HashSet<&str> = current.into_iter().collect();
        self.mounts.retain(|mount, state| {
            current.contains(mount.as_str()) || state.in_flight.load(Ordering::Acquire)
        });
    }

    #[cfg(test)]
    pub fn tracked_mounts(&self) -> usize {
        self.mounts.len()
    }

    /// Space stats for `mount_point`, or `None` if the read failed, timed
    /// out, or an earlier read of this mount is still stuck.
    pub fn read(&mut self, mount_point: &str) -> Option<SpaceStats> {
        if !self.mounts.contains_key(mount_point) {
            self.mounts
                .insert(mount_point.to_string(), MountState::default());
        }
        let state = self.mounts.get_mut(mount_point)?;
        if state.in_flight.load(Ordering::Acquire) {
            return None;
        }

        let result = spawn_read(&self.read, &state.in_flight, mount_point, self.timeout);
        match result {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !state.warned {
                    state.warned = true;
                    warn!(
                        mount = mount_point,
                        timeout_secs = self.timeout.as_secs_f64(),
                        "statvfs timed out, skipping mount until it responds"
                    );
                }
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
            Ok(stats) => {
                if state.warned {
                    state.warned = false;
                    info!(mount = mount_point, "mount responding again");
                }
                stats
            }
        }
    }
}

fn spawn_read<F>(
    read: &Arc<F>,
    in_flight: &Arc<AtomicBool>,
    mount_point: &str,
    timeout: Duration,
) -> Result<Option<SpaceStats>, mpsc::RecvTimeoutError>
where
    F: Fn(&str) -> Option<SpaceStats> + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    let read = Arc::clone(read);
    let path = mount_point.to_string();
    in_flight.store(true, Ordering::Release);
    let guard = InFlightGuard(Arc::clone(in_flight));
    let spawned = std::thread::Builder::new()
        .name("witness-statvfs".into())
        .spawn(move || {
            let _guard = guard;
            // Receiver is gone if the caller already timed out.
            let _ = tx.send(read(&path));
        });
    // On spawn failure the closure (and guard) is dropped, clearing the flag,
    // and the receiver reports Disconnected.
    if let Err(e) = spawned {
        warn!(mount = mount_point, "statvfs thread spawn failed: {e}");
    }
    rx.recv_timeout(timeout)
}

/// Whether `fs_type` is in the configured list, or the platform default list
/// when none is configured.
pub fn fs_type_allowed(fs_type: &str, configured: &[String], defaults: &[&str]) -> bool {
    if configured.is_empty() {
        defaults.contains(&fs_type)
    } else {
        configured.iter().any(|t| t == fs_type)
    }
}
