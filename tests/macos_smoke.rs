//! Smoke test — verifies macOS collectors produce real data.
//! Run with: cargo test --test macos_smoke -- --nocapture

#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::mem::MaybeUninit;

unsafe extern "C" {
    fn host_processor_info(
        host: u32,
        flavor: i32,
        out_count: *mut u32,
        out_info: *mut *mut i32,
        out_info_cnt: *mut u32,
    ) -> i32;
    fn host_statistics64(host: u32, flavor: i32, out: *mut i32, cnt: *mut u32) -> i32;
    fn mach_host_self() -> u32;
}

#[test]
fn load_average() {
    let mut loads = [0.0f64; 3];
    let ret = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    assert_eq!(ret, 3, "getloadavg failed");
    assert!(loads[0] >= 0.0);

    eprintln!(
        "load: 1m={:.2}  5m={:.2}  15m={:.2}",
        loads[0], loads[1], loads[2]
    );
}

#[test]
fn cpu_per_core() {
    let host = unsafe { mach_host_self() };
    let mut num_cpus: u32 = 0;
    let mut info: *mut i32 = std::ptr::null_mut();
    let mut info_count: u32 = 0;

    let ret = unsafe { host_processor_info(host, 2, &mut num_cpus, &mut info, &mut info_count) };
    assert_eq!(ret, 0, "host_processor_info failed");
    assert!(num_cpus > 0);
    assert!(!info.is_null());

    eprintln!("cpu: {num_cpus} cores detected");
    let mut total = [0u64; 4];
    for i in 0..num_cpus as usize {
        let base = i * 4;
        let user = unsafe { *info.add(base) } as u64;
        let system = unsafe { *info.add(base + 1) } as u64;
        let idle = unsafe { *info.add(base + 2) } as u64;
        let nice = unsafe { *info.add(base + 3) } as u64;
        total[0] += user;
        total[1] += system;
        total[2] += idle;
        total[3] += nice;
    }
    let sum = total[0] + total[1] + total[2] + total[3];
    eprintln!(
        "cpu: user={:.1}%  sys={:.1}%  idle={:.1}%",
        total[0] as f64 / sum as f64 * 100.0,
        total[1] as f64 / sum as f64 * 100.0,
        total[2] as f64 / sum as f64 * 100.0,
    );
}

#[test]
fn memory_stats() {
    // Total RAM
    let mut total_mem: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut total_mem as *mut u64 as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(ret, 0, "sysctl hw.memsize failed");
    assert!(total_mem > 0);

    // VM stats
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

    let mut stats = MaybeUninit::<VmStats>::zeroed();
    let mut count = (std::mem::size_of::<VmStats>() / std::mem::size_of::<i32>()) as u32;
    let host = unsafe { mach_host_self() };
    let vm_ret = unsafe { host_statistics64(host, 4, stats.as_mut_ptr().cast(), &mut count) };
    assert_eq!(vm_ret, 0, "host_statistics64 failed");

    let vm = unsafe { stats.assume_init() };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let free = vm.free_count as u64 * page_size;
    let active = vm.active_count as u64 * page_size;
    let inactive = vm.inactive_count as u64 * page_size;
    let wired = vm.wire_count as u64 * page_size;
    let compressed = vm.compressor_page_count as u64 * page_size;
    let purgeable = vm.purgeable_count as u64 * page_size;
    let available = free + inactive + purgeable;

    eprintln!("memory: total={:.1}GB", total_mem as f64 / 1e9);
    eprintln!(
        "  free={:.1}GB  active={:.1}GB  inactive={:.1}GB  wired={:.1}GB  compressed={:.1}GB  purgeable={:.1}GB",
        free as f64 / 1e9,
        active as f64 / 1e9,
        inactive as f64 / 1e9,
        wired as f64 / 1e9,
        compressed as f64 / 1e9,
        purgeable as f64 / 1e9,
    );
    eprintln!(
        "  available={:.1}GB  used={:.1}GB ({:.0}%)",
        available as f64 / 1e9,
        (total_mem - available) as f64 / 1e9,
        (total_mem - available) as f64 / total_mem as f64 * 100.0,
    );
}

