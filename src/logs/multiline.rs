//! Multiline record aggregation for the file tailer (spec 008).
//!
//! Consecutive physical lines are merged into one logical record when they
//! belong together (stack traces, pretty-printed JSON, multi-line panics). A
//! `multiline_start_pattern` regex marks the **start** of a record; every
//! following line that does not match is appended as a continuation until the
//! next start-match, an inactivity timeout, or a byte cap. The assembled record
//! ships as one [`Sink`] entry and is classified (syslog/structured/severity) as
//! a whole, using its first line for service extraction — so classification is
//! byte-identical to the single-line path when aggregation is off.
//!
//! **The record, not the line, gates offset advance** (the watcher's no-loss
//! invariant, R3): the persisted `pos` is the file offset of the *start of the
//! in-flight record* and advances by the record's full on-disk byte span only
//! when the whole record ships. The reader seeks to `read_ahead` (`pos` plus the
//! bytes already folded into the in-flight record and sub-line partial), so it
//! never re-reads consumed lines. In-flight state is in memory only; a crash
//! re-seeks to `pos` and re-derives the same unshipped record from the file.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::{Duration, Instant};

use regex_lite::Regex;

use super::structured::{FileParseOpts, classify_line};
use super::watcher::{MAX_LINES_PER_POLL, TailedFile, push_partial};
use crate::sink::Sink;

/// Compiled, shared multiline settings — built once per tailer in `tail_files`
/// and reused for every line (never compiled per line).
pub struct MultilineOpts {
    pattern: Regex,
    timeout: Duration,
    max_bytes: usize,
}

impl MultilineOpts {
    /// Compile a start pattern and bundle the timeout / byte-cap knobs.
    ///
    /// # Errors
    ///
    /// Returns the `regex-lite` compile error for an invalid pattern (the same
    /// error surfaced at startup by `config::parse_config`).
    pub fn new(
        pattern: &str,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Result<Self, regex_lite::Error> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
            timeout: Duration::from_millis(timeout_ms),
            max_bytes,
        })
    }
}

/// Per-file in-flight aggregation state (in memory, never persisted — R3).
#[derive(Default)]
pub(crate) struct Aggregator {
    /// Joined text of the in-flight record (empty when none).
    pub(crate) record: String,
    /// Total on-disk byte length of the lines folded into `record` — how much
    /// `pos` advances when the record ships (spec 008 R3/R5).
    pub(crate) record_bytes: u64,
    /// File offset the reader has consumed to: `pos + record_bytes +
    /// partial_bytes`. Reads seek here, not `pos`.
    pub(crate) read_ahead: u64,
    /// On-disk byte length of the current sub-line `partial` (bytes read with no
    /// trailing newline yet), tracked separately from the lossy string length.
    pub(crate) partial_bytes: u64,
    /// Instant of the most recent append, for the inactivity timeout.
    pub(crate) last_append: Option<Instant>,
}

impl Aggregator {
    /// A fresh aggregator whose read cursor starts at `pos`.
    pub(crate) fn new(pos: u64) -> Self {
        Self {
            read_ahead: pos,
            ..Self::default()
        }
    }

    /// Whether a record (possibly all-whitespace) is buffered.
    fn has_record(&self) -> bool {
        self.record_bytes > 0
    }
}

/// Outcome of committing one complete physical line.
enum Commit {
    /// The line was consumed into the record.
    Ok,
    /// Shipping the preceding record hit backpressure — nothing was consumed;
    /// stop the poll and retry the same record next time (spec 008 R3).
    Backpressure,
}

/// Read new content from `file` (opened at the current path), aggregating into
/// records and shipping completed ones. Returns bytes consumed this poll (for
/// the catchup heuristic). `pos` advances only when a record ships.
pub(crate) fn read_live(
    file: File,
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) -> u64 {
    ensure_agg(tailed);
    let read_from = tailed.agg.as_ref().map_or(tailed.pos, |a| a.read_ahead);
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(read_from)).is_err() {
        return 0;
    }
    // A live read never force-flushes: on EOF the in-flight record stays buffered
    // (more lines may still arrive); it ships on the next start-match, the
    // inactivity timeout (R4), rotation, or shutdown (R7). Only `consumed`
    // matters here — the stop reason is not actionable for a live read.
    read_loop(&mut reader, tailed, sink, opts, ml).consumed
}

