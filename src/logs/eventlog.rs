//! Windows Event Log source — the `Evt*` pull pump (spec 004 R1/R2/R4).
//!
//! Windows-only plumbing that mirrors `journal.rs`/`unified.rs`: it subscribes
//! to each configured channel in the PULL model (a `CreateEventW` signal
//! handle + `EvtNext` at our own pace), renders each event to XML with
//! `EvtRender`, hands it to the pure [`eventlog_parse::process_entry`], ships
//! via the sink with journald backpressure semantics (pause pulling, retry the
//! same event, never advance the bookmark past an unshipped entry), and
//! persists a per-channel bookmark as its checkpoint.
//!
//! The blocking `Evt*` FFI runs on a dedicated blocking thread per channel
//! (`spawn_blocking`), never a tokio worker (concurrency rules). All parsing,
//! level mapping, payload building, and bookmark round-trip live in the pure,
//! fixture-tested `eventlog_parse` module.

use std::ffi::c_void;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_NO_MORE_ITEMS, ERROR_TIMEOUT, HANDLE, WAIT_OBJECT_0,
    WIN32_ERROR,
};
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtCreateBookmark, EvtNext, EvtRender, EvtRenderBookmark,
    EvtRenderEventXml, EvtSubscribe, EvtSubscribeStartAfterBookmark, EvtSubscribeToFutureEvents,
    EvtUpdateBookmark,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{HRESULT, HSTRING, PCWSTR};

use super::eventlog_filter::EventFilter;
use super::eventlog_parse::{
    self as parse, ProcessResult, bookmark_path, effective_query, load_bookmark, save_bookmark,
};
use super::source::next_backoff;
use crate::sink::Sink;

/// Maximum backoff between subscription-restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long to wait for the signal event before re-checking cancellation.
const WAIT_TIMEOUT_MS: u32 = 1000;
/// How long to wait before retrying an event when the SDK channel is full.
const FULL_CHANNEL_RETRY: Duration = Duration::from_millis(100);
/// Persist the bookmark every N shipped events (mirror `CURSOR_SAVE_INTERVAL`).
const BOOKMARK_SAVE_INTERVAL: usize = 100;
/// How many event handles to pull per `EvtNext` batch.
const BATCH: usize = 32;

// ─── Public API ──────────────────────────────────────────────────────

/// Run the Event Log watcher until cancellation. One blocking pump thread per
/// channel; joins them all on cancel (spec 004 R1).
pub async fn tail_eventlog(
    sink: Sink,
    mut cancel: watch::Receiver<bool>,
    channels: Vec<String>,
    query: Option<String>,
    event_ids: Option<String>,
    exclude_providers: Vec<String>,
) {
    let query = effective_query(query.as_deref());
    // The event-id spec was validated at startup (config load); on the
    // off-chance it is unparseable here, fall back to no filter with a warn.
    let filter = EventFilter::new(event_ids.as_deref(), &exclude_providers).unwrap_or_else(|e| {
        warn!("invalid eventlog_event_ids ({e}) — event-id filter disabled");
        EventFilter::default()
    });
    let mut handles = Vec::with_capacity(channels.len());

    for channel in channels {
        let sink = sink.clone();
        let cancel = cancel.clone();
        let query = query.clone();
        let filter = filter.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            run_channel(&channel, &query, &filter, &sink, &cancel);
        }));
    }

    let _ = cancel.changed().await;
    for h in handles {
        let _ = h.await;
    }
    info!("event log watcher stopped");
}

// ─── Per-channel pump ────────────────────────────────────────────────

enum PumpExit {
    Cancelled,
    Failed(String),
    /// The channel is not readable (e.g. `Security` without privilege) — skip
    /// it and never retry (spec 004 R6).
    AccessDenied,
}

