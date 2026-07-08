//! Journal watcher — reads structured log entries from systemd-journald.
//!
//! Spawns `journalctl --output=json --follow` as a subprocess, reads entries
//! line by line, and ships them via the Tell SDK with proper severity, service
//! name, and clean message body. Cursor-based checkpointing for restart recovery.
//! Auto-restarts with exponential backoff if journalctl exits unexpectedly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::sink::Sink;

// Structured MESSAGE parsing lives in the shared `structured` module so the
// file tailer reuses byte-identical logic. Re-exported here so this remains the
// journald source's public API surface.
pub use super::structured::split_message;

/// Maximum JSON line length before we skip processing.
/// Note: `read_line` buffers the full line before returning — this guard
/// prevents wasting CPU on JSON parsing, not the memory allocation itself.
const MAX_LINE_LEN: usize = 256 * 1024;

/// How often to persist the cursor (every N entries).
const CURSOR_SAVE_INTERVAL: usize = 100;

/// Maximum backoff between journalctl restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long to wait before retrying an entry when the SDK channel is full.
/// While we wait we stop reading journalctl's stdout, so the pipe (and
/// journald behind it) becomes the buffer — no entries are dropped.
const FULL_CHANNEL_RETRY: Duration = Duration::from_millis(100);

/// Our own syslog identifier — filtered to prevent feedback loops.
const SELF_IDENTIFIER: &str = "witness";

/// Service-name include/exclude filter for the journald source (spec: journald
/// parity with the Windows Event Log `eventlog_exclude_providers` and the macOS
/// `unified_log_predicate`). Matches the resolved program name
/// (`SYSLOG_IDENTIFIER` / `_COMM`), case-sensitive exact.
#[derive(Clone, Default)]
pub struct ServiceFilter {
    /// Allow-list. Empty means allow all.
    pub include: Vec<String>,
    /// Deny-list. An exclude match wins over an include.
    pub exclude: Vec<String>,
}

impl ServiceFilter {
    /// Whether an entry from `program` must be dropped (not shipped).
    #[must_use]
    fn excludes(&self, program: &str) -> bool {
        if self.exclude.iter().any(|s| s == program) {
            return true;
        }
        !self.include.is_empty() && !self.include.iter().any(|s| s == program)
    }
}

// ─── Types ─────────────��────────────────────────────────────────────

/// Structured journal entry.
///
/// Named fields map the five well-known keys we handle specially; anything
/// else the application emits (e.g. tracing-structured fields like `IP`,
/// `JAIL`, `REASON`) lands in `extras` and is forwarded to Tell as a
/// structured log payload. systemd-trusted (`_*`) and journal-internal
/// (`__*`) fields are filtered out downstream.
#[derive(Deserialize)]
struct JournalEntry {
    #[serde(rename = "MESSAGE", default)]
    message: Option<String>,
    #[serde(rename = "SYSLOG_IDENTIFIER", default)]
    syslog_identifier: Option<String>,
    #[serde(rename = "_COMM", default)]
    comm: Option<String>,
    #[serde(rename = "PRIORITY", default)]
    priority: Option<String>,
    #[serde(rename = "__CURSOR", default)]
    cursor: Option<String>,
    #[serde(flatten)]
    extras: HashMap<String, serde_json::Value>,
}

enum LoopExit {
    Cancelled,
    Failed(String),
}

/// Outcome of processing one journal entry.
#[derive(Debug, PartialEq)]
pub(crate) enum ProcessResult {
    /// Entry handled — shipped or intentionally filtered. Carries the cursor
    /// to record, if the entry had one.
    Handled(Option<String>),
    /// SDK channel full — entry NOT shipped. Retry the same line; the cursor
    /// must not advance past it.
    ChannelFull,
    /// JSON parse failure.
    ParseFailed,
}

/// Mutable state carried across the read loop.
struct ReaderState {
    last_cursor: Option<String>,
    entries_since_save: usize,
    dropped: u64,
}

impl ReaderState {
    fn new(initial_cursor: Option<String>) -> Self {
        Self {
            last_cursor: initial_cursor,
            entries_since_save: 0,
            dropped: 0,
        }
    }

