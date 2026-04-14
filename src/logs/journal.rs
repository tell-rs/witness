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

use crate::sink::Sink;

/// Maximum JSON line length before we skip processing.
/// Note: `read_line` buffers the full line before returning — this guard
/// prevents wasting CPU on JSON parsing, not the memory allocation itself.
const MAX_LINE_LEN: usize = 256 * 1024;

/// How often to persist the cursor (every N entries).
const CURSOR_SAVE_INTERVAL: usize = 100;

/// Maximum backoff between journalctl restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Our own syslog identifier — filtered to prevent feedback loops.
const SELF_IDENTIFIER: &str = "witness";

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

    fn handle_entry(&mut self, line: &str, sink: &Sink) {
        match process_entry(line, sink) {
            Some(cursor) => {
                if let Some(c) = cursor {
                    self.last_cursor = Some(c);
                }
                self.entries_since_save += 1;
                if self.entries_since_save >= CURSOR_SAVE_INTERVAL {
                    save_cursor(self.last_cursor.as_deref());
                    self.entries_since_save = 0;
                }
            }
            None => {
                self.dropped += 1;
                if self.dropped.is_power_of_two() {
                    eprintln!("WARNING: {} journal entries failed to parse", self.dropped);
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
pub async fn tail_journal(sink: Sink, mut cancel: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);

    loop {
        match run_journalctl(&sink, &mut cancel).await {
            LoopExit::Cancelled => break,
            LoopExit::Failed(reason) => {
                if *cancel.borrow() {
                    break;
                }
                eprintln!("WARNING: journalctl exited ({reason}), retrying in {backoff:?}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel.changed() => break,
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    eprintln!("journal watcher stopped");
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
async fn run_journalctl(sink: &Sink, cancel: &mut watch::Receiver<bool>) -> LoopExit {
    let cursor = load_cursor();
    eprintln!(
        "journal watcher starting (cursor: {})",
        cursor.as_deref().unwrap_or("(none)")
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
                    Ok(_) => state.handle_entry(&line_buf, sink),
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
/// Returns `Some(cursor)` if parsing succeeded (entry may or may not have
/// been shipped — filtered/empty entries still advance the cursor).
/// Returns `None` on JSON parse failure.
pub(crate) fn process_entry(json_line: &str, sink: &Sink) -> Option<Option<String>> {
    let entry: JournalEntry = serde_json::from_str(json_line.trim()).ok()?;

    let cursor = entry.cursor;

    let Some(ref message) = entry.message else {
        return Some(cursor);
    };
    if message.is_empty() {
        return Some(cursor);
    }

    let program = entry
        .syslog_identifier
        .as_deref()
        .or(entry.comm.as_deref())
        .unwrap_or("unknown");

    // Filter our own entries to prevent feedback loops.
    // witness logs to journal via systemd, and we'd read those back.
    if program == SELF_IDENTIFIER {
        return Some(cursor);
    }

    let level = entry
        .priority
        .as_deref()
        .and_then(priority_to_level)
        .unwrap_or(tell::LogLevel::Info);

    let payload = app_fields_payload(entry.extras);
    sink.log_with_service(level, message, None, Some(program), payload);

    Some(cursor)
}

/// Convert the flattened extras into a structured log payload.
///
/// Filters systemd-trusted (`_*`) and journal-internal (`__*`) fields,
/// keeping only application-emitted ones (uppercase-by-journald-convention).
/// Keys are lowercased so ClickHouse queries match the logfmt keys in MESSAGE
/// — `JSONExtractString(message, 'jail')` reads the same way operators write
/// `jail=sshd` in logs. Returns `None` when nothing is left after filtering
/// so we preserve the existing `None::<()>` wire shape for untagged entries.
pub fn app_fields_payload(extras: HashMap<String, serde_json::Value>) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::with_capacity(extras.len());
    for (k, v) in extras {
        if k.starts_with('_') {
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
    use std::io::Write;
    let Some(c) = cursor else { return };
    let path = cursor_path();
    let tmp = path.with_extension("tmp");
    let Ok(mut file) = std::fs::File::create(&tmp) else {
        return;
    };
    if file.write_all(c.as_bytes()).is_ok() && file.sync_all().is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}
