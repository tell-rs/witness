//! Unified log watcher — ships entries from macOS unified logging to Tell.
//!
//! Architecturally mirrors `journal.rs`: an NDJSON subprocess (`/usr/bin/log`)
//! read line by line, shipped via the SDK with backpressure honored (never
//! advancing the checkpoint past an unshipped entry), auto-restarted with
//! exponential backoff.
//!
//! Resume differs from journald's opaque cursor. `log stream` has no cursor and
//! is lossy under backpressure, so the watcher runs a two-phase state machine:
//!
//! 1. **Backfill** — on start (and after a backpressure stall), replay the
//!    durable store with `log show --start <checkpoint>`, deduping the inclusive
//!    boundary by `machTimestamp`. This makes the default `error`/`fault`
//!    predicate lossless: any persisted entries dropped by `log stream` during a
//!    stall are recovered by the replay.
//! 2. **Live** — after backfill drains, `log stream` follows for low latency.
//!    If backpressure persists past [`BACKPRESSURE_RECONCILE`], tear down the
//!    stream and return to Backfill.
//!
//! `Info`/`Debug` are memory-only in the unified log store; if an operator
//! widens the predicate to include them, they are best-effort under sustained
//! backpressure (not replayable by `log show`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tracing::{info, warn};

use super::source::{next_backoff, write_checkpoint};
use super::unified_parse::{self as parse, Checkpoint, UnifiedResult};
use crate::sink::Sink;

/// Absolute path to Apple's `log` tool.
const LOG_BIN: &str = "/usr/bin/log";

/// Maximum NDJSON line length before we skip processing.
const MAX_LINE_LEN: usize = 256 * 1024;

/// How often to persist the checkpoint (every N shipped entries).
const CURSOR_SAVE_INTERVAL: usize = 100;

/// Maximum backoff between `log` restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long to wait before retrying an entry when the SDK channel is full.
const FULL_CHANNEL_RETRY: Duration = Duration::from_millis(100);

/// Continuous backpressure in Live mode beyond this tears down the live stream
/// and returns to Backfill, so the durable store replay recovers anything
/// `log stream` dropped during the stall.
const BACKPRESSURE_RECONCILE: Duration = Duration::from_secs(5);

// ─── Public API ──────────────────────────────────────────────────────

/// Run the unified log watcher until cancellation.
///
/// `predicate` is the operator's `unified_log_predicate` (if any); when `None`,
/// the built-in default is used. Either way it is passed verbatim to
/// `/usr/bin/log --predicate` as a single argument, never through a shell.
pub async fn tail_unified_log(
    sink: Sink,
    mut cancel: watch::Receiver<bool>,
    predicate: Option<String>,
) {
    let pred = effective_predicate(predicate.as_deref());
    let mut backoff = Duration::from_secs(1);

    loop {
        match run_cycle(&sink, &mut cancel, &pred).await {
            LoopExit::Cancelled => break,
            LoopExit::Failed(reason) => {
                if *cancel.borrow() {
                    break;
                }
                warn!(?backoff, "log exited ({reason}), retrying");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel.changed() => break,
                }
                backoff = next_backoff(backoff, MAX_BACKOFF);
            }
        }
    }
    info!("unified log watcher stopped");
}

/// Whether the unified log tool is available (`/usr/bin/log` is executable).
#[must_use]
pub fn is_available() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(LOG_BIN)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        Path::new(LOG_BIN).is_file()
    }
}

/// The effective predicate: the operator's override, else the built-in default.
#[must_use]
pub fn effective_predicate(configured: Option<&str>) -> String {
    configured
        .map(str::to_string)
        .unwrap_or_else(|| parse::DEFAULT_PREDICATE.to_string())
}

/// On-disk checkpoint file path.
#[must_use]
pub fn checkpoint_path() -> PathBuf {
    Path::new(crate::config::state_dir()).join("unified_log_checkpoint")
}

// ─── State machine ───────────────────────────────────────────────────

enum LoopExit {
    Cancelled,
    Failed(String),
}

/// Outcome of one Backfill or Live phase.
enum PhaseExit {
    /// Subprocess drained/exited cleanly (backfill done, or stream ended).
    Done(String),
    Cancelled,
    Failed(String),
    /// Live backpressure exceeded the reconcile threshold — re-enter Backfill.
    Reconcile,
}