/// Drain the retained fd of a rotated-out file, aggregating and flushing the
/// final record at EOF (spec 008 R7). On backpressure the fd is put back and the
/// drain resumes next poll; at a clean EOF the fd is dropped so the caller
/// removes the entry.
pub(crate) fn drain(
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) -> u64 {
    let Some(file) = tailed.retained_fd.take() else {
        return 0;
    };
    ensure_agg(tailed);
    let read_from = tailed.agg.as_ref().map_or(tailed.pos, |a| a.read_ahead);
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(read_from)).is_err() {
        return 0;
    }

    let outcome = read_loop(&mut reader, tailed, sink, opts, ml);

    match outcome.reason {
        // Backpressure (a record ship was refused) or the per-poll line cap:
        // keep the fd positioned mid-file and resume the drain next poll. Do
        // NOT fold/ship — the in-flight record is incomplete (spec 008 R3/R7).
        StopReason::Backpressure | StopReason::Cap => {
            tailed.retained_fd = Some(reader.into_inner());
            outcome.consumed
        }
        // Clean EOF: the old file is fully read. Fold any trailing sub-line
        // partial into the record and ship the final record (R7).
        StopReason::Eof => {
            fold_partial(tailed, ml);
            if !ship(tailed, sink, opts) {
                // Channel full during the final flush — keep the fd (positioned
                // at EOF) and retry the flush next poll without re-reading (R3).
                tailed.retained_fd = Some(reader.into_inner());
            }
            outcome.consumed
        }
    }
}

/// Why [`read_loop`] stopped, so callers can distinguish "the record is complete
/// and may be flushed" (`Eof`) from "keep the fd and resume, the record is still
/// in progress" (`Backpressure`/`Cap`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    /// Reached end of file (or a read error): no more bytes available now.
    Eof,
    /// A record ship was refused by a full channel — retry the same record.
    Backpressure,
    /// Hit the per-poll line cap ([`MAX_LINES_PER_POLL`]) — resume next poll.
    Cap,
}

/// The result of one [`read_loop`] pass: bytes consumed and why it stopped.
struct ReadOutcome {
    consumed: u64,
    reason: StopReason,
}

/// Read complete physical lines from `reader`, committing each into the in-flight
/// record. Stops at EOF, on backpressure, or after [`MAX_LINES_PER_POLL`],
/// reporting which via [`ReadOutcome::reason`] so the caller knows whether the
/// record may be flushed (`Eof`) or must be resumed (`Backpressure`/`Cap`).
fn read_loop(
    reader: &mut BufReader<File>,
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) -> ReadOutcome {
    let mut consumed = 0u64;
    let mut lines = 0usize;
    let mut buf = Vec::new();
    let reason = loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break StopReason::Eof,
            Ok(n) => {
                if buf.last() == Some(&b'\n') {
                    match commit_line(tailed, &buf[..n - 1], n as u64, sink, opts, ml) {
                        Commit::Ok => {
                            consumed += n as u64;
                            lines += 1;
                            if lines >= MAX_LINES_PER_POLL {
                                break StopReason::Cap;
                            }
                        }
                        Commit::Backpressure => break StopReason::Backpressure,
                    }
                } else if let Some(agg) = tailed.agg.as_mut() {
                    // Sub-line partial: buffer it, advance read_ahead only.
                    agg.read_ahead += n as u64;
                    agg.partial_bytes += n as u64;
                    consumed += n as u64;
                    push_partial(&mut tailed.partial, &buf);
                }
            }
            // A read error is treated like EOF: stop and (in a drain) flush what
            // we have, matching the single-line drain's error handling.
            Err(_) => break StopReason::Eof,
        }
    };
    ReadOutcome { consumed, reason }
}

/// Commit one complete physical line into the in-flight record. A start-match
/// first flushes the current record (subject to backpressure); non-matching
/// lines append. Only mutates offset/record state after the flush (if any)
/// succeeds, so a backpressured flush leaves everything intact for retry.
fn commit_line(
    tailed: &mut TailedFile,
    line_bytes: &[u8],
    n: u64,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) -> Commit {
    let line_lossy = String::from_utf8_lossy(line_bytes);
    let complete: String = if tailed.partial.is_empty() {
        line_lossy.into_owned()
    } else {
        let mut s = String::with_capacity(tailed.partial.len() + line_lossy.len());
        s.push_str(&tailed.partial);
        s.push_str(&line_lossy);
        s
    };
    let normalized = normalize_line(&complete);
    let is_start = !normalized.is_empty() && ml.pattern.is_match(normalized);

    // On-disk span of this physical line = prior partial bytes + this chunk.
    let line_span = tailed.agg.as_ref().map_or(0, |a| a.partial_bytes) + n;

    if is_start
        && tailed.agg.as_ref().is_some_and(Aggregator::has_record)
        && !ship(tailed, sink, opts)
    {
        // Backpressure: do NOT consume this line — read_ahead unchanged and the
        // partial buffer untouched, so the next poll re-reads it identically.
        return Commit::Backpressure;
    }

    let Some(agg) = tailed.agg.as_mut() else {
        return Commit::Backpressure;
    };
    agg.read_ahead += n;
    agg.partial_bytes = 0;
    tailed.partial.clear();
    append_to_record(agg, normalized, line_span, ml.max_bytes);
    agg.last_append = Some(Instant::now());
    Commit::Ok
}