/// Run one channel's pump, restarting with exponential backoff on unexpected
/// subscription failure (mirrors `tail_journal`).
fn run_channel(
    channel: &str,
    query: &str,
    filter: &EventFilter,
    sink: &Sink,
    cancel: &watch::Receiver<bool>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if *cancel.borrow() {
            break;
        }
        match pump_once(channel, query, filter, sink, cancel) {
            PumpExit::Cancelled => break,
            PumpExit::AccessDenied => {
                warn!(channel, "event log channel access denied — skipping");
                break;
            }
            PumpExit::Failed(reason) => {
                if *cancel.borrow() {
                    break;
                }
                warn!(
                    channel,
                    ?backoff,
                    "event log subscription failed ({reason}), retrying"
                );
                if sleep_cancellable(backoff, cancel) {
                    break;
                }
                backoff = next_backoff(backoff, MAX_BACKOFF);
            }
        }
    }
}

/// One subscription lifetime: subscribe (resume from bookmark or tail from
/// now), then pull/ship/bookmark until cancel or failure.
fn pump_once(
    channel: &str,
    query: &str,
    filter: &EventFilter,
    sink: &Sink,
    cancel: &watch::Receiver<bool>,
) -> PumpExit {
    let signal = match Signal::new() {
        Ok(s) => s,
        Err(e) => return PumpExit::Failed(format!("CreateEventW: {e}")),
    };

    let bookmark_xml = load_bookmark(&bookmark_path(channel));
    let mut bookmark = match Bookmark::new(bookmark_xml.as_deref()) {
        Ok(b) => b,
        Err(e) => return PumpExit::Failed(format!("EvtCreateBookmark: {e}")),
    };

    let sub = match subscribe(channel, query, &signal, &bookmark, bookmark_xml.is_some()) {
        Ok(s) => s,
        Err(e) if is_win32(&e, ERROR_ACCESS_DENIED) => return PumpExit::AccessDenied,
        Err(e) => return PumpExit::Failed(format!("EvtSubscribe: {e}")),
    };

    info!(
        channel,
        resumed = bookmark_xml.is_some(),
        "event log subscription open"
    );
    pull_loop(channel, &sub, &signal, &mut bookmark, filter, sink, cancel)
}

/// Wait on the signal, pull batches with `EvtNext`, ship each event, and
/// persist the bookmark, until cancel or a fatal error.
fn pull_loop(
    channel: &str,
    sub: &Handle,
    signal: &Signal,
    bookmark: &mut Bookmark,
    filter: &EventFilter,
    sink: &Sink,
    cancel: &watch::Receiver<bool>,
) -> PumpExit {
    let mut state = SaveState::new(channel);
    loop {
        if *cancel.borrow() {
            state.flush(bookmark);
            return PumpExit::Cancelled;
        }
        let wait = unsafe { WaitForSingleObject(signal.0, WAIT_TIMEOUT_MS) };
        if wait != WAIT_OBJECT_0 {
            continue; // timeout — re-check cancellation
        }
        match drain_batch(sub, bookmark, &mut state, filter, sink, cancel) {
            BatchOutcome::More => {}
            BatchOutcome::Cancelled => {
                state.flush(bookmark);
                return PumpExit::Cancelled;
            }
            BatchOutcome::Failed(e) => {
                state.flush(bookmark);
                return PumpExit::Failed(e);
            }
        }
    }
}

enum BatchOutcome {
    /// Batch drained; wait for the signal again.
    More,
    Cancelled,
    Failed(String),
}

