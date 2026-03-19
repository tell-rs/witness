use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Agent configuration.
///
/// Minimal config: just `api_key`. Everything else has sensible defaults.
///
/// ```toml
/// api_key = "feed1e11feed1e11feed1e11feed1e11"
/// ```
#[derive(Debug, Deserialize)]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub struct AgentConfig {
    /// API key for authentication (32 hex chars, required).
    pub api_key: String,

    /// Collector endpoint.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Hostname override. Auto-detected if empty or absent.
    #[serde(default)]
    pub hostname: String,

    /// Collection interval for all system metrics.
    #[serde(
        default = "default_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Duration,

    /// Global tags applied to every metric and log.
    #[serde(default)]
    pub tags: HashMap<String, String>,

    /// Log files to tail (glob patterns supported).
    /// Defaults to common system and service log paths. Set to `[]` to disable.
    #[serde(default = "default_logs")]
    pub logs: Vec<String>,

    /// Batch size — number of data points per TCP flush. Default: 500.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Maximum disk buffer size in bytes. Default: 3 GiB.
    /// The disk buffer persists unsent data during network outages.
    /// Oldest data is evicted when this limit is reached.
    #[serde(default)]
    pub buffer_max_bytes: Option<u64>,

    /// System monitoring configuration. All collectors enabled by default.
    #[serde(default)]
    pub system: SystemConfig,
}

/// System monitoring configuration.
///
/// All collectors are enabled by default. Set to `false` to disable.
/// Sub-tables (`[system.network]`, `[system.disk]`) configure filtering.
#[derive(Debug, Deserialize)]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub struct SystemConfig {
    /// Enable/disable CPU collector.
    #[serde(default = "default_true")]
    pub cpu: bool,

    /// Enable/disable memory collector.
    #[serde(default = "default_true")]
    pub memory: bool,

    /// Enable/disable load average collector.
    #[serde(default = "default_true")]
    pub load: bool,

    /// Enable/disable disk I/O + space collector.
    #[serde(default = "default_true")]
    pub disk: bool,

    /// Enable/disable network collector.
    #[serde(default = "default_true")]
    pub network: bool,

    /// Enable/disable TCP connection state collector.
    #[serde(default = "default_true")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub tcp: bool,

    /// Enable/disable cgroups v2 collector.
    #[serde(default = "default_true")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub cgroups: bool,

    /// Enable/disable per-container cgroup collector.
    #[serde(default = "default_true")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub containers: bool,

    /// Enable/disable process collector.
    #[serde(default = "default_true")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub processes: bool,

    /// Top N processes to report by CPU/memory.
    #[serde(default = "default_top_n")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub process_top: usize,

    /// Network interface filtering.
    #[serde(default)]
    pub network_filter: FilterConfig,

    /// Disk device filtering.
    #[serde(default)]
    pub disk_filter: FilterConfig,
}

/// Include/exclude glob filter.
#[derive(Debug, Default, Deserialize)]
pub struct FilterConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            cpu: true,
            memory: true,
            load: true,
            disk: true,
            network: true,
            tcp: true,
            cgroups: true,
            containers: true,
            processes: true,
            process_top: default_top_n(),
            network_filter: FilterConfig::default(),
            disk_filter: FilterConfig::default(),
        }
    }
}

fn default_endpoint() -> String {
    "localhost:50000".to_string()
}

fn default_interval() -> Duration {
    Duration::from_secs(15)
}

fn default_true() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn default_logs() -> Vec<String> {
    vec![
        // System
        "/var/log/syslog".into(),
        "/var/log/auth.log".into(),
        "/var/log/kern.log".into(),
        // Web servers
        "/var/log/nginx/*.log".into(),
        "/var/log/apache2/*.log".into(),
        // Databases
        "/var/log/postgresql/*.log".into(),
        "/var/log/mysql/*.log".into(),
        "/var/log/mongodb/*.log".into(),
        "/var/log/redis/*.log".into(),
        // Infrastructure
        "/var/log/haproxy/*.log".into(),
        "/var/log/traefik/*.log".into(),
        "/var/log/elasticsearch/*.log".into(),
        "/var/log/rabbitmq/*.log".into(),
    ]
}

