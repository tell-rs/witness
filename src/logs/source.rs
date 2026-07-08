//! Shared machinery for log sources.
//!
//! `journal.rs` (systemd journald) and `unified.rs` (macOS unified log) differ
//! precisely on resume semantics — an opaque cursor vs a timestamp checkpoint
//! plus `log show` backfill and mach-timestamp dedupe — so there is
//! deliberately no `LogSource` trait (it would abstract the wrong axis). The
//! only genuinely-shared machinery is (a) the atomic checkpoint write and
//! (b) the exponential-backoff restart delay; both live here as free functions.

use std::path::Path;
use std::time::Duration;

/// Persist a checkpoint atomically: write to a sibling `.tmp` file, `fsync`,
/// then `rename` over the target. A crash mid-write leaves the previous
/// checkpoint intact — the rename is the last, atomic step, and nothing ever
/// truncates the real file in place.
///
/// Best-effort: any I/O error (e.g. an unwritable state dir) is swallowed. A
/// checkpoint is a resume optimization, never a correctness requirement — the
/// source re-reads from the last durable point on restart, and backpressure is
/// honored regardless of whether the checkpoint landed.
pub(crate) fn write_checkpoint(path: &Path, bytes: &[u8]) {
    use std::io::Write;

    let tmp = path.with_extension("tmp");
    let Ok(mut file) = std::fs::File::create(&tmp) else {
        return;
    };
    if file.write_all(bytes).is_ok() && file.sync_all().is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Next exponential-backoff delay: double the current delay, clamped to `max`.
///
/// Used identically by every source's restart loop after an unexpected
/// subprocess exit.
#[must_use]
pub(crate) fn next_backoff(current: Duration, max: Duration) -> Duration {
    (current * 2).min(max)
}
