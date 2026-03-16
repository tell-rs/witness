use std::path::PathBuf;

use super::container::*;

// --- parse_scope_entry ---

#[test]
fn parse_docker_scope() {
    let id = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("docker-{id}.scope");
    let info = parse_scope_entry(&name, "docker-", "docker", PathBuf::from("/cgroup")).unwrap();
    assert_eq!(info.short_id, "a1b2c3d4e5f6");
    assert_eq!(info.runtime, "docker");
}

#[test]
fn parse_containerd_scope() {
    let id = "ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00";
    let name = format!("cri-containerd-{id}.scope");
    let info =
        parse_scope_entry(&name, "cri-containerd-", "containerd", PathBuf::from("/x")).unwrap();
    assert_eq!(info.short_id, "ff00ff00ff00");
    assert_eq!(info.runtime, "containerd");
}

#[test]
fn parse_crio_scope() {
    let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let name = format!("crio-{id}.scope");
    let info = parse_scope_entry(&name, "crio-", "crio", PathBuf::from("/x")).unwrap();
    assert_eq!(info.short_id, "0123456789ab");
    assert_eq!(info.runtime, "crio");
}

#[test]
fn parse_podman_scope() {
    let id = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
    let name = format!("libpod-{id}.scope");
    let info = parse_scope_entry(&name, "libpod-", "podman", PathBuf::from("/x")).unwrap();
    assert_eq!(info.short_id, "abcdef123456");
    assert_eq!(info.runtime, "podman");
}

#[test]
fn rejects_wrong_prefix() {
    let id = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("docker-{id}.scope");
    assert!(parse_scope_entry(&name, "podman-", "podman", PathBuf::from("/x")).is_none());
}

#[test]
fn rejects_short_id() {
    let name = "docker-abcdef.scope";
    assert!(parse_scope_entry(name, "docker-", "docker", PathBuf::from("/x")).is_none());
}

#[test]
fn rejects_non_hex_id() {
    let id = "g1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("docker-{id}.scope");
    assert!(parse_scope_entry(&name, "docker-", "docker", PathBuf::from("/x")).is_none());
}

#[test]
fn rejects_missing_scope_suffix() {
    let id = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("docker-{id}.service");
    assert!(parse_scope_entry(&name, "docker-", "docker", PathBuf::from("/x")).is_none());
}

// --- parse_cpu_stat ---

#[test]
fn parse_cpu_stat_normal() {
    let input = "\
usage_usec 123456
user_usec 100000
system_usec 23456
nr_periods 10
nr_throttled 0
throttled_usec 0
";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 123456);
    assert_eq!(stats.user_usec, 100000);
    assert_eq!(stats.system_usec, 23456);
}

#[test]
fn parse_cpu_stat_empty() {
    let stats = parse_cpu_stat("");
    assert_eq!(stats.usage_usec, 0);
    assert_eq!(stats.user_usec, 0);
    assert_eq!(stats.system_usec, 0);
}

#[test]
fn parse_cpu_stat_partial() {
    let input = "usage_usec 500\n";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 500);
    assert_eq!(stats.user_usec, 0);
    assert_eq!(stats.system_usec, 0);
}

#[test]
fn parse_cpu_stat_malformed_lines() {
    let input = "\
usage_usec not_a_number
user_usec
system_usec 999
";
    let stats = parse_cpu_stat(input);
    assert_eq!(stats.usage_usec, 0);
    assert_eq!(stats.user_usec, 0);
    assert_eq!(stats.system_usec, 999);
}

// --- k8s container scope parsing (uses parse_scope_entry with k8s prefixes) ---

#[test]
fn parse_k8s_containerd_scope() {
    let id = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("cri-containerd-{id}.scope");
    let info = parse_scope_entry(
        &name,
        "cri-containerd-",
        "containerd",
        PathBuf::from("/kubepods/pod/ctr"),
    )
    .unwrap();
    assert_eq!(info.runtime, "containerd");
    assert_eq!(info.short_id, "a1b2c3d4e5f6");
}

#[test]
fn parse_k8s_crio_scope() {
    let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let name = format!("crio-{id}.scope");
    let info =
        parse_scope_entry(&name, "crio-", "crio", PathBuf::from("/kubepods/pod/ctr")).unwrap();
    assert_eq!(info.runtime, "crio");
}

#[test]
fn parse_k8s_rejects_unknown_runtime() {
    let id = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let name = format!("unknown-{id}.scope");
    assert!(
        parse_scope_entry(&name, "cri-containerd-", "containerd", PathBuf::from("/x")).is_none()
    );
    assert!(parse_scope_entry(&name, "crio-", "crio", PathBuf::from("/x")).is_none());
}