#[cfg(target_os = "macos")]
fn default_logs() -> Vec<String> {
    vec!["/var/log/system.log".into()]
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn default_logs() -> Vec<String> {
    vec![]
}

fn default_batch_size() -> usize {
    500
}

fn default_top_n() -> usize {
    10
}

// --- Duration parsing ---

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".to_string());
    }

    if let Some(n) = s.strip_suffix("ms") {
        let v: u64 = n
            .parse()
            .map_err(|_| format!("invalid number in duration '{s}'"))?;
        return Ok(Duration::from_millis(v));
    }
    if let Some(n) = s.strip_suffix('s') {
        let v: u64 = n
            .parse()
            .map_err(|_| format!("invalid number in duration '{s}'"))?;
        return Ok(Duration::from_secs(v));
    }
    if let Some(n) = s.strip_suffix('m') {
        let v: u64 = n
            .parse()
            .map_err(|_| format!("invalid number in duration '{s}'"))?;
        return Ok(Duration::from_secs(v * 60));
    }
    if let Some(n) = s.strip_suffix('h') {
        let v: u64 = n
            .parse()
            .map_err(|_| format!("invalid number in duration '{s}'"))?;
        return Ok(Duration::from_secs(v * 3600));
    }

    Err(format!(
        "unknown duration suffix in '{s}', expected ms/s/m/h"
    ))
}

// --- Device filter ---

/// Glob-based device/interface filter.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub struct DeviceFilter {
    include: Vec<glob::Pattern>,
    exclude: Vec<glob::Pattern>,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
impl DeviceFilter {
    pub fn new(config: &FilterConfig, default_exclude: &[&str]) -> Self {
        let exclude_patterns: Vec<String> = if config.exclude.is_empty() {
            default_exclude.iter().map(|s| (*s).to_string()).collect()
        } else {
            config.exclude.clone()
        };

        Self {
            include: config
                .include
                .iter()
                .filter_map(|s| glob::Pattern::new(s).ok())
                .collect(),
            exclude: exclude_patterns
                .iter()
                .filter_map(|s| glob::Pattern::new(s).ok())
                .collect(),
        }
    }

    pub fn allows(&self, name: &str) -> bool {
        let included = self.include.is_empty() || self.include.iter().any(|p| p.matches(name));
        let excluded = self.exclude.iter().any(|p| p.matches(name));
        included && !excluded
    }
}

// --- Config loading ---

pub fn load_config(path: &PathBuf) -> Result<AgentConfig, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let config: AgentConfig = toml::from_str(&contents)?;
    validate_api_key(&config.api_key)?;
    Ok(config)
}

fn validate_api_key(key: &str) -> Result<(), Box<dyn std::error::Error>> {
    if key.is_empty() {
        return Err("api_key is required".into());
    }
    if key.len() != 32 {
        return Err(format!("api_key must be 32 hex characters, got {}", key.len()).into());
    }
    if !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("api_key must contain only hex characters (0-9, a-f)".into());
    }
    Ok(())
}

/// Platform-appropriate state directory.
/// Linux: /var/lib/witness (systemd StateDirectory=witness creates this)
/// macOS: ~/Library/Application Support/witness
pub fn state_dir() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        static DIR: OnceLock<String> = OnceLock::new();
        DIR.get_or_init(|| {
            if let Some(home) = std::env::var_os("HOME") {
                let p = std::path::Path::new(&home).join("Library/Application Support/witness");
                p.to_string_lossy().into_owned()
            } else {
                "/tmp/witness".to_string()
            }
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        "/var/lib/witness"
    }
}

/// Auto-detect hostname if not configured.
pub fn resolve_hostname(configured: &str) -> String {
    if !configured.is_empty() {
        return configured.to_string();
    }

    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = h.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if ret == 0
            && let Some(end) = buf.iter().position(|&b| b == 0)
            && let Ok(s) = std::str::from_utf8(&buf[..end])
            && !s.is_empty()
        {
            return s.to_string();
        }
    }

    "unknown".to_string()
}
