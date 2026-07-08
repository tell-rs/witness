//! Parsing and per-entry processing for the macOS unified log source.
//!
//! Deserializes one NDJSON line emitted by `/usr/bin/log --style ndjson` into a
//! [`LogEntry`], maps `messageType` to a [`tell::LogLevel`], derives the service
//! name and a curated structured payload, filters witness's own entries, and
//! ships the body via the sink — mirroring `journal::process_entry`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::sink::Sink;

/// Default `--predicate`: actionable, durably-persisted entries only.
///
/// Verified live against `/usr/bin/log` on macOS 26 (`log show --predicate`
/// accepts it and exits 0). The `error`/`fault` clause is the persisted,
/// operator-actionable firehose subset (enabling lossless resume via
/// `log show`); the remaining clauses add curated Default-level security
/// signal (authorization/TCC, code-execution policy, and the login/remote
/// access surface). `process ==` is translated by `log` to its internal
/// process filter even though the emitted NDJSON has no `process` field.
///
/// A configured `unified_log_predicate` REPLACES this entirely and is passed
/// verbatim as one argv element (never through a shell).
pub(crate) const DEFAULT_PREDICATE: &str = "(messageType == \"error\" OR messageType == \"fault\") \
     OR subsystem == \"com.apple.TCC\" \
     OR subsystem == \"com.apple.syspolicy.exec\" \
     OR process == \"sudo\" \
     OR process == \"sshd\" \
     OR process == \"loginwindow\" \
     OR process == \"screensharingd\"";

/// Basename of witness's own process, filtered to prevent feedback loops.
const SELF_PROCESS: &str = "witness";

/// launchd bundle identifier (spec 002 R1); a `subsystem` under it is our own.
const SELF_SUBSYSTEM_PREFIX: &str = "rs.tell.witness";

/// Internal unified-log fields never forwarded as structured payload (mach /
/// thread / UUID / backtrace / activity internals). Compared lowercased so the
/// forwarded payload reads uniformly with the journald path. Named fields
/// (`subsystem`, `category`, …) are consumed before `extras`, so most of these
/// only appear defensively; listing them keeps the filter robust to schema
/// drift and to `traceEvent` shapes.
const INTERNAL_FIELDS: &[&str] = &[
    "threadid",
    "senderimageuuid",
    "senderimagepath",
    "processimageuuid",
    "backtrace",
    "traceid",
    "senderprogramcounter",
    "machtimestamp",
    "activityidentifier",
    "parentactivityidentifier",
    "creatoractivityid",
    "timezonename",
    "source",
    "formatstring",
    "userid",
    "bootuuid",
];

/// Durable resume position for the unified log.
///
/// `mach_timestamp` is monotonic within a boot session (`boot_uuid`); it is the
/// dedupe key for the inclusive `log show --start` boundary. `wall_timestamp`
/// is the exact `timestamp` string `log show --start` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub boot_uuid: String,
    pub mach_timestamp: u64,
    pub wall_timestamp: String,
}

/// One NDJSON log entry. Only the fields witness acts on are named; unknown
/// fields land in `extras` and are forwarded when they are not internals.
#[derive(Debug, Deserialize)]
pub(crate) struct LogEntry {
    #[serde(rename = "eventType", default)]
    event_type: Option<String>,
    #[serde(rename = "messageType", default)]
    message_type: Option<String>,
    #[serde(rename = "eventMessage", default)]
    event_message: Option<String>,
    #[serde(rename = "subsystem", default)]
    subsystem: Option<String>,
    #[serde(rename = "category", default)]
    category: Option<String>,
    #[serde(rename = "processImagePath", default)]
    process_image_path: Option<String>,
    #[serde(rename = "processID", default)]
    process_id: Option<i64>,
    #[serde(rename = "machTimestamp", default)]
    mach_timestamp: Option<u64>,
    #[serde(rename = "bootUUID", default)]
    boot_uuid: Option<String>,
    #[serde(rename = "timestamp", default)]
    wall_timestamp: Option<String>,
    #[serde(flatten)]
    extras: HashMap<String, serde_json::Value>,
}

impl LogEntry {
    /// The resume position of this entry, if it carries one.
    fn checkpoint(&self) -> Option<Checkpoint> {
        match (
            self.boot_uuid.as_deref(),
            self.mach_timestamp,
            self.wall_timestamp.as_deref(),
        ) {
            (Some(boot), Some(mach), Some(wall)) => Some(Checkpoint {
                boot_uuid: boot.to_string(),
                mach_timestamp: mach,
                wall_timestamp: wall.to_string(),
            }),
            _ => None,
        }
    }
}