/// Ship the in-flight record as one entry (spec 008 R3). On success advances
/// `pos` by `record_bytes` and clears the record; on backpressure returns
/// `false` leaving everything intact. An all-whitespace record is a no-op that
/// still advances `pos` (mirrors the single-line empty-line skip).
pub(crate) fn ship(tailed: &mut TailedFile, sink: &Sink, opts: FileParseOpts) -> bool {
    let Some(agg) = tailed.agg.as_mut() else {
        return true;
    };
    if !agg.has_record() {
        return true;
    }

    if agg.record.trim().is_empty() {
        tailed.pos += agg.record_bytes;
        clear_record(agg);
        return true;
    }

    let ok = {
        let classified = classify_line(&agg.record, opts);
        sink.try_log_with_service(
            classified.level,
            &classified.body,
            None,
            classified.service,
            classified.payload,
        )
    };
    if ok {
        let span = agg.record_bytes;
        clear_record(agg);
        tailed.pos += span;
    }
    ok
}

/// Flush any in-flight record at shutdown (spec 008 R7): fold a trailing
/// sub-line partial in, then ship. A failed ship leaves `pos` at the record
/// start so a restart re-derives and re-ships (no loss).
pub(crate) fn flush_final(
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) {
    fold_partial(tailed, ml);
    let _ = ship(tailed, sink, opts);
}

/// Ship any record whose inactivity timeout has elapsed, and return the
/// smallest remaining time to a pending record's timeout (for the poll-delay
/// cap, spec 008 R4). `None` when no record is pending.
pub(crate) fn flush_if_expired(
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: &MultilineOpts,
) -> Option<Duration> {
    let elapsed = tailed.agg.as_ref().and_then(|a| {
        if a.has_record() {
            Some(a.last_append.map_or(Duration::ZERO, |i| i.elapsed()))
        } else {
            None
        }
    })?;

    if elapsed >= ml.timeout {
        ship(tailed, sink, opts);
    }

    // Recompute after the possible ship — a shipped record is gone.
    tailed.agg.as_ref().and_then(|a| {
        if a.has_record() {
            let el = a.last_append.map_or(Duration::ZERO, |i| i.elapsed());
            Some(ml.timeout.saturating_sub(el))
        } else {
            None
        }
    })
}

/// Discard the in-flight record and reset the read cursor to `pos` (spec 008
/// R7). Used on copytruncate (buffered bytes are gone from the file) and rename
/// rotation (the retained-fd drain re-derives the record from `pos`).
pub(crate) fn reset(tailed: &mut TailedFile) {
    tailed.partial.clear();
    if let Some(agg) = tailed.agg.as_mut() {
        *agg = Aggregator::new(tailed.pos);
    }
}

/// Lazily create the aggregator for a file, its cursor starting at `pos`.
fn ensure_agg(tailed: &mut TailedFile) {
    if tailed.agg.is_none() {
        tailed.agg = Some(Aggregator::new(tailed.pos));
    }
}

/// Fold a trailing sub-line partial into the record as a final line.
fn fold_partial(tailed: &mut TailedFile, ml: &MultilineOpts) {
    if tailed.partial.is_empty() {
        return;
    }
    let line = std::mem::take(&mut tailed.partial);
    let normalized = normalize_line(&line);
    if let Some(agg) = tailed.agg.as_mut() {
        let span = agg.partial_bytes;
        agg.partial_bytes = 0;
        append_to_record(agg, normalized, span, ml.max_bytes);
    }
}

/// Append a normalized line to the record, joined with `\n`, applying the
/// drop-excess char-boundary byte cap (spec 008 R5). `record_bytes` accounts for
/// the full on-disk span even when the shipped text is truncated.
fn append_to_record(agg: &mut Aggregator, line: &str, line_span: u64, max_bytes: usize) {
    if !agg.record.is_empty() {
        agg.record.push('\n');
    }
    agg.record.push_str(line);
    if agg.record.len() > max_bytes {
        let boundary = agg.record.floor_char_boundary(max_bytes);
        agg.record.truncate(boundary);
    }
    agg.record_bytes += line_span;
}

/// Clear a shipped/discarded record (leaves `read_ahead` and `partial_bytes`
/// alone — the reader position is unchanged).
fn clear_record(agg: &mut Aggregator) {
    agg.record.clear();
    agg.record_bytes = 0;
    agg.last_append = None;
}

/// Normalize a physical line the same way the single-line path does before
/// matching/appending (spec 008 R6): strip trailing CRLF/whitespace and a
/// leading UTF-8 BOM so the start regex sees clean text and the joined record
/// contains no stray `\r`/BOM.
fn normalize_line(line: &str) -> &str {
    let trimmed = line.trim_end();
    trimmed.strip_prefix('\u{feff}').unwrap_or(trimmed)
}
