//! Container cgroup collector — enumerates per-container cgroups under /sys/fs/cgroup.
//!
//! Detects Docker, containerd (Kubernetes), CRI-O, and Podman containers
//! by scanning well-known cgroup v2 slice directories.
//!
//! Emits counters (delta): system.container.cpu.usage_usec, .user_usec, .system_usec
//! Emits gauges: system.container.memory.usage, .limit, .anon, .file
//! Labels: {container_id, runtime}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::collectors::{Collector, read_procfs};
use crate::sink::Sink;

/// Per-container metadata discovered from the cgroup filesystem.
pub(crate) struct ContainerInfo {
    /// 12-char truncated container ID (matches `docker ps` format).
    pub(crate) short_id: String,
    /// Runtime that owns this container.
    pub(crate) runtime: &'static str,
    /// Absolute path to the container's cgroup directory.
    pub(crate) cgroup_path: PathBuf,
}

/// Previous CPU counter values for delta calculation.
#[derive(Clone, Default)]
pub(crate) struct CpuPrev {
    pub(crate) usage_usec: u64,
    pub(crate) user_usec: u64,
    pub(crate) system_usec: u64,
}

pub struct ContainerCollector {
    /// Keyed by full container ID (64 hex chars).
    prev_cpu: HashMap<String, CpuPrev>,
}

impl ContainerCollector {
    pub fn new() -> Self {
        Self {
            prev_cpu: HashMap::new(),
        }
    }
}