    /// Process one entry, waiting out SDK backpressure.
    ///
    /// Returns `false` if cancellation arrived while waiting for channel
    /// capacity — the entry was not shipped and the cursor was not advanced.
    async fn handle_entry(
        &mut self,
        line: &str,
        filter: &ServiceFilter,
        sink: &Sink,
        cancel: &mut watch::Receiver<bool>,
    ) -> bool {
        loop {
            match process_entry(line, filter, sink) {
                ProcessResult::Handled(cursor) => {
                    if let Some(c) = cursor {
                        self.last_cursor = Some(c);
                    }
                    self.entries_since_save += 1;
                    if self.entries_since_save >= CURSOR_SAVE_INTERVAL {
                        save_cursor(self.last_cursor.as_deref());
                        self.entries_since_save = 0;
                    }
                    return true;
                }
                ProcessResult::ChannelFull => {
                    // Stop reading until the SDK drains — the journalctl pipe
                    // (and journald) buffer for us, so nothing is lost.
                    tokio::select! {
                        _ = tokio::time::sleep(FULL_CHANNEL_RETRY) => {}
                        _ = cancel.changed() => return false,
                    }
                }
                ProcessResult::ParseFailed => {
                    self.dropped += 1;
                    if self.dropped.is_power_of_two() {
                        warn!(dropped = self.dropped, "journal entries failed to parse");
                    }
                    return true;
                }
            }
        }
    }

    fn save_final(&self) {
        save_cursor(self.last_cursor.as_deref());
    }
}

// ─── Public API ──────────────────────���──────────────────────────────

/// Run the journal watcher until cancellation.
///
/// Spawns `journalctl --output=json --follow`, reads structured entries,
/// and ships each via the sink. Restarts with exponential backoff if the
/// subprocess exits unexpectedly.
pub async fn tail_journal(sink: Sink, mut cancel: watch::Receiver<bool>, filter: ServiceFilter) {
    let mut backoff = Duration::from_secs(1);

    loop {
        match run_journalctl(&sink, &mut cancel, &filter).await {
            LoopExit::Cancelled => break,
            LoopExit::Failed(reason) => {
                if *cancel.borrow() {
                    break;
                }
                warn!(?backoff, "journalctl exited ({reason}), retrying");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel.changed() => break,
                }
                backoff = super::source::next_backoff(backoff, MAX_BACKOFF);
            }
        }
    }
    info!("journal watcher stopped");
}

