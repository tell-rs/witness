use crate::config::{AgentConfig, DeviceFilter, FilterConfig, LogSource};
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
    assert!(cfg.parse_syslog);
    // Collectors default ON on Linux, OFF on macOS (log-forwarder-first).
    let want = cfg!(target_os = "linux");
    assert_eq!(cfg.system.cpu, want);
    assert_eq!(cfg.system.memory, want);
    assert_eq!(cfg.system.load, want);
    assert_eq!(cfg.system.disk, want);
    assert_eq!(cfg.system.network, want);
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

#[test]
fn parse_syslog_default_true() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.parse_syslog);
}

#[test]
fn parse_syslog_explicit_false() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
parse_syslog = false
"#,
    );
    assert!(!cfg.parse_syslog);
}

#[test]
fn parse_structured_and_detect_levels_default_true() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.parse_structured);
    assert!(cfg.detect_levels);
}

#[test]
fn parse_structured_and_detect_levels_explicit_false() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
parse_structured = false
detect_levels = false
"#,
    );
    assert!(!cfg.parse_structured);
    assert!(!cfg.detect_levels);
}

// --- LogSource ---

#[test]
fn log_source_default_auto() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert_eq!(cfg.log_source, LogSource::Auto);
}

#[test]
fn log_source_journald() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "journald"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::Journald);
}

#[test]
fn log_source_files() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "files"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::Files);
}

#[test]
fn log_source_invalid_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "journal"
"#,
    );
    assert!(
        result.is_err(),
        "typo 'journal' should fail, not silently become auto"
    );
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
    // Platform-gated: ON on Linux, OFF on macOS.
    let want = cfg!(target_os = "linux");
    assert_eq!(sys.cpu, want);
    assert_eq!(sys.memory, want);
    assert_eq!(sys.load, want);
    assert_eq!(sys.disk, want);
    assert_eq!(sys.network, want);
    assert_eq!(sys.tcp, want);
    assert_eq!(sys.cgroups, want);
    assert_eq!(sys.containers, want);
    assert_eq!(sys.processes, want);
    // Non-collector fields keep their defaults regardless of platform.
    assert_eq!(sys.process_top, 10);
}

/// The field-level serde default and the `Default` impl must agree (spec 003 R1).
#[test]
fn system_config_default_impl_matches_serde() {
    let parsed = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#).system;
    let defaulted = crate::config::SystemConfig::default();
    assert_eq!(parsed.cpu, defaulted.cpu);
    assert_eq!(parsed.memory, defaulted.memory);
    assert_eq!(parsed.load, defaulted.load);
    assert_eq!(parsed.disk, defaulted.disk);
    assert_eq!(parsed.network, defaulted.network);
    assert_eq!(parsed.tcp, defaulted.tcp);
    assert_eq!(parsed.cgroups, defaulted.cgroups);
    assert_eq!(parsed.containers, defaulted.containers);
    assert_eq!(parsed.processes, defaulted.processes);
}

/// Explicit opt-in works and the format is identical to Linux: `cpu = true`
/// under `[system]` enables cpu; on macOS the rest stay disabled.
#[test]
fn system_config_explicit_opt_in() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system]
cpu = true
"#,
    );
    assert!(cfg.system.cpu);
    // On macOS the omitted collectors default OFF; on Linux they default ON.
    let want = cfg!(target_os = "linux");
    assert_eq!(cfg.system.memory, want);
    assert_eq!(cfg.system.disk, want);
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
    assert!(!cfg.system.processes);
    // Untouched fields keep the platform default.
    assert_eq!(cfg.system.load, cfg!(target_os = "linux"));
    assert_eq!(cfg.system.process_top, 5);
}

// --- UnifiedLog source + predicate (spec 001 R7 / R5, spec 003) ---

#[test]
fn log_source_unifiedlog() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "unifiedlog"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::UnifiedLog);
}

#[test]
fn log_source_unified_alias() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "unified"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::UnifiedLog);
}

#[test]
fn unified_log_predicate_default_none() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.unified_log_predicate.is_none());
}

#[test]
fn unified_log_predicate_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
unified_log_predicate = "messageType == \"fault\""
"#,
    );
    assert_eq!(
        cfg.unified_log_predicate.as_deref(),
        Some("messageType == \"fault\"")
    );
}

