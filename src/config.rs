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
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub struct AgentConfig {
    /// API key for authentication (32 hex chars, required).
    pub api_key: String,

    /// Collector endpoint.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Tell control-plane URL for remote config polling (spec 007). When set to
    /// an `https://` URL, the agent periodically GETs `{server}/v1/agent/config`
    /// and applies a changed, valid config via the reload path. Omit to disable
    /// remote config. MUST be `https://` for the poller — it repeatedly carries a
    /// bearer token and returns authority-bearing config (an `http://` value
    /// disables the poller with a warning). The server may change ANY field,
    /// including `api_key` and `endpoint` (it is the root of config authority);
    /// such changes are logged loudly.
    #[serde(default)]
    pub server: Option<String>,

    /// Remote-config poll interval (spec 007). Default `5m`. `0s` disables
    /// polling even when `server` is set. Only meaningful when `server` is set.
    #[serde(
        default = "default_config_poll_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub config_poll_interval: Duration,

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

    /// Log source backend. Default: `auto`.
    /// - `auto` — Linux: journald if available, else file tailing.
    ///   macOS: the unified log if `/usr/bin/log` is present, else file tailing.
    /// - `journald` — journald only (exits if journalctl not available)
    /// - `unifiedlog` (alias `unified`) — macOS unified log only (exits off macOS)
    /// - `files` — file tailing only
    #[serde(default = "default_log_source")]
    pub log_source: LogSource,

    /// Raw `--predicate` for the macOS unified log source. When set, it REPLACES
    /// the built-in default predicate and is passed verbatim to `/usr/bin/log`
    /// as a single argument (never through a shell). Ignored off macOS and when
    /// `log_source` does not resolve to the unified log.
    #[serde(default)]
    pub unified_log_predicate: Option<String>,

    /// Windows Event Log channels to subscribe to (spec 004 R6). Defaults to
    /// `["System", "Application", "Security"]`. The `Security` channel needs
    /// `SeSecurityPrivilege`; on access-denied the pump warns once and skips it,
    /// continuing with the rest. Inert off Windows.
    #[serde(default = "default_eventlog_channels")]
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub eventlog_channels: Vec<String>,

    /// Raw XPath query for the Windows Event Log source. When set, it REPLACES
    /// the built-in default query (Level 0–4) and is passed verbatim to
    /// `EvtSubscribe` for every channel (never through a shell). Inert off
    /// Windows.
    #[serde(default)]
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub eventlog_query: Option<String>,

    /// Windows Event Log event-id filter, Winlogbeat syntax
    /// (`"4624,4625,4700-4800,-4735"`): comma list, `N` / `N-M` include,
    /// `-N` / `-N-M` exclude. If any includes are present an event must match an
    /// include AND no exclude; excludes alone mean "everything except". An
    /// invalid spec is a startup config error. Applied agent-side after render.
    /// Part of the same filter family as `journal_{include,exclude}_services`
    /// (Linux) and `unified_log_predicate` (macOS). Inert off Windows.
    #[serde(default)]
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub eventlog_event_ids: Option<String>,

    /// Windows Event Log providers to drop (case-insensitive exact match on
    /// `Provider Name`), applied agent-side. Inert off Windows.
    #[serde(default)]
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub eventlog_exclude_providers: Vec<String>,

    /// journald services to include (match on the resolved program name —
    /// `SYSLOG_IDENTIFIER` / `_COMM` — case-sensitive exact). Empty = allow all.
    /// Part of the same filter family as `eventlog_event_ids` (Windows) and
    /// `unified_log_predicate` (macOS).
    #[serde(default)]
    pub journal_include_services: Vec<String>,

    /// journald services to drop (match on the resolved program name,
    /// case-sensitive exact). An exclude wins over an include.
    #[serde(default)]
    pub journal_exclude_services: Vec<String>,

    /// Parse syslog lines to extract service name and message body.
    /// Only applies when `log_source` is "files" or "auto" (file tailing path).
    /// Default: true.
    #[serde(default = "default_true")]
    pub parse_syslog: bool,

    /// Extract structured content (JSON or logfmt) from file log lines: the
    /// event phrase becomes the message body and the remaining key/value pairs
    /// ship as a structured payload — the same treatment journald `MESSAGE`
    /// fields receive. Runs after any syslog envelope is stripped. Only applies
    /// on the file-tailing path. Default: true.
    #[serde(default = "default_true")]
    pub parse_structured: bool,

    /// Detect a log level for file log lines. A structured `level`/`severity`/
    /// `lvl` field (from JSON/logfmt, when `parse_structured` is on) is used
    /// first; otherwise a cheap heuristic scans the start of the line for a
    /// delimited level token (` ERROR `, `[error]`, `level=error`). Without
    /// this, every file line ships as `Info`. Only applies on the file-tailing
    /// path. Default: true.
    #[serde(default = "default_true")]
    pub detect_levels: bool,

    /// Multiline record start pattern (spec 008), a regex matched against each
    /// complete physical line. A match marks the start of a new record; every
    /// following non-matching line is appended as a continuation until the next
    /// start-match, the inactivity timeout, or the byte cap. `None` (default)
    /// disables aggregation — every line ships as its own entry. Matched with
    /// "find anywhere in the line" semantics; anchor with `^` for "line begins
    /// with". Compiled and validated at startup (an invalid regex is a config
    /// error). ONLY applies when the resolved log source is file tailing
    /// (`files`, or `auto` resolved to files); inert for the structured sources
    /// (journald / unified log / Event Log), which already deliver whole records.
    #[serde(default)]
    pub multiline_start_pattern: Option<String>,

    /// Multiline inactivity flush timeout in milliseconds (spec 008). A buffered
    /// record with no new bytes for this long is shipped so the trailing record
    /// of an idle file is not held indefinitely. Default `1000`. Only meaningful
    /// when `multiline_start_pattern` is set.
    #[serde(default = "default_multiline_timeout_ms")]
    pub multiline_timeout_ms: u64,

    /// Multiline per-record byte cap (spec 008). A record is truncated at a
    /// char boundary (drop-excess, like the single-line partial cap) once its
    /// joined text exceeds this, bounding memory for a pathological
    /// never-ending record. Default 1 MiB (equal to the single-line cap). Only
    /// meaningful when `multiline_start_pattern` is set.
    #[serde(default = "default_multiline_max_bytes")]
    pub multiline_max_bytes: usize,

    /// Log files to tail (glob patterns supported).
    /// Only applies when `log_source` is "files" or "auto" (file tailing fallback).
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
/// Metric collectors default ON on Linux and OFF on macOS: witness is a
/// log-forwarder-first agent on macOS, where operators opt into metrics by
/// adding a `[system]` table with the collectors they want. The config file
/// FORMAT is byte-for-byte identical across platforms; only the compiled-in
/// default values differ (`default_metrics_enabled`). Set fields to `false` to
/// disable. Sub-tables (`[system.network]`, `[system.disk]`) configure filtering.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub struct SystemConfig {
    /// Enable/disable CPU collector.
    #[serde(default = "default_metrics_enabled")]
    pub cpu: bool,

    /// Enable/disable memory collector.
    #[serde(default = "default_metrics_enabled")]
    pub memory: bool,

    /// Enable/disable load average collector.
    #[serde(default = "default_metrics_enabled")]
    pub load: bool,

    /// Enable/disable disk I/O + space collector.
    #[serde(default = "default_metrics_enabled")]
    pub disk: bool,

    /// Enable/disable network collector.
    #[serde(default = "default_metrics_enabled")]
    pub network: bool,

    /// Enable/disable TCP connection state collector.
    #[serde(default = "default_metrics_enabled")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub tcp: bool,

    /// Enable/disable cgroups v2 collector.
    #[serde(default = "default_metrics_enabled")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub cgroups: bool,

    /// Enable/disable per-container cgroup collector.
    #[serde(default = "default_metrics_enabled")]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub containers: bool,

    /// Enable/disable process collector.
    #[serde(default = "default_metrics_enabled")]
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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FilterConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        // Must agree with the field-level `#[serde(default = ...)]` above:
        // collectors default ON on Linux, OFF on macOS.
        let enabled = default_metrics_enabled();
        Self {
            cpu: enabled,
            memory: enabled,
            load: enabled,
            disk: enabled,
            network: enabled,
            tcp: enabled,
            cgroups: enabled,
            containers: enabled,
            processes: enabled,
            process_top: default_top_n(),
            network_filter: FilterConfig::default(),
            disk_filter: FilterConfig::default(),
        }
    }
}