#[test]
fn network_interfaces() {
    #[repr(C)]
    struct IfData {
        ifi_type: u8,
        ifi_typelen: u8,
        ifi_physical: u8,
        ifi_addrlen: u8,
        ifi_hdrlen: u8,
        ifi_recvquota: u8,
        ifi_xmitquota: u8,
        ifi_unused1: u8,
        ifi_mtu: u32,
        ifi_metric: u32,
        ifi_baudrate: u32,
        ifi_ipackets: u32,
        ifi_ierrors: u32,
        ifi_opackets: u32,
        ifi_oerrors: u32,
        ifi_collisions: u32,
        ifi_ibytes: u32,
        ifi_obytes: u32,
        ifi_imcasts: u32,
        ifi_omcasts: u32,
        ifi_iqdrops: u32,
        ifi_noproto: u32,
        ifi_recvtiming: u32,
        ifi_xmittiming: u32,
    }

    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    let ret = unsafe { libc::getifaddrs(&mut ifap) };
    assert_eq!(ret, 0, "getifaddrs failed");

    let mut found_any = false;
    let mut cursor = ifap;
    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        cursor = ifa.ifa_next;

        let addr = ifa.ifa_addr;
        if addr.is_null() || unsafe { (*addr).sa_family } as i32 != libc::AF_LINK {
            continue;
        }
        if ifa.ifa_data.is_null() {
            continue;
        }

        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        let d = unsafe { &*(ifa.ifa_data as *const IfData) };

        if d.ifi_ibytes == 0 && d.ifi_obytes == 0 {
            continue;
        }

        found_any = true;
        eprintln!(
            "net {name}: rx={} tx={} (pkts: rx={} tx={})",
            fmt_bytes(d.ifi_ibytes as u64),
            fmt_bytes(d.ifi_obytes as u64),
            d.ifi_ipackets,
            d.ifi_opackets,
        );
    }

    unsafe { libc::freeifaddrs(ifap) };
    assert!(found_any, "no active network interfaces found");
}

#[test]
fn disk_mounts() {
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    assert!(count > 0, "getfsstat returned {count}");

    let mut stats: Vec<libc::statfs> = Vec::with_capacity(count as usize);
    let buf_size = count as i32 * std::mem::size_of::<libc::statfs>() as i32;
    let actual = unsafe { libc::getfsstat(stats.as_mut_ptr(), buf_size, libc::MNT_NOWAIT) };
    assert!(actual > 0);
    unsafe { stats.set_len(actual as usize) };

    let mut found_any = false;
    for fs in &stats {
        let fstype =
            unsafe { std::ffi::CStr::from_ptr(fs.f_fstypename.as_ptr()) }.to_string_lossy();
        if !matches!(
            fstype.as_ref(),
            "apfs" | "hfs" | "msdos" | "exfat" | "ufs" | "zfs"
        ) {
            continue;
        }

        let device =
            unsafe { std::ffi::CStr::from_ptr(fs.f_mntfromname.as_ptr()) }.to_string_lossy();
        let mount = unsafe { std::ffi::CStr::from_ptr(fs.f_mntonname.as_ptr()) }.to_string_lossy();

        let c_path = CString::new(mount.as_bytes()).unwrap();
        let mut sv: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), sv.as_mut_ptr()) };
        if ret != 0 {
            continue;
        }
        let sv = unsafe { sv.assume_init() };

        let bs = sv.f_frsize as f64;
        let total = sv.f_blocks as f64 * bs;
        let free = sv.f_bfree as f64 * bs;
        if total == 0.0 {
            continue;
        }

        found_any = true;
        eprintln!(
            "disk {mount} ({fstype}, {device}): total={:.0}GB used={:.0}GB free={:.0}GB ({:.0}%)",
            total / 1e9,
            (total - free) / 1e9,
            free / 1e9,
            (total - free) / total * 100.0,
        );
    }

    assert!(found_any, "no APFS/HFS mounts found");
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1}GB", b as f64 / 1e9)
    } else if b >= 1_000_000 {
        format!("{:.1}MB", b as f64 / 1e6)
    } else if b >= 1_000 {
        format!("{:.1}KB", b as f64 / 1e3)
    } else {
        format!("{b}B")
    }
}