/// Check if journalctl is available on this system.
pub fn is_available() -> bool {
    std::process::Command::new("journalctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ─── Subprocess loop ────────────────��───────────────────────────────

/// Single run of the journalctl subprocess. Returns when the process exits
/// or cancellation is received.
async fn run_journalctl(
    sink: &Sink,
    cancel: &mut watch::Receiver<bool>,
    filter: &ServiceFilter,
) -> LoopExit {
    let cursor = load_cursor();
    info!(
        cursor = cursor.as_deref().unwrap_or("(none)"),
        "journal watcher starting"
    );

    let mut child = match spawn_journalctl(cursor.as_deref()) {
        Ok(c) => c,
        Err(e) => return LoopExit::Failed(format!("spawn failed: {e}")),
    };

    let Some(stdout) = child.stdout.take() else {
        return LoopExit::Failed("stdout not available".into());
    };

    let mut reader = BufReader::new(stdout);
    let mut line_buf = String::new();
    let mut state = ReaderState::new(cursor);

    loop {
        line_buf.clear();
        tokio::select! {
            _ = cancel.changed() => {
                let _ = child.kill().await;
                break;
            }
            result = reader.read_line(&mut line_buf) => {
                match result {
                    Ok(0) => {
                        state.save_final();
                        let _ = child.wait().await;
                        return LoopExit::Failed("stream ended".into());
                    }
                    Ok(n) if n > MAX_LINE_LEN => continue,
                    Ok(_) => {
                        if !state.handle_entry(&line_buf, filter, sink, cancel).await {
                            // Cancelled while waiting out backpressure.
                            let _ = child.kill().await;
                            break;
                        }
                    }
                    Err(e) => {
                        state.save_final();
                        let _ = child.wait().await;
                        return LoopExit::Failed(format!("read error: {e}"));
                    }
                }
            }
        }
    }

    state.save_final();
    let _ = child.wait().await;
    LoopExit::Cancelled
}

/// Spawn the journalctl subprocess with the appropriate arguments.
fn spawn_journalctl(cursor: Option<&str>) -> Result<tokio::process::Child, std::io::Error> {
    let mut cmd = Command::new("journalctl");
    cmd.arg("--output=json").arg("--follow").arg("--no-pager");

    if let Some(c) = cursor {
        cmd.arg(format!("--after-cursor={c}"));
    } else {
        cmd.arg("--lines=0");
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    cmd.spawn()
}

// ─── Entry processing ───────────────────��───────────────────────────

/// Process a single JSON journal entry.
///
/// Filtered/empty entries count as handled and still advance the cursor.
/// When the SDK channel is full the entry is not shipped and the caller
/// must retry the same line without advancing the cursor.
pub(crate) fn process_entry(json_line: &str, filter: &ServiceFilter, sink: &Sink) -> ProcessResult {
    let Ok(entry) = serde_json::from_str::<JournalEntry>(json_line.trim()) else {
        return ProcessResult::ParseFailed;
    };

    let cursor = entry.cursor;

    let Some(ref message) = entry.message else {
        return ProcessResult::Handled(cursor);
    };
    if message.is_empty() {
        return ProcessResult::Handled(cursor);
    }

    let program = entry
        .syslog_identifier
        .as_deref()
        .or(entry.comm.as_deref())
        .unwrap_or("unknown");

    // Filter our own entries to prevent feedback loops.
    // witness logs to journal via systemd, and we'd read those back.
    if program == SELF_IDENTIFIER {
        return ProcessResult::Handled(cursor);
    }

    // Operator service include/exclude (filtered entries still advance the
    // cursor so no reprocessing on restart).
    if filter.excludes(program) {
        return ProcessResult::Handled(cursor);
    }

    let level = entry
        .priority
        .as_deref()
        .and_then(priority_to_level)
        .unwrap_or(tell::LogLevel::Info);

    // Parse structured content out of MESSAGE itself (logfmt or JSON).
    // Apps that write their whole payload into MESSAGE (e.g. fail2ban-rs)
    // give us the event phrase plus key=value fields there.
    let (body, parsed_fields) = split_message(message);

    // Merge: journald metadata (for daemons that use it) + parsed MESSAGE
    // fields (for daemons that put everything in the text payload). In
    // either direction the result is one flat structured payload.
    let payload = merge_payloads(entry.extras, parsed_fields);

    if !sink.try_log_with_service(level, &body, None, Some(program), payload) {
        return ProcessResult::ChannelFull;
    }

    ProcessResult::Handled(cursor)
}

/// Combine journald metadata fields with fields parsed from MESSAGE.
/// Parsed-MESSAGE fields win on key collisions — they're the app's voice.
fn merge_payloads(
    extras: HashMap<String, serde_json::Value>,
    parsed: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let metadata = app_fields_payload(extras);
    match (metadata, parsed) {
        (None, None) => None,
        (Some(m), None) => Some(m),
        (None, Some(p)) => Some(p),
        (Some(serde_json::Value::Object(mut m)), Some(serde_json::Value::Object(p))) => {
            for (k, v) in p {
                m.insert(k, v);
            }
            Some(serde_json::Value::Object(m))
        }
        (Some(m), Some(_)) => Some(m),
    }
}

/// Convert the flattened extras into a structured log payload.
///
/// Filters systemd-trusted (`_*`) and journal-internal (`__*`) fields,
/// keeping only application-emitted ones (uppercase-by-journald-convention).
/// Keys are lowercased so ClickHouse queries match the logfmt keys in MESSAGE
/// — `JSONExtractString(message, 'jail')` reads the same way operators write
/// `jail=sshd` in logs. Returns `None` when nothing is left after filtering
/// so we preserve the existing `None::<()>` wire shape for untagged entries.
/// Systemd/libsystemd-emitted fields that carry no structured value for
/// Tell. Kept out of the forwarded payload so they don't pollute queries.
///
/// - `SYSLOG_FACILITY`: derived from PRIORITY, already encoded as level.
/// - `SYSLOG_PID`: duplicate of `_PID` (already filtered by the `_*` rule).
/// - `SYSLOG_RAW`: pre-parsed syslog line; everything in it is broken out.
///
/// `MESSAGE_ID` and `CODE_FILE`/`CODE_FUNCTION`/`CODE_LINE` are kept —
/// they're opt-in event/location identifiers apps choose to emit.
const SYSTEMD_META_DENYLIST: &[&str] = &["SYSLOG_FACILITY", "SYSLOG_PID", "SYSLOG_RAW"];

pub fn app_fields_payload(extras: HashMap<String, serde_json::Value>) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::with_capacity(extras.len());
    for (k, v) in extras {
        if k.starts_with('_') || SYSTEMD_META_DENYLIST.contains(&k.as_str()) {
            continue;
        }
        obj.insert(k.to_lowercase(), v);
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

/// Map syslog PRIORITY string (0-7) to Tell LogLevel.
pub(crate) fn priority_to_level(priority: &str) -> Option<tell::LogLevel> {
    match priority {
        "0" => Some(tell::LogLevel::Emergency),
        "1" => Some(tell::LogLevel::Alert),
        "2" => Some(tell::LogLevel::Critical),
        "3" => Some(tell::LogLevel::Error),
        "4" => Some(tell::LogLevel::Warning),
        "5" => Some(tell::LogLevel::Notice),
        "6" => Some(tell::LogLevel::Info),
        "7" => Some(tell::LogLevel::Debug),
        _ => None,
    }
}

// ─── Cursor persistence ─────────────────────────────────────────────

fn cursor_path() -> PathBuf {
    std::path::Path::new(crate::config::state_dir()).join("journal_cursor")
}

fn load_cursor() -> Option<String> {
    std::fs::read_to_string(cursor_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_cursor(cursor: Option<&str>) {
    let Some(c) = cursor else { return };
    super::source::write_checkpoint(&cursor_path(), c.as_bytes());
}