#[test]
fn log_source_unified_log_underscore_variant_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "unified_log"
"#,
    );
    assert!(
        result.is_err(),
        "only 'unifiedlog' and its 'unified' alias are accepted, not 'unified_log'"
    );
}

#[test]
fn log_source_wrong_case_fails() {
    let result: Result<AgentConfig, _> = toml::from_str(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "UnifiedLog"
"#,
    );
    assert!(
        result.is_err(),
        "log_source matching is case-sensitive lowercase, not Title/Pascal case"
    );
}

/// Every collector explicitly set `true` under `[system]` must be honored on
/// both platforms (spec 003 R1: explicit opt-in overrides the macOS-off
/// default; a no-op override on Linux where the default is already on).
#[test]
fn system_config_all_collectors_explicit_true_regardless_of_platform() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system]
cpu = true
memory = true
load = true
disk = true
network = true
tcp = true
cgroups = true
containers = true
processes = true
"#,
    );
    assert!(cfg.system.cpu);
    assert!(cfg.system.memory);
    assert!(cfg.system.load);
    assert!(cfg.system.disk);
    assert!(cfg.system.network);
    assert!(cfg.system.tcp);
    assert!(cfg.system.cgroups);
    assert!(cfg.system.containers);
    assert!(cfg.system.processes);
}

/// Every collector explicitly set `false` under `[system]` must be honored on
/// both platforms (spec 003 R1 acceptance: "On Linux, `[system]\ncpu = false`
/// yields cpu disabled", extended here to every collector field).
#[test]
fn system_config_all_collectors_explicit_false_regardless_of_platform() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"

[system]
cpu = false
memory = false
load = false
disk = false
network = false
tcp = false
cgroups = false
containers = false
processes = false
"#,
    );
    assert!(!cfg.system.cpu);
    assert!(!cfg.system.memory);
    assert!(!cfg.system.load);
    assert!(!cfg.system.disk);
    assert!(!cfg.system.network);
    assert!(!cfg.system.tcp);
    assert!(!cfg.system.cgroups);
    assert!(!cfg.system.containers);
    assert!(!cfg.system.processes);
}

// --- Windows Event Log source + channels + query (spec 004 R6) ---

#[test]
fn log_source_eventlog() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "eventlog"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::EventLog);
}

#[test]
fn log_source_eventlog_underscore_alias() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
log_source = "event_log"
"#,
    );
    assert_eq!(cfg.log_source, LogSource::EventLog);
}

#[test]
fn eventlog_channels_default_three() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert_eq!(
        cfg.eventlog_channels,
        vec!["System", "Application", "Security"]
    );
}

#[test]
fn eventlog_query_default_none() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.eventlog_query.is_none());
}

#[test]
fn eventlog_channels_and_query_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
eventlog_channels = ["System", "Setup"]
eventlog_query = "*[System[(Level=2)]]"
"#,
    );
    assert_eq!(cfg.eventlog_channels, vec!["System", "Setup"]);
    assert_eq!(cfg.eventlog_query.as_deref(), Some("*[System[(Level=2)]]"));
}

// --- Event Log + journald filter knobs (severity/filtering follow-up) ---

#[test]
fn eventlog_filter_knobs_default_empty() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.eventlog_event_ids.is_none());
    assert!(cfg.eventlog_exclude_providers.is_empty());
}

#[test]
fn eventlog_filter_knobs_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
eventlog_event_ids = "4624,4625,4700-4800,-4735"
eventlog_exclude_providers = ["Microsoft-Windows-WFP", "VSS"]
"#,
    );
    assert_eq!(
        cfg.eventlog_event_ids.as_deref(),
        Some("4624,4625,4700-4800,-4735")
    );
    assert_eq!(
        cfg.eventlog_exclude_providers,
        vec!["Microsoft-Windows-WFP", "VSS"]
    );
}

#[test]
fn eventlog_event_ids_invalid_is_config_error() {
    // A malformed spec is rejected at load, not silently skipped (spec 004 R6).
    let toml = r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
eventlog_event_ids = "4624,not-a-number"
"#;
    let err = crate::config::parse_config(toml).expect_err("should reject invalid spec");
    assert!(err.to_string().contains("eventlog_event_ids"));
}

