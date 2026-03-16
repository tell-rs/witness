//! macOS collectors — mach APIs, sysctl, getifaddrs.

pub mod cpu;
pub mod disk;
pub mod load;
pub mod memory;
pub mod network;

#[cfg(test)]
mod cpu_test;
#[cfg(test)]
mod disk_test;
#[cfg(test)]
mod init_test;
#[cfg(test)]
mod load_test;
#[cfg(test)]
mod memory_test;
#[cfg(test)]
mod network_test;

use crate::collectors::Collector;
use crate::config::{DeviceFilter, SystemConfig};

pub fn init_collectors(config: &SystemConfig) -> Vec<Box<dyn Collector>> {
    let mut collectors: Vec<Box<dyn Collector>> = Vec::new();

    if config.load {
        collectors.push(Box::new(load::LoadCollector));
    }
    if config.memory {
        collectors.push(Box::new(memory::MemoryCollector));
    }
    if config.cpu {
        collectors.push(Box::new(cpu::CpuCollector::new()));
    }
    if config.disk {
        collectors.push(Box::new(disk::DiskCollector::new(DeviceFilter::new(
            &config.disk_filter,
            &[],
        ))));
    }
    if config.network {
        collectors.push(Box::new(network::NetworkCollector::new(DeviceFilter::new(
            &config.network_filter,
            &[
                "lo0", "gif*", "stf*", "anpi*", "awdl*", "llw*", "ap*", "utun*", "bridge*",
            ],
        ))));
    }

    collectors
}
