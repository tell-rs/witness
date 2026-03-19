//! System metric collectors.
//!
//! Platform-specific implementations live in `linux/` and `macos/` subdirectories.
//! A shared `String` buffer is reused across all collectors to avoid
//! per-tick allocations.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;

#[cfg(test)]
mod metrics_test;

use crate::config::SystemConfig;
use crate::sink::Sink;

/// Trait for system metric collectors.
pub trait Collector: Send {
    fn name(&self) -> &'static str;
    fn collect(&mut self, sink: &Sink, hostname: &str, buf: &mut String);

    /// Emit cumulative checkpoint values for counter metrics.
    /// Called once per checkpoint interval (default: 1 hour). Only collectors
    /// that track deltas (disk I/O, network) need to override this.
    fn checkpoint(&mut self, _sink: &Sink, _hostname: &str) {}
}

/// Read a file into the shared buffer, reusing its allocation.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_procfs(path: &str, buf: &mut String) -> std::io::Result<()> {
    use std::io::Read;
    buf.clear();
    std::fs::File::open(path)?.read_to_string(buf)?;
    Ok(())
}

/// Initialize collectors based on config and current platform.
pub fn init_collectors(config: &SystemConfig) -> Vec<Box<dyn Collector>> {
    platform::init_collectors(config)
}