/// Log source backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    /// Platform default: journald (Linux) or the unified log (macOS) if
    /// available, else file tailing.
    Auto,
    /// Journald only — exits if journalctl is not available.
    Journald,
    /// macOS unified log only (`unifiedlog` / alias `unified`) — exits off macOS.
    #[serde(alias = "unified")]
    UnifiedLog,
    /// Windows Event Log only (`eventlog` / alias `event_log`) — exits off
    /// Windows.
    #[serde(alias = "event_log")]
    EventLog,
    /// File tailing only.
    Files,
}

fn default_log_source() -> LogSource {
    LogSource::Auto
}

/// Default Windows Event Log channels (spec 004 R6).
fn default_eventlog_channels() -> Vec<String> {
    vec![
        "System".to_string(),
        "Application".to_string(),
        "Security".to_string(),
    ]
}

/// Whether metric collectors are enabled by default on this platform.
///
/// Linux keeps everything ON (unchanged). macOS is a log-forwarder first:
/// collectors are OFF by default and opted into via a `[system]` table.
fn default_metrics_enabled() -> bool {
    cfg!(target_os = "linux")
}

fn default_endpoint() -> String {
    "localhost:50000".to_string()
}

fn default_interval() -> Duration {
    Duration::from_secs(15)
}

/// Default remote-config poll interval: 5 minutes (spec 007 R1).
fn default_config_poll_interval() -> Duration {
    Duration::from_secs(300)
}

