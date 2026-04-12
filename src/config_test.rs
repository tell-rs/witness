use crate::config::{AgentConfig, DeviceFilter, FilterConfig};
use std::time::Duration;

// --- TOML parsing ---

fn parse(toml: &str) -> AgentConfig {
    toml::from_str(toml).expect("valid TOML")
}

#[test]
fn minimal_config() {
    let cfg = parse(r#"api_key = "abcd1234abcd1234abcd1234abcd1234""#);
    assert_eq!(cfg.api_key, "abcd1234abcd1234abcd1234abcd1234");
    assert_eq!(cfg.endpoint, "localhost:50000");
    assert!(cfg.hostname.is_empty());
    assert_eq!(cfg.interval, Duration::from_secs(15));
    assert!(cfg.tags.is_empty());
    assert!(!cfg.logs.is_empty(), "should have default log paths");
    assert!(cfg.system.cpu);
    assert!(cfg.system.memory);
    assert!(cfg.system.load);
    assert!(cfg.system.disk);
    assert!(cfg.system.network);
}

#[test]
fn full_config() {
    let cfg = parse(
        r#"
api_key = "feed1e11feed1e11feed1e11feed1e11"
endpoint = "collector.example.com:9000"
hostname = "web-01"
interval = "30s"
logs = ["/var/log/app.log", "/var/log/syslog"]

[tags]
env = "production"
region = "us-east-1"

[system]
cpu = true
memory = true
load = false
disk = false
network = true
"#,
    );
    assert_eq!(cfg.endpoint, "collector.example.com:9000");
    assert_eq!(cfg.hostname, "web-01");
    assert_eq!(cfg.interval, Duration::from_secs(30));
    assert_eq!(cfg.tags.get("env").unwrap(), "production");
    assert_eq!(cfg.tags.get("region").unwrap(), "us-east-1");
    assert_eq!(cfg.logs.len(), 2);
    assert!(cfg.system.cpu);
    assert!(!cfg.system.load);
    assert!(!cfg.system.disk);
}

#[test]
fn missing_api_key_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(r#"endpoint = "localhost:50000""#);
    assert!(result.is_err());
}

// --- Duration parsing ---

#[test]
fn interval_milliseconds() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "500ms"
"#,
    );
    assert_eq!(cfg.interval, Duration::from_millis(500));
}

#[test]
fn interval_seconds() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "10s"
"#,
    );
    assert_eq!(cfg.interval, Duration::from_secs(10));
}

#[test]
fn interval_minutes() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "2m"
"#,
    );
    assert_eq!(cfg.interval, Duration::from_secs(120));
}

#[test]
fn interval_hours() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "1h"
"#,
    );
    assert_eq!(cfg.interval, Duration::from_secs(3600));
}

#[test]
fn interval_bad_suffix_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "10d"
"#,
    );
    assert!(result.is_err());
}

#[test]
fn interval_empty_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = ""
"#,
    );
    assert!(result.is_err());
}

#[test]
fn interval_not_a_number_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
interval = "abcs"
"#,
    );
    assert!(result.is_err());
}

// --- DeviceFilter ---

#[test]
fn filter_allows_all_when_empty() {
    let cfg = FilterConfig::default();
    let filter = DeviceFilter::new(&cfg, &[]);
    assert!(filter.allows("eth0"));
    assert!(filter.allows("sda"));
    assert!(filter.allows("anything"));
}

#[test]
fn filter_include_only() {
    let cfg = FilterConfig {
        include: vec!["eth*".to_string()],
        exclude: vec![],
    };
    let filter = DeviceFilter::new(&cfg, &[]);
    assert!(filter.allows("eth0"));
    assert!(filter.allows("eth1"));
    assert!(!filter.allows("lo"));
    assert!(!filter.allows("wlan0"));
}

#[test]
fn filter_exclude_only() {
    let cfg = FilterConfig {
        include: vec![],
        exclude: vec!["lo*".to_string()],
    };
    let filter = DeviceFilter::new(&cfg, &[]);
    assert!(filter.allows("eth0"));
    assert!(!filter.allows("lo"));
    assert!(!filter.allows("lo0"));
}