/// One full Backfill → Live cycle. Loops back to Backfill on reconcile.
async fn run_cycle(sink: &Sink, cancel: &mut watch::Receiver<bool>, pred: &str) -> LoopExit {
    loop {
        let initial = load_checkpoint();
        let mut state = ReaderState::new(initial.clone());

        // Backfill only when we have a durable position to resume from;
        // otherwise start live "from now".
        if let Some(cp) = &initial {
            info!(start = %cp.wall_timestamp, "unified log backfill");
            match run_backfill(sink, cancel, pred, cp, &mut state).await {
                PhaseExit::Cancelled => return LoopExit::Cancelled,
                PhaseExit::Failed(r) => return LoopExit::Failed(r),
                PhaseExit::Reconcile | PhaseExit::Done(_) => {}
            }
        }

        info!("unified log live stream");
        match run_live(sink, cancel, pred, &mut state).await {
            PhaseExit::Cancelled => return LoopExit::Cancelled,
            PhaseExit::Failed(r) => return LoopExit::Failed(r),
            PhaseExit::Done(r) => return LoopExit::Failed(r),
            PhaseExit::Reconcile => continue,
        }
    }
}

/// Backfill phase: `log show --start <checkpoint>` replayed to the store tail.
async fn run_backfill(
    sink: &Sink,
    cancel: &mut watch::Receiver<bool>,
    pred: &str,
    dedupe: &Checkpoint,
    state: &mut ReaderState,
) -> PhaseExit {
    let mut child = match spawn_show(pred, &dedupe.wall_timestamp) {
        Ok(c) => c,
        Err(e) => return PhaseExit::Failed(format!("spawn failed: {e}")),
    };
    read_stream(sink, cancel, state, &mut child, Some(dedupe), false).await
}

/// Live phase: `log stream` followed until cancel, failure, or reconcile.
async fn run_live(
    sink: &Sink,
    cancel: &mut watch::Receiver<bool>,
    pred: &str,
    state: &mut ReaderState,
) -> PhaseExit {
    let mut child = match spawn_stream(pred) {
        Ok(c) => c,
        Err(e) => return PhaseExit::Failed(format!("spawn failed: {e}")),
    };
    read_stream(sink, cancel, state, &mut child, None, true).await
}

/// Shared NDJSON read loop for both phases.
async fn read_stream(
    sink: &Sink,
    cancel: &mut watch::Receiver<bool>,
    state: &mut ReaderState,
    child: &mut Child,
    dedupe: Option<&Checkpoint>,
    reconcile: bool,
) -> PhaseExit {
    let Some(stdout) = child.stdout.take() else {
        return PhaseExit::Failed("stdout not available".into());
    };
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut full_since: Option<Instant> = None;

    loop {
        line.clear();
        tokio::select! {
            _ = cancel.changed() => {
                let _ = child.kill().await;
                state.save_final();
                return PhaseExit::Cancelled;
            }
            result = reader.read_line(&mut line) => match result {
                Ok(0) => {
                    state.save_final();
                    let _ = child.wait().await;
                    return PhaseExit::Done("stream ended".into());
                }
                Ok(n) if n > MAX_LINE_LEN => continue,
                // Skip the `log stream` banner and blank lines silently — they
                // are not parse failures.
                Ok(_) if !line.trim_start().starts_with('{') => continue,
                Ok(_) => {
                    match ship_line(&line, sink, state, cancel, dedupe, reconcile, &mut full_since)
                        .await
                    {
                        LineOutcome::Done => {}
                        LineOutcome::Cancelled => {
                            let _ = child.kill().await;
                            state.save_final();
                            return PhaseExit::Cancelled;
                        }
                        LineOutcome::Reconcile => {
                            let _ = child.kill().await;
                            state.save_final();
                            return PhaseExit::Reconcile;
                        }
                    }
                }
                Err(e) => {
                    state.save_final();
                    let _ = child.wait().await;
                    return PhaseExit::Failed(format!("read error: {e}"));
                }
            }
        }
    }
}

/// Outcome of shipping one line (after waiting out any backpressure).
enum LineOutcome {
    Done,
    Cancelled,
    Reconcile,
}