/// Pull one `EvtNext` batch and ship each event.
fn drain_batch(
    sub: &Handle,
    bookmark: &mut Bookmark,
    state: &mut SaveState,
    filter: &EventFilter,
    sink: &Sink,
    cancel: &watch::Receiver<bool>,
) -> BatchOutcome {
    loop {
        // `EvtNext` fills a raw `isize` array (the ABI of `EVT_HANDLE`).
        let mut raw = [0isize; BATCH];
        let mut returned = 0u32;
        let ok = unsafe { EvtNext(sub.0, &mut raw, WAIT_TIMEOUT_MS, 0, &mut returned) };
        if let Err(e) = ok {
            return if is_win32(&e, ERROR_NO_MORE_ITEMS) || is_win32(&e, ERROR_TIMEOUT) {
                BatchOutcome::More
            } else {
                BatchOutcome::Failed(format!("EvtNext: {e}"))
            };
        }
        for &r in raw.iter().take(returned as usize) {
            let handle = EVT_HANDLE(r);
            let outcome = ship_event(handle, bookmark, state, filter, sink, cancel);
            unsafe {
                let _ = EvtClose(handle);
            }
            match outcome {
                EventOutcome::Handled => {}
                EventOutcome::Cancelled => return BatchOutcome::Cancelled,
                EventOutcome::Failed(e) => return BatchOutcome::Failed(e),
            }
        }
    }
}

enum EventOutcome {
    Handled,
    Cancelled,
    Failed(String),
}

/// Render one event, process it, and act on the [`ProcessResult`] with journald
/// backpressure semantics (spec 004 R2).
fn ship_event(
    event: EVT_HANDLE,
    bookmark: &mut Bookmark,
    state: &mut SaveState,
    filter: &EventFilter,
    sink: &Sink,
    cancel: &watch::Receiver<bool>,
) -> EventOutcome {
    let xml = match render_xml(event) {
        Ok(x) => x,
        Err(_) => return EventOutcome::Handled, // unrenderable → drop, advance
    };
    let message = format_message(event);

    loop {
        match parse::process_entry(&xml, message.as_deref(), filter, sink) {
            ProcessResult::Handled | ProcessResult::ParseFailed => {
                if bookmark.update(event).is_err() {
                    return EventOutcome::Failed("EvtUpdateBookmark".into());
                }
                state.on_shipped(bookmark);
                return EventOutcome::Handled;
            }
            ProcessResult::ChannelFull => {
                // Stop pulling; the channel's retained store is the buffer. Do
                // NOT advance the bookmark. Retry the SAME event.
                if sleep_cancellable(FULL_CHANNEL_RETRY, cancel) {
                    return EventOutcome::Cancelled;
                }
            }
        }
    }
}

// ─── Bookmark save cadence ───────────────────────────────────────────

struct SaveState {
    channel: String,
    since_save: usize,
}

impl SaveState {
    fn new(channel: &str) -> Self {
        Self {
            channel: channel.to_string(),
            since_save: 0,
        }
    }

    fn on_shipped(&mut self, bookmark: &Bookmark) {
        self.since_save += 1;
        if self.since_save >= BOOKMARK_SAVE_INTERVAL {
            self.flush(bookmark);
            self.since_save = 0;
        }
    }

    fn flush(&self, bookmark: &Bookmark) {
        if let Ok(xml) = bookmark.render() {
            save_bookmark(&bookmark_path(&self.channel), &xml);
        }
    }
}

// ─── Evt* wrappers ───────────────────────────────────────────────────

/// RAII wrapper closing an `EVT_HANDLE` on drop.
struct Handle(EVT_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = EvtClose(self.0);
            }
        }
    }
}

/// RAII wrapper for a Win32 event handle used as the subscription signal.
struct Signal(HANDLE);

impl Signal {
    fn new() -> windows::core::Result<Self> {
        // Manual-reset, initially non-signaled, unnamed.
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null())? };
        Ok(Self(handle))
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// RAII wrapper for a bookmark `EVT_HANDLE`.
struct Bookmark(EVT_HANDLE);

impl Bookmark {
    /// Create a bookmark: from persisted XML when present (resume), else empty
    /// (tail from future events). Corrupt XML is handled by the caller passing
    /// `None` (`load_bookmark` already rejects garbage).
    fn new(xml: Option<&str>) -> windows::core::Result<Self> {
        let handle = match xml {
            Some(x) => unsafe { EvtCreateBookmark(&HSTRING::from(x))? },
            None => unsafe { EvtCreateBookmark(PCWSTR::null())? },
        };
        Ok(Self(handle))
    }