#[test]
fn filter_include_and_exclude() {
    let cfg = FilterConfig {
        include: vec!["eth*".to_string()],
        exclude: vec!["eth1".to_string()],
    };
    let filter = DeviceFilter::new(&cfg, &[]);
    assert!(filter.allows("eth0"));
    assert!(!filter.allows("eth1"));
    assert!(!filter.allows("wlan0"));
}

#[test]
fn filter_default_exclude_used_when_config_empty() {
    let cfg = FilterConfig::default();
    let filter = DeviceFilter::new(&cfg, &["lo*", "veth*"]);
    assert!(filter.allows("eth0"));
    assert!(!filter.allows("lo"));
    assert!(!filter.allows("lo0"));
    assert!(!filter.allows("veth123"));
}

#[test]
fn filter_config_exclude_overrides_defaults() {
    let cfg = FilterConfig {
        include: vec![],
        exclude: vec!["br*".to_string()],
    };
    let filter = DeviceFilter::new(&cfg, &["lo*", "veth*"]);
    // Config exclude replaces defaults, so lo is allowed now
    assert!(filter.allows("lo"));
    assert!(!filter.allows("br0"));
}

// --- SystemConfig defaults ---

#[test]
fn system_config_defaults() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    let sys = &cfg.system;
    assert!(sys.cpu);
    assert!(sys.memory);
    assert!(sys.load);
    assert!(sys.disk);
    assert!(sys.network);
    assert!(sys.tcp);
    assert!(sys.cgroups);
    assert!(sys.containers);
    assert!(sys.processes);
    assert_eq!(sys.process_top, 10);
}

#[test]
fn system_config_selective_disable() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system]
cpu = false
memory = false
processes = false
process_top = 5
"#,
    );
    assert!(!cfg.system.cpu);
    assert!(!cfg.system.memory);
    assert!(cfg.system.load);
    assert!(!cfg.system.processes);
    assert_eq!(cfg.system.process_top, 5);
}

// --- resolve_hostname ---

#[test]
fn resolve_hostname_returns_configured_value() {
    let result = crate::config::resolve_hostname("my-host");
    assert_eq!(result, "my-host");
}

#[test]
fn resolve_hostname_empty_falls_back() {
    let result = crate::config::resolve_hostname("");
    // Should return something (either /etc/hostname, gethostname(), or "unknown")
    assert!(!result.is_empty());
}

// --- Network / disk filter configs from TOML ---

#[test]
fn network_filter_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system.network_filter]
include = ["en0"]
exclude = ["lo*"]
"#,
    );
    assert_eq!(cfg.system.network_filter.include, vec!["en0"]);
    assert_eq!(cfg.system.network_filter.exclude, vec!["lo*"]);
}

#[test]
fn disk_filter_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system.disk_filter]
include = ["disk*"]
"#,
    );
    assert_eq!(cfg.system.disk_filter.include, vec!["disk*"]);
    assert!(cfg.system.disk_filter.exclude.is_empty());
}

// --- load_config ---

#[test]
fn load_config_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
api_key = "feed1e11feed1e11feed1e11feed1e11"
endpoint = "collector:9000"
interval = "10s"
"#,
    )
    .unwrap();

    let cfg = crate::config::load_config(&path).unwrap();
    assert_eq!(cfg.api_key, "feed1e11feed1e11feed1e11feed1e11");
    assert_eq!(cfg.endpoint, "collector:9000");
    assert_eq!(cfg.interval, std::time::Duration::from_secs(10));
}

#[test]
fn load_config_missing_file_fails() {
    let path = std::path::PathBuf::from("/tmp/tell_test_nonexistent_config.toml");
    assert!(crate::config::load_config(&path).is_err());
}

#[test]
fn load_config_invalid_toml_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "not valid toml {{{{").unwrap();
    assert!(crate::config::load_config(&path).is_err());
}