/// Ship one line, retrying under backpressure. In Live mode (`reconcile`),
/// continuous fullness past [`BACKPRESSURE_RECONCILE`] returns `Reconcile`.
async fn ship_line(
    line: &str,
    sink: &Sink,
    state: &mut ReaderState,
    cancel: &mut watch::Receiver<bool>,
    dedupe: Option<&Checkpoint>,
    reconcile: bool,
    full_since: &mut Option<Instant>,
) -> LineOutcome {
    loop {
        match state.record(parse::process_entry(line, sink, dedupe)) {
            RecordOutcome::Advanced => {
                *full_since = None;
                return LineOutcome::Done;
            }
            RecordOutcome::Full => {
                if reconcile {
                    let now = Instant::now();
                    let start = *full_since.get_or_insert(now);
                    if reconcile_due(Some(start), now, BACKPRESSURE_RECONCILE) {
                        return LineOutcome::Reconcile;
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(FULL_CHANNEL_RETRY) => {}
                    _ = cancel.changed() => return LineOutcome::Cancelled,
                }
            }
        }
    }
}

/// Whether a backpressure stall has lasted at least `threshold`.
fn reconcile_due(full_since: Option<Instant>, now: Instant, threshold: Duration) -> bool {
    full_since.is_some_and(|start| now.duration_since(start) >= threshold)
}

// ─── Reader bookkeeping ──────────────────────────────────────────────

enum RecordOutcome {
    Advanced,
    Full,
}

/// Mutable state carried across the read loop (checkpoint + counters).
struct ReaderState {
    last: Option<Checkpoint>,
    since_save: usize,
    dropped: u64,
}

impl ReaderState {
    fn new(initial: Option<Checkpoint>) -> Self {
        Self {
            last: initial,
            since_save: 0,
            dropped: 0,
        }
    }

    fn record(&mut self, result: UnifiedResult) -> RecordOutcome {
        match result {
            UnifiedResult::Handled(Some(cp)) => {
                self.last = Some(cp);
                self.since_save += 1;
                if self.since_save >= CURSOR_SAVE_INTERVAL {
                    save_checkpoint(self.last.as_ref());
                    self.since_save = 0;
                }
                RecordOutcome::Advanced
            }
            UnifiedResult::Handled(None) => RecordOutcome::Advanced,
            UnifiedResult::ParseFailed => {
                self.dropped += 1;
                if self.dropped.is_power_of_two() {
                    warn!(
                        dropped = self.dropped,
                        "unified log entries failed to parse"
                    );
                }
                RecordOutcome::Advanced
            }
            UnifiedResult::ChannelFull => RecordOutcome::Full,
        }
    }

    fn save_final(&self) {
        save_checkpoint(self.last.as_ref());
    }
}

// ─── Subprocess spawning ─────────────────────────────────────────────

fn spawn_show(predicate: &str, start_wall: &str) -> Result<Child, std::io::Error> {
    let mut cmd = Command::new(LOG_BIN);
    cmd.arg("show")
        .arg("--style")
        .arg("ndjson")
        .arg("--start")
        .arg(start_wall)
        .arg("--predicate")
        .arg(predicate);
    configure(&mut cmd);
    cmd.spawn()
}

fn spawn_stream(predicate: &str) -> Result<Child, std::io::Error> {
    let mut cmd = Command::new(LOG_BIN);
    cmd.arg("stream")
        .arg("--style")
        .arg("ndjson")
        .arg("--predicate")
        .arg(predicate);
    configure(&mut cmd);
    cmd.spawn()
}

fn configure(cmd: &mut Command) {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    cmd.kill_on_drop(true);
}

// ─── Checkpoint persistence ──────────────────────────────────────────

/// Load the checkpoint; a missing or corrupt file yields `None` (start from
/// "now"), never a crash (spec 001 threat model).
fn load_checkpoint() -> Option<Checkpoint> {
    let data = std::fs::read(checkpoint_path()).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_checkpoint(cp: Option<&Checkpoint>) {
    let Some(cp) = cp else { return };
    let Ok(bytes) = serde_json::to_vec(cp) else {
        return;
    };
    write_checkpoint(&checkpoint_path(), &bytes);
}

#[cfg(test)]
pub(crate) use inner_test_hooks::*;

#[cfg(test)]
mod inner_test_hooks {
    use super::*;

    /// Test hook: expose the reconcile decision.
    pub(crate) fn reconcile_due_for_test(
        full_since: Option<Instant>,
        now: Instant,
        threshold: Duration,
    ) -> bool {
        reconcile_due(full_since, now, threshold)
    }

    /// Test hook: run the reader bookkeeping on a sequence of results.
    pub(crate) struct StateProbe(ReaderState);

    impl StateProbe {
        pub(crate) fn new(initial: Option<Checkpoint>) -> Self {
            Self(ReaderState::new(initial))
        }
        pub(crate) fn record_advanced(&mut self, result: UnifiedResult) -> bool {
            matches!(self.0.record(result), RecordOutcome::Advanced)
        }
        pub(crate) fn last(&self) -> Option<Checkpoint> {
            self.0.last.clone()
        }
    }
}
