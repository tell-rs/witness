//! Derived (ratio) metrics shared by the Linux and macOS collectors.
//!
//! Percentages and per-CPU ratios let a Tell board compare hosts of different
//! sizes without writing formulas. Every function here is pure so the edge
//! cases (zero totals, no swap, no CPUs) are tested on every platform, even
//! though the collectors that call them are platform-gated.

/// Memory in use as a percentage of total: `(total - available) / total * 100`.
///
/// `available` is the kernel's estimate of memory obtainable without swapping
/// (Linux `MemAvailable`; on macOS free + inactive + purgeable pages), so the
/// page cache does not count as used. Returns `None` when `total` is zero,
/// negative or not finite. The result is clamped to `0..=100` because the
/// macOS page-count estimate can momentarily exceed `total`.
#[must_use]
pub fn memory_used_percent(total: f64, available: f64) -> Option<f64> {
    ratio_percent(total - available, total)
}

/// Swap in use as a percentage of swap total: `used / total * 100`.
///
/// Returns `None` when the host has no swap (`total` is zero), so a board
/// shows "no data" instead of a misleading 0 %.
#[must_use]
pub fn swap_used_percent(total: f64, used: f64) -> Option<f64> {
    ratio_percent(used, total)
}

/// Busy CPU time as a percentage: `100 - idle`.
///
/// `idle_percent` is the `system.cpu.idle` value for the same interval, so the
/// result shares its semantics (on Linux, iowait and steal are not idle and
/// therefore count as busy). Clamped to `0..=100`; a non-finite input yields
/// `None`.
#[must_use]
pub fn cpu_busy_percent(idle_percent: f64) -> Option<f64> {
    if !idle_percent.is_finite() {
        return None;
    }
    Some((100.0 - idle_percent).clamp(0.0, 100.0))
}

/// A load average divided by the logical CPU count.
///
/// Returns `None` when `cpu_count` is zero (the count could not be read).
#[must_use]
pub fn load_per_cpu(load: f64, cpu_count: usize) -> Option<f64> {
    if cpu_count == 0 || !load.is_finite() {
        return None;
    }
    Some(load / cpu_count as f64)
}

/// Count the CPUs in a kernel CPU list such as `0-3,5,8-11`.
///
/// This is the format of `/sys/devices/system/cpu/online`. Returns `None` for
/// an empty or malformed list, or a range whose end precedes its start.
#[must_use]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn count_cpu_list(list: &str) -> Option<usize> {
    let list = list.trim();
    if list.is_empty() {
        return None;
    }
    list.split(',').try_fold(0usize, |acc, part| {
        let n = count_cpu_range(part.trim())?;
        acc.checked_add(n)
    })
}

/// Count the CPUs in one list element: `7` or `0-3`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn count_cpu_range(part: &str) -> Option<usize> {
    let Some((lo, hi)) = part.split_once('-') else {
        part.parse::<usize>().ok()?;
        return Some(1);
    };
    let lo: usize = lo.parse().ok()?;
    let hi: usize = hi.parse().ok()?;
    hi.checked_sub(lo)?.checked_add(1)
}

/// `part / whole * 100`, clamped to `0..=100`; `None` when `whole` is not a
/// positive finite number or `part` is not finite.
fn ratio_percent(part: f64, whole: f64) -> Option<f64> {
    if !whole.is_finite() || whole <= 0.0 || !part.is_finite() {
        return None;
    }
    Some((part / whole * 100.0).clamp(0.0, 100.0))
}
