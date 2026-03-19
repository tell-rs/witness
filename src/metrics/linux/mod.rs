//! Linux collectors — read procfs / sysfs.

pub mod cgroups;
pub mod container;
pub mod cpu;
pub mod disk;
pub mod load;
pub mod memory;
pub mod network;
pub mod process;
pub mod tcp;

#[cfg(test)]
mod container_test;
#[cfg(test)]
mod disk_test;
#[cfg(test)]
mod network_test;

use crate::config::{DeviceFilter, SystemConfig};
use crate::metrics::Collector;

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
            &["ram*", "loop*", "dm-*"],
        ))));
    }
    if config.network {
        collectors.push(Box::new(network::NetworkCollector::new(DeviceFilter::new(
            &config.network_filter,
            &["lo", "docker0", "veth*", "br-*"],
        ))));
    }
    if config.tcp {
        collectors.push(Box::new(tcp::TcpCollector));
    }
    if config.cgroups {
        collectors.push(Box::new(cgroups::CgroupCollector::new()));
    }
    if config.containers {
        collectors.push(Box::new(container::ContainerCollector::new()));
    }
    if config.processes {
        collectors.push(Box::new(process::ProcessCollector::new(config.process_top)));
    }

    collectors
}