/// Outcome of processing one line, mirroring `journal::ProcessResult`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UnifiedResult {
    /// Handled — shipped or intentionally skipped. Carries the entry's
    /// checkpoint to record, if it had a position (`None` means "do not
    /// advance", e.g. a backfill dedupe skip of an already-shipped entry).
    Handled(Option<Checkpoint>),
    /// SDK channel full — not shipped. Retry the same line; do not advance.
    ChannelFull,
    /// JSON parse failure.
    ParseFailed,
}

/// Process one NDJSON line.
///
/// `dedupe` is the loaded checkpoint during Backfill (skip entries at or before
/// it within the same boot); `None` in Live mode. Non-`logEvent`/`traceEvent`
/// structural entries are skipped but still advance the checkpoint.
pub(crate) fn process_entry(line: &str, sink: &Sink, dedupe: Option<&Checkpoint>) -> UnifiedResult {
    let Ok(entry) = serde_json::from_str::<LogEntry>(line.trim()) else {
        return UnifiedResult::ParseFailed;
    };

    let pos = entry.checkpoint();

    // Only log/trace events carry a human message; the rest is structural noise.
    let event_type = entry.event_type.as_deref().unwrap_or_default();
    if event_type != "logEvent" && event_type != "traceEvent" {
        return UnifiedResult::Handled(pos);
    }

    // Backfill dedupe of the inclusive `--start` boundary (same boot only).
    if let (Some(cp), Some(mach), Some(boot)) =
        (dedupe, entry.mach_timestamp, entry.boot_uuid.as_deref())
        && boot == cp.boot_uuid
        && mach <= cp.mach_timestamp
    {
        return UnifiedResult::Handled(None);
    }

    // Self-feedback prevention.
    if is_self(&entry) {
        return UnifiedResult::Handled(pos);
    }

    let Some(message) = entry.event_message.as_deref() else {
        return UnifiedResult::Handled(pos);
    };
    if message.is_empty() {
        return UnifiedResult::Handled(pos);
    }

    let level = message_type_to_level(entry.message_type.as_deref());
    let service = service_of(&entry);
    let payload = build_payload(&entry);

    if !sink.try_log_with_service(level, message, None, Some(&service), payload) {
        return UnifiedResult::ChannelFull;
    }

    UnifiedResult::Handled(pos)
}

/// Whether an entry is witness's own output (feedback-loop guard).
fn is_self(entry: &LogEntry) -> bool {
    if entry
        .subsystem
        .as_deref()
        .is_some_and(|s| s.starts_with(SELF_SUBSYSTEM_PREFIX))
    {
        return true;
    }
    entry
        .process_image_path
        .as_deref()
        .map(basename)
        .is_some_and(|b| b == SELF_PROCESS)
}

/// Map `messageType` to a Tell log level. Missing/unknown → `Info`.
pub(crate) fn message_type_to_level(message_type: Option<&str>) -> tell::LogLevel {
    match message_type {
        Some("Fault") => tell::LogLevel::Critical,
        Some("Error") => tell::LogLevel::Error,
        Some("Default") => tell::LogLevel::Notice,
        Some("Debug") => tell::LogLevel::Debug,
        _ => tell::LogLevel::Info,
    }
}

/// Service name: `subsystem` if non-empty, else the `processImagePath`
/// basename, else `"unknown"`.
pub(crate) fn service_of(entry: &LogEntry) -> String {
    if let Some(sub) = entry.subsystem.as_deref()
        && !sub.is_empty()
    {
        return sub.to_string();
    }
    if let Some(path) = entry.process_image_path.as_deref() {
        let base = basename(path);
        if !base.is_empty() {
            return base.to_string();
        }
    }
    "unknown".to_string()
}

/// Curated structured payload: `subsystem`, `category`, `process` basename,
/// `pid`, plus any non-internal `extras` (lowercased). Excludes all mach /
/// thread / UUID / backtrace internals.
pub(crate) fn build_payload(entry: &LogEntry) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();

    if let Some(sub) = entry.subsystem.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("subsystem".into(), sub.into());
    }
    if let Some(cat) = entry.category.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("category".into(), cat.into());
    }
    if let Some(path) = entry.process_image_path.as_deref() {
        let base = basename(path);
        if !base.is_empty() {
            obj.insert("process".into(), base.into());
        }
    }
    if let Some(pid) = entry.process_id {
        obj.insert("pid".into(), pid.into());
    }

    for (key, value) in &entry.extras {
        let lower = key.to_lowercase();
        if INTERNAL_FIELDS.contains(&lower.as_str()) {
            continue;
        }
        obj.entry(lower).or_insert_with(|| value.clone());
    }

    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

/// Last path component of an absolute image path.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