#[test]
fn eventlog_event_ids_valid_passes_config_validation() {
    let toml = r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
eventlog_event_ids = "4624,-4735,4700-4800"
"#;
    assert!(crate::config::parse_config(toml).is_ok());
}

#[test]
fn journal_service_filters_default_empty() {
    let cfg = parse(r#"api_key = "aaaa1111bbbb2222cccc3333dddd4444""#);
    assert!(cfg.journal_include_services.is_empty());
    assert!(cfg.journal_exclude_services.is_empty());
}

#[test]
fn journal_service_filters_from_toml() {
    let cfg = parse(
        r#"
api_key = "aaaa1111bbbb2222cccc3333dddd4444"
journal_include_services = ["sshd", "nginx"]
journal_exclude_services = ["cron"]
"#,
    );
    assert_eq!(cfg.journal_include_services, vec!["sshd", "nginx"]);
    assert_eq!(cfg.journal_exclude_services, vec!["cron"]);
}

// --- program_data_state_dir (spec 005 R4) ---

#[test]
fn program_data_state_dir_uses_env_when_set() {
    let dir = crate::config::program_data_state_dir(Some("D:\\Data".into()));
    assert!(dir.ends_with("witness/state") || dir.ends_with("witness\\state"));
    assert!(dir.starts_with("D:\\Data"));
}

#[test]
fn program_data_state_dir_falls_back_when_unset() {
    let dir = crate::config::program_data_state_dir(None);
    assert!(dir.starts_with("C:\\ProgramData"));
    assert!(dir.contains("witness"));
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

// --- Remote config fields (spec 007 R1) ---

#[test]
fn remote_config_defaults() {
    let cfg = parse(r#"api_key = "abcd1234abcd1234abcd1234abcd1234""#);
    assert!(cfg.server.is_none(), "no server → remote config off");
    // Default poll interval is 5 minutes.
    assert_eq!(cfg.config_poll_interval, Duration::from_secs(300));
}

#[test]
fn remote_config_server_and_interval_parse() {
    let cfg = parse(
        r#"
api_key = "abcd1234abcd1234abcd1234abcd1234"
server = "https://tell.example"
config_poll_interval = "30s"
"#,
    );
    assert_eq!(cfg.server.as_deref(), Some("https://tell.example"));
    assert_eq!(cfg.config_poll_interval, Duration::from_secs(30));
}

#[test]
fn remote_config_interval_zero_disables() {
    let cfg = parse(
        r#"
api_key = "abcd1234abcd1234abcd1234abcd1234"
server = "https://tell.example"
config_poll_interval = "0s"
"#,
    );
    assert!(cfg.config_poll_interval.is_zero());
}

// --- Multiline fields (spec 008 R1) ---

#[test]
fn multiline_defaults() {
    let cfg = parse(r#"api_key = "abcd1234abcd1234abcd1234abcd1234""#);
    assert!(cfg.multiline_start_pattern.is_none(), "off by default");
    assert_eq!(cfg.multiline_timeout_ms, 1000);
    assert_eq!(cfg.multiline_max_bytes, 1024 * 1024);
}

#[test]
fn multiline_valid_pattern_parses() {
    let cfg = crate::config::parse_config(
        r#"
api_key = "abcd1234abcd1234abcd1234abcd1234"
multiline_start_pattern = "^\\d{4}-\\d{2}-\\d{2}"
multiline_timeout_ms = 2000
"#,
    )
    .expect("valid pattern compiles");
    assert_eq!(
        cfg.multiline_start_pattern.as_deref(),
        Some(r"^\d{4}-\d{2}-\d{2}")
    );
    assert_eq!(cfg.multiline_timeout_ms, 2000);
}

#[test]
fn multiline_invalid_pattern_is_startup_error() {
    // An unclosed character class fails to compile → parse_config rejects it at
    // startup (spec 008 R1), like eventlog_event_ids.
    let err = crate::config::parse_config(
        r#"
api_key = "abcd1234abcd1234abcd1234abcd1234"
multiline_start_pattern = "["
"#,
    );
    assert!(err.is_err(), "invalid regex must be a startup config error");
}