/// Default multiline inactivity flush timeout: 1000 ms (spec 008 R1; the Vector
/// default).
fn default_multiline_timeout_ms() -> u64 {
    1000
}

/// Default multiline per-record byte cap: 1 MiB, equal to the single-line
/// partial cap `watcher::MAX_PARTIAL_BYTES` (spec 008 R1/R5).
fn default_multiline_max_bytes() -> usize {
    1024 * 1024
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

/// macOS file-tailing defaults (used when `log_source` resolves to files;
/// the unified log is the primary source on macOS). Modern macOS writes
/// almost everything to the unified store — `install.log` is the one
/// meaningful surviving plaintext log; `system.log` is retained for older
/// systems (a dead file is skipped at discovery). The Homebrew globs cover
/// services like nginx/postgres on Mac servers (ARM and Intel prefixes).
#[cfg(target_os = "macos")]
fn default_logs() -> Vec<String> {
    vec![
        "/var/log/install.log".into(),
        "/var/log/system.log".into(),
        "/opt/homebrew/var/log/*.log".into(),
        "/usr/local/var/log/*.log".into(),
    ]
}

/// Windows file-tailing defaults: EMPTY (deviation from spec 006 R2). The
/// Event Log (spec 004) is the primary Windows source, so file tailing is a
/// pure opt-in fallback. IIS/HTTPERR globs are NOT enabled by default because
/// IIS is not universal and the `glob` crate's backslash-as-escape semantics on
/// Windows paths are unverified without a Windows runner; the example config
/// ships them commented out with forward slashes for operators who want them.
#[cfg(target_os = "windows")]
fn default_logs() -> Vec<String> {
    vec![]
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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
    parse_config(&contents)
}

/// Parse and validate config TOML. Used by `load_config` and by `witness
/// setup` to reject a broken server-provided config before writing it.
pub fn parse_config(contents: &str) -> Result<AgentConfig, Box<dyn std::error::Error>> {
    let config: AgentConfig = toml::from_str(contents)?;
    validate_api_key(&config.api_key)?;
    // Reject an invalid Event Log event-id filter at startup rather than
    // silently ignoring it (spec 004 R6). Validated on every platform so the
    // config format stays uniform; the filter itself is only used on Windows.
    if let Some(spec) = &config.eventlog_event_ids {
        crate::logs::eventlog_filter::EventIdFilter::parse(spec)
            .map_err(|e| format!("invalid eventlog_event_ids: {e}"))?;
    }
    // Reject an invalid multiline start pattern at startup (spec 008 R1) — the
    // same validate-at-startup posture as `eventlog_event_ids`. Validated on
    // every platform so the config format stays uniform; the pattern is only
    // used by the file tailer.
    if let Some(pattern) = &config.multiline_start_pattern {
        regex_lite::Regex::new(pattern)
            .map_err(|e| format!("invalid multiline_start_pattern: {e}"))?;
    }
    Ok(config)
}

/// Validate an API key/token: exactly 32 hex characters.
pub fn validate_api_key(key: &str) -> Result<(), Box<dyn std::error::Error>> {
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
///
/// Linux: `/var/lib/witness` (systemd `StateDirectory=witness` creates this).
/// macOS root daemon (euid 0): `/Library/Application Support/witness` — a
/// stable, root-owned location, since a root daemon has no meaningful `$HOME`.
/// macOS non-root (developer / `witness setup`): `$HOME/Library/Application
/// Support/witness`, falling back to `/tmp/witness` when `$HOME` is unset.
/// Windows: `%ProgramData%\witness\state` (falling back to
/// `C:\ProgramData\witness\state` when `%ProgramData%` is unset) — a stable,
/// non-roaming location a LocalSystem service can write (spec 005 R4).
pub fn state_dir() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        use std::sync::OnceLock;
        static DIR: OnceLock<String> = OnceLock::new();
        DIR.get_or_init(|| program_data_state_dir(std::env::var_os("ProgramData")))
            .as_str()
    }

    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        static DIR: OnceLock<String> = OnceLock::new();
        DIR.get_or_init(|| {
            // A root daemon (launchd) must not use $HOME — it has none.
            if unsafe { libc::geteuid() } == 0 {
                return "/Library/Application Support/witness".to_string();
            }
            if let Some(home) = std::env::var_os("HOME") {
                let p = std::path::Path::new(&home).join("Library/Application Support/witness");
                p.to_string_lossy().into_owned()
            } else {
                "/tmp/witness".to_string()
            }
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "/var/lib/witness"
    }
}

/// Compute the Windows state dir from a `%ProgramData%` value: `<root>\witness\
/// state`, falling back to `C:\ProgramData` when unset. Pure for unit testing
/// (spec 005 R4); the OnceLock caches the result at the call site.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn program_data_state_dir(program_data: Option<std::ffi::OsString>) -> String {
    let root = program_data
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"));
    root.join("witness")
        .join("state")
        .to_string_lossy()
        .into_owned()
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