impl Collector for ContainerCollector {
    fn name(&self) -> &'static str {
        "container"
    }

    fn collect(&mut self, sink: &Sink, _hostname: &str, buf: &mut String) {
        let containers = discover_containers();

        let mut seen_ids: Vec<&str> = Vec::with_capacity(containers.len());

        for c in &containers {
            seen_ids.push(&c.short_id);
            let labels: &[(&'static str, &str)] =
                &[("container_id", &c.short_id), ("runtime", c.runtime)];

            collect_cpu(
                sink,
                &c.cgroup_path,
                &c.short_id,
                labels,
                &mut self.prev_cpu,
                buf,
            );
            collect_memory(sink, &c.cgroup_path, labels, buf);
        }

        // Prune state for containers that no longer exist.
        self.prev_cpu
            .retain(|id, _| seen_ids.contains(&id.as_str()));
    }
}

/// Collect CPU delta counters from `cpu.stat`.
fn collect_cpu(
    sink: &Sink,
    cgroup: &Path,
    short_id: &str,
    labels: &[(&'static str, &str)],
    prev_map: &mut HashMap<String, CpuPrev>,
    buf: &mut String,
) {
    let path = cgroup.join("cpu.stat");
    let path_str = path.to_string_lossy();
    if read_procfs(&path_str, buf).is_err() {
        return;
    }

    let current = parse_cpu_stat(buf);

    if let Some(prev) = prev_map.get(short_id) {
        let d = |c: u64, p: u64| c.saturating_sub(p) as f64;
        sink.counter_dyn(
            "system.container.cpu.usage_usec",
            d(current.usage_usec, prev.usage_usec),
            labels,
        );
        sink.counter_dyn(
            "system.container.cpu.user_usec",
            d(current.user_usec, prev.user_usec),
            labels,
        );
        sink.counter_dyn(
            "system.container.cpu.system_usec",
            d(current.system_usec, prev.system_usec),
            labels,
        );
    }

    prev_map.insert(short_id.to_string(), current);
}

/// Collect memory gauges from `memory.current`, `memory.max`, and `memory.stat`.
fn collect_memory(sink: &Sink, cgroup: &Path, labels: &[(&'static str, &str)], buf: &mut String) {
    collect_memory_current(sink, cgroup, labels, buf);
    collect_memory_limit(sink, cgroup, labels, buf);
    collect_memory_stat(sink, cgroup, labels, buf);
}

fn collect_memory_current(
    sink: &Sink,
    cgroup: &Path,
    labels: &[(&'static str, &str)],
    buf: &mut String,
) {
    let path = cgroup.join("memory.current");
    let path_str = path.to_string_lossy();
    if read_procfs(&path_str, buf).is_err() {
        return;
    }
    if let Ok(bytes) = buf.trim().parse::<f64>() {
        sink.gauge_dyn("system.container.memory.usage", bytes, labels);
    }
}

fn collect_memory_limit(
    sink: &Sink,
    cgroup: &Path,
    labels: &[(&'static str, &str)],
    buf: &mut String,
) {
    let path = cgroup.join("memory.max");
    let path_str = path.to_string_lossy();
    if read_procfs(&path_str, buf).is_err() {
        return;
    }
    let trimmed = buf.trim();
    if trimmed != "max"
        && let Ok(bytes) = trimmed.parse::<f64>()
    {
        sink.gauge_dyn("system.container.memory.limit", bytes, labels);
    }
}

fn collect_memory_stat(
    sink: &Sink,
    cgroup: &Path,
    labels: &[(&'static str, &str)],
    buf: &mut String,
) {
    let path = cgroup.join("memory.stat");
    let path_str = path.to_string_lossy();
    if read_procfs(&path_str, buf).is_err() {
        return;
    }
    for line in buf.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(val) = parts.next().and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        match key {
            "anon" => sink.gauge_dyn("system.container.memory.anon", val, labels),
            "file" => sink.gauge_dyn("system.container.memory.file", val, labels),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Container discovery
// ---------------------------------------------------------------------------

/// Scan well-known cgroup v2 paths for container scopes.
fn discover_containers() -> Vec<ContainerInfo> {
    let mut containers = Vec::new();

    // Docker: /sys/fs/cgroup/system.slice/docker-{id}.scope
    scan_scope_dir(
        "/sys/fs/cgroup/system.slice",
        "docker-",
        "docker",
        &mut containers,
    );

    // Kubernetes (containerd + CRI-O): walk kubepods slices
    scan_kubepods(&mut containers);

    // Podman: /sys/fs/cgroup/user.slice/**/libpod-{id}.scope
    scan_podman(&mut containers);

    containers
}

/// Scan a directory for entries matching `{prefix}{64-hex}.scope`.
fn scan_scope_dir(dir: &str, prefix: &str, runtime: &'static str, out: &mut Vec<ContainerInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(info) = parse_scope_entry(&name_str, prefix, runtime, entry.path()) {
            out.push(info);
        }
    }
}

/// Try to parse a single directory entry as `{prefix}{64-hex-id}.scope`.
pub(crate) fn parse_scope_entry(
    name: &str,
    prefix: &str,
    runtime: &'static str,
    path: PathBuf,
) -> Option<ContainerInfo> {
    let inner = name.strip_prefix(prefix)?.strip_suffix(".scope")?;
    if inner.len() == 64 && inner.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(ContainerInfo {
            short_id: inner[..12].to_string(),
            runtime,
            cgroup_path: path,
        })
    } else {
        None
    }
}

/// Walk Kubernetes kubepods slice hierarchy for containerd and CRI-O scopes.
///
/// Structure:
///   /sys/fs/cgroup/kubepods.slice/
///     kubepods-burstable.slice/kubepods-burstable-pod{uid}.slice/cri-containerd-{id}.scope
///     kubepods-besteffort.slice/kubepods-besteffort-pod{uid}.slice/crio-{id}.scope
///     kubepods-pod{uid}.slice/cri-containerd-{id}.scope  (guaranteed QoS)
fn scan_kubepods(out: &mut Vec<ContainerInfo>) {
    let base = "/sys/fs/cgroup/kubepods.slice";
    if !Path::new(base).is_dir() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.contains("-pod") && name_str.ends_with(".slice") {
            // Guaranteed QoS: pod slice directly under kubepods.slice
            scan_container_scopes_in_dir(&path, out);
        } else if name_str.ends_with(".slice") {
            // Burstable/besteffort: one more level of nesting (pod slices inside)
            scan_qos_slice(&path, out);
        }
    }
}

/// Scan a QoS-class slice (burstable/besteffort) for pod slices, then scan
/// each pod slice for container scopes.
fn scan_qos_slice(qos_dir: &Path, out: &mut Vec<ContainerInfo>) {
    let Ok(entries) = std::fs::read_dir(qos_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains("-pod") && name_str.ends_with(".slice") {
            scan_container_scopes_in_dir(&path, out);
        }
    }
}

/// Scan a pod slice directory for `cri-containerd-{id}.scope` and `crio-{id}.scope`.
fn scan_container_scopes_in_dir(pod_dir: &Path, out: &mut Vec<ContainerInfo>) {
    let Ok(entries) = std::fs::read_dir(pod_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if let Some(info) =
            parse_scope_entry(&name_str, "cri-containerd-", "containerd", entry.path())
        {
            out.push(info);
        } else if let Some(info) = parse_scope_entry(&name_str, "crio-", "crio", entry.path()) {
            out.push(info);
        }
    }
}

/// Scan Podman container cgroups under user.slice.
///
/// Podman rootless containers typically live at:
///   /sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/.../libpod-{id}.scope
/// We walk up to 4 levels deep to find them without a full recursive walk.
fn scan_podman(out: &mut Vec<ContainerInfo>) {
    let base = "/sys/fs/cgroup/user.slice";
    if !Path::new(base).is_dir() {
        return;
    }
    walk_for_libpod(Path::new(base), 0, out);
}

/// Bounded-depth walk looking for `libpod-{id}.scope` entries.
fn walk_for_libpod(dir: &Path, depth: u8, out: &mut Vec<ContainerInfo>) {
    const MAX_DEPTH: u8 = 5;
    if depth > MAX_DEPTH {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if let Some(info) = parse_scope_entry(&name_str, "libpod-", "podman", entry.path()) {
            out.push(info);
        } else if entry.path().is_dir() && depth < MAX_DEPTH {
            walk_for_libpod(&entry.path(), depth + 1, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Cgroup file parsing
// ---------------------------------------------------------------------------

/// Parse `cpu.stat` into cumulative counter values.
pub(crate) fn parse_cpu_stat(buf: &str) -> CpuPrev {
    let mut stats = CpuPrev::default();
    for line in buf.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(val) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        match key {
            "usage_usec" => stats.usage_usec = val,
            "user_usec" => stats.user_usec = val,
            "system_usec" => stats.system_usec = val,
            _ => {}
        }
    }
    stats
}
