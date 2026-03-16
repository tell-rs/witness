//! Memory collector — uses mach host_statistics64() + sysctl.
//!
//! Emits gauges (bytes): system.memory.total, .available, .used, .cached, .swap_used

use crate::collectors::Collector;
use crate::sink::Sink;

pub struct MemoryCollector;

impl Collector for MemoryCollector {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, _buf: &mut String) {
        let total = sysctl_u64(c"hw.memsize");
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;

        let Some(vm) = read_vm_stats() else { return };

        let free = vm.free_count as u64 * page_size;
        let inactive = vm.inactive_count as u64 * page_size;
        let purgeable = vm.purgeable_count as u64 * page_size;
        let available = free + inactive + purgeable;
        let cached = inactive + purgeable;

        if let Some(t) = total {
            sink.gauge("system.memory.total", t as f64, &[]);
            sink.gauge("system.memory.used", (t - available) as f64, &[]);
        }
        sink.gauge("system.memory.available", available as f64, &[]);
        sink.gauge("system.memory.cached", cached as f64, &[]);

        // Swap
        let swap = read_swap_usage();
        if let Some((used, _total)) = swap {
            sink.gauge("system.memory.swap_used", used as f64, &[]);
        }
    }
}

/// Read a u64 sysctl by name.
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut val: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut val as *mut u64 as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(val) } else { None }
}

#[repr(C)]
struct VmStats {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

// Mach kernel function not wrapped by the mach2 crate.
unsafe extern "C" {
    fn host_statistics64(
        host: u32,
        flavor: i32,
        host_info_out: *mut i32,
        host_info_out_cnt: *mut u32,
    ) -> i32;
}

fn read_vm_stats() -> Option<VmStats> {
    use mach2::kern_return::KERN_SUCCESS;

    const HOST_VM_INFO64: i32 = 4;

    let mut stats = std::mem::MaybeUninit::<VmStats>::zeroed();
    let mut count = (std::mem::size_of::<VmStats>() / std::mem::size_of::<i32>()) as u32;

    let host = unsafe { mach2::mach_init::mach_host_self() };
    let ret =
        unsafe { host_statistics64(host, HOST_VM_INFO64, stats.as_mut_ptr().cast(), &mut count) };

    if ret == KERN_SUCCESS {
        Some(unsafe { stats.assume_init() })
    } else {
        None
    }
}

#[repr(C)]
struct XswUsage {
    xsu_total: u64,
    xsu_avail: u64,
    xsu_used: u64,
    xsu_pagesize: u32,
    xsu_encrypted: bool,
}

fn read_swap_usage() -> Option<(u64, u64)> {
    let mut usage = std::mem::MaybeUninit::<XswUsage>::zeroed();
    let mut size = std::mem::size_of::<XswUsage>();
    let ret = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            usage.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        let u = unsafe { usage.assume_init() };
        Some((u.xsu_used, u.xsu_total))
    } else {
        None
    }
}