    fn update(&mut self, event: EVT_HANDLE) -> windows::core::Result<()> {
        unsafe { EvtUpdateBookmark(self.0, event) }
    }

    fn render(&self) -> windows::core::Result<String> {
        render_handle(None, self.0, EvtRenderBookmark.0)
    }
}

impl Drop for Bookmark {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = EvtClose(self.0);
            }
        }
    }
}

/// Open a pull-model subscription for `channel` with `query`, resuming from the
/// bookmark when one was loaded, else tailing from future events.
fn subscribe(
    channel: &str,
    query: &str,
    signal: &Signal,
    bookmark: &Bookmark,
    resume: bool,
) -> windows::core::Result<Handle> {
    let flags = if resume {
        EvtSubscribeStartAfterBookmark.0
    } else {
        EvtSubscribeToFutureEvents.0
    };
    let bm = resume.then_some(bookmark.0);
    let sub = unsafe {
        EvtSubscribe(
            None,
            Some(signal.0),
            &HSTRING::from(channel),
            &HSTRING::from(query),
            bm,
            None,
            None,
            flags,
        )?
    };
    Ok(Handle(sub))
}

/// Render an event to its XML string (`EvtRenderEventXml`).
fn render_xml(event: EVT_HANDLE) -> windows::core::Result<String> {
    render_handle(None, event, EvtRenderEventXml.0)
}

/// Two-call `EvtRender`: size probe, then render into a byte buffer decoded to
/// a `String`.
fn render_handle(
    context: Option<EVT_HANDLE>,
    fragment: EVT_HANDLE,
    flags: u32,
) -> windows::core::Result<String> {
    let mut used = 0u32;
    let mut count = 0u32;
    // Probe for the required byte size (expected to fail with ERROR_INSUFFICIENT_BUFFER).
    let _ = unsafe { EvtRender(context, fragment, flags, 0, None, &mut used, &mut count) };
    if used == 0 {
        return Ok(String::new());
    }
    let mut buf = vec![0u8; used as usize];
    unsafe {
        EvtRender(
            context,
            fragment,
            flags,
            used,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            &mut used,
            &mut count,
        )?;
    }
    Ok(utf16_bytes_to_string(&buf))
}

/// Enrich with the human-readable message via `EvtFormatMessage`, if a
/// publisher manifest exists; `None` otherwise (the pure parser synthesizes a
/// body). Best-effort — never fails the event.
fn format_message(event: EVT_HANDLE) -> Option<String> {
    // v1: rely on the pure parser's synthesized body. Publisher-metadata
    // enrichment via EvtOpenPublisherMetadata + EvtFormatMessage is a follow-up
    // (documented); witness never drops an event for lack of a manifest.
    let _ = event;
    None
}

/// Decode a UTF-16LE byte buffer (as produced by `EvtRender`) to a `String`,
/// trimming a trailing NUL.
fn utf16_bytes_to_string(bytes: &[u8]) -> String {
    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&u16s)
}

/// Whether a `windows` error carries a specific Win32 status code.
fn is_win32(e: &windows::core::Error, code: WIN32_ERROR) -> bool {
    e.code() == HRESULT::from_win32(code.0)
}

/// Sleep for `dur`, returning `true` if cancellation arrived first. Polls the
/// watch in short steps so shutdown stays prompt on a blocking thread.
fn sleep_cancellable(dur: Duration, cancel: &watch::Receiver<bool>) -> bool {
    const STEP: Duration = Duration::from_millis(50);
    let mut remaining = dur;
    while remaining > Duration::ZERO {
        if *cancel.borrow() {
            return true;
        }
        let step = remaining.min(STEP);
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    *cancel.borrow()
}
