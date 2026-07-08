//! Multiline aggregation tests (spec 008). Exercises the record-gated offset
//! invariant (R3), the StopReason drain paths (R7), the inactivity timeout (R4),
//! the byte cap (R5), CRLF/BOM normalization (R6), and copytruncate reset (R6).

use std::fs::File;
use std::io::Write;

use super::multiline::{self, MultilineOpts};
use super::structured::FileParseOpts;
use super::watcher::{self, FileId, TailedFile};
use crate::sink::{Capture, Recorded, Sink};

/// Plain tailing: no envelope/structure/level detection, so a shipped record's
/// body is exactly the joined line text.
const PLAIN: FileParseOpts = FileParseOpts {
    syslog: false,
    structured: false,
    levels: false,
};

fn file_id(path: &std::path::Path) -> FileId {
    #[cfg(unix)]
    {
        FileId::from_metadata(&std::fs::metadata(path).unwrap())
    }
    #[cfg(target_os = "windows")]
    {
        FileId::from_file(&File::open(path).unwrap(), path)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        FileId::from_path(path)
    }
}

fn make_tailed(path: &std::path::Path) -> TailedFile {
    TailedFile {
        path: path.to_path_buf(),
        pos: 0,
        id: file_id(path),
        partial: String::new(),
        open_failures: 0,
        retained_fd: None,
        skip_utf16: false,
        agg: None,
    }
}

fn capture() -> (Capture, Sink) {
    let cap = Capture::new();
    (cap.clone(), Sink::capture(cap, Default::default()))
}

fn bodies(cap: &Capture) -> Vec<String> {
    cap.events()
        .into_iter()
        .filter_map(|e| match e {
            Recorded::Log { message, .. } => Some(message),
            Recorded::Metric { .. } => None,
        })
        .collect()
}

fn opts(pattern: &str, timeout_ms: u64, max_bytes: usize) -> MultilineOpts {
    MultilineOpts::new(pattern, timeout_ms, max_bytes).unwrap()
}

fn write_temp(name: &str, contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

// --- R2: aggregation semantics ---

#[test]
fn test_java_stack_trace_joins_one_record() {
    // A log4j start line plus indented "at ..." continuations is one record; it
    // ships exactly once, when the next start line arrives.
    let log = b"2026-07-08 10:00:00 ERROR NullPointerException\n\
                \tat com.example.Foo.bar(Foo.java:42)\n\
                \tat com.example.Baz.qux(Baz.java:13)\n\
                2026-07-08 10:00:01 INFO recovered\n";
    let (_d, path) = write_temp("app.log", log);
    let ml = opts(r"^\d{4}-\d{2}-\d{2}", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    let consumed = multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);

    // Only the first (complete) record shipped; the second is still in flight.
    assert_eq!(
        bodies(&cap),
        vec![
            "2026-07-08 10:00:00 ERROR NullPointerException\n\
             \tat com.example.Foo.bar(Foo.java:42)\n\
             \tat com.example.Baz.qux(Baz.java:13)"
        ]
    );
    // pos advanced past exactly the three shipped lines, not the fourth: it
    // rests at the byte offset where the fourth (start-match) line begins.
    let fourth = b"2026-07-08 10:00:01 INFO recovered\n";
    assert_eq!(t.pos as usize, log.len() - fourth.len());
    assert_eq!(consumed as usize, log.len());

    // The trailing record ships on shutdown flush.
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap).len(), 2);
    assert_eq!(bodies(&cap)[1], "2026-07-08 10:00:01 INFO recovered");
}

#[test]
fn test_leading_lines_form_one_headerless_record() {
    // Continuation lines seen before any start-match accumulate into a single
    // leading record that flushes at the first start-match (spec 008 R2).
    let (_d, path) = write_temp("app.log", b"leadingA\nleadingB\nSTART x\nmore\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);

    assert_eq!(bodies(&cap), vec!["leadingA\nleadingB"]);
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["leadingA\nleadingB", "START x\nmore"]);
}

#[test]
fn test_aggregation_disabled_would_ship_each_line() {
    // Sanity: with a pattern matching EVERY line, each line is its own record,
    // shipped when the next arrives (mirrors the single-line path's granularity).
    let (_d, path) = write_temp("app.log", b"a\nb\nc\n");
    let ml = opts("^.", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    // "a" ships when "b" starts, "b" when "c" starts; "c" is in flight.
    assert_eq!(bodies(&cap), vec!["a", "b"]);
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["a", "b", "c"]);
}

// --- R4: inactivity timeout ---

#[test]
fn test_timeout_flush_emits_partial_group() {
    // A record with no following start line ships once its inactivity timeout
    // elapses (timeout_ms = 0 makes it immediately due).
    let (_d, path) = write_temp("app.log", b"START x\ncont1\ncont2\n");
    let ml = opts("^START", 0, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert!(bodies(&cap).is_empty(), "record still in flight after read");

    let remaining = multiline::flush_if_expired(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["START x\ncont1\ncont2"]);
    assert_eq!(remaining, None, "no record pending after the flush");
}

#[test]
fn test_flush_if_expired_reports_remaining_when_not_due() {
    let (_d, path) = write_temp("app.log", b"START x\ncont\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    let remaining = multiline::flush_if_expired(&mut t, &sink, PLAIN, &ml);
    assert!(bodies(&cap).is_empty(), "not yet due");
    assert!(remaining.is_some(), "reports time until the pending flush");
}

// --- R3: backpressure and offset invariants ---

#[test]
fn test_backpressure_midrecord_no_advance_then_reships() {
    // File holds two records. A full sink refuses the first record's ship when
    // the second start line arrives; pos stays 0 and nothing ships. On the next
    // poll with a working sink the same record ships identically.
    let (_d, path) = write_temp("app.log", b"START1\ncont1\nSTART2\ncont2\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let mut t = make_tailed(&path);

    // Poll 1: full sink — the record cannot ship.
    let full = Sink::full();
    multiline::read_live(File::open(&path).unwrap(), &mut t, &full, PLAIN, &ml);
    assert_eq!(t.pos, 0, "no shipped record → pos does not advance");

    // Poll 2: working sink — record1 re-ships from the retained in-flight state.
    let (cap, sink) = capture();
    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["START1\ncont1"]);
    assert_eq!(t.pos, 13, "pos advanced by the shipped record's full span");
}

#[test]
fn test_pos_advances_by_full_multibyte_span() {
    // A record with multi-byte UTF-8 content advances pos by on-disk byte length.
    let (_d, path) = write_temp("app.log", "STARTé\nx\nSTART2\n".as_bytes());
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["STARTé\nx"]);
    // "STARTé\n" = 8 bytes (é is 2), "x\n" = 2 → 10.
    assert_eq!(t.pos, 10);
}

// --- R6: copytruncate reset ---

#[test]
fn test_reset_discards_in_flight() {
    let (_d, path) = write_temp("app.log", b"START x\ncont\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert!(bodies(&cap).is_empty(), "record in flight");

    // A copytruncate resets pos to 0, then discards the buffered record.
    t.pos = 0;
    multiline::reset(&mut t);
    let agg = t.agg.as_ref().unwrap();
    assert!(agg.record.is_empty(), "in-flight record discarded");
    assert_eq!(agg.record_bytes, 0);
    assert_eq!(agg.read_ahead, 0, "read cursor reset to pos");
    assert!(t.partial.is_empty());

    // Nothing was shipped by the discard.
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert!(bodies(&cap).is_empty());
}

// --- R6: CRLF and BOM ---

#[test]
fn test_crlf_normalized_and_full_bytes_counted() {
    let (_d, path) = write_temp("app.log", b"START a\r\ncont\r\nSTART b\r\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    // No stray '\r' in the joined record.
    assert_eq!(bodies(&cap), vec!["START a\ncont"]);
    // pos advanced by the full on-disk span incl. CRLF: 9 + 6 = 15.
    assert_eq!(t.pos, 15);
}

#[test]
fn test_utf8_bom_stripped_and_start_matches() {
    let mut data = vec![0xEF, 0xBB, 0xBF];
    data.extend_from_slice(b"START head\ncont\nSTART next\n");
    let (_d, path) = write_temp("app.log", &data);
    // Anchored start pattern must still match despite the leading BOM.
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert_eq!(
        bodies(&cap),
        vec!["START head\ncont"],
        "BOM not in the record"
    );
}

// --- R5: byte cap ---

#[test]
fn test_byte_cap_truncates_at_char_boundary_pos_full_span() {
    // A record whose joined text exceeds the cap ships truncated at a char
    // boundary (never panicking), yet pos advances by the full on-disk span.
    let cap_bytes = 8;
    // "START\n" (6) then a multi-byte continuation "ééé\n" (7). Joined text
    // "START\nééé" is 11 bytes; truncation lands mid-"é" and must step back.
    let (_d, path) = write_temp("app.log", "START\nééé\nSTART2\n".as_bytes());
    let ml = opts("^START", 60_000, cap_bytes);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    let shipped = &bodies(&cap)[0];
    assert!(shipped.len() <= cap_bytes, "shipped text within the cap");
    assert!(
        shipped.is_char_boundary(shipped.len()),
        "valid UTF-8 truncation"
    );
    // "START\n" (6) + "ééé\n" (7) = 13 on-disk bytes, all consumed.
    assert_eq!(t.pos, 13);
}

// --- R7: StopReason drain paths ---

#[test]
fn test_drain_at_eof_flushes_final_record() {
    // A retained fd drained to EOF ships its final in-flight record and drops
    // the fd (StopReason::Eof).
    let (_d, path) = write_temp("old.log", b"START1\ncont\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);
    t.retained_fd = Some(File::open(&path).unwrap());

    let consumed = multiline::drain(&mut t, &sink, PLAIN, &ml);
    assert_eq!(
        bodies(&cap),
        vec!["START1\ncont"],
        "final record flushed at EOF"
    );
    assert!(t.retained_fd.is_none(), "fd dropped after a clean drain");
    assert_eq!(t.pos, 12);
    assert_eq!(consumed, 12);
}

#[test]
fn test_drain_at_backpressure_keeps_fd() {
    // A full sink during drain leaves the fd retained; nothing ships and pos
    // does not advance (StopReason::Backpressure).
    let (_d, path) = write_temp("old.log", b"START1\ncont\nSTART2\ncont2\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let mut t = make_tailed(&path);
    t.retained_fd = Some(File::open(&path).unwrap());

    let full = Sink::full();
    let consumed = multiline::drain(&mut t, &full, PLAIN, &ml);
    assert!(t.retained_fd.is_some(), "fd retained for the next poll");
    assert_eq!(t.pos, 0, "no shipped record → pos unchanged");
    // "START1\n" (7) + "cont\n" (5) consumed before the refused ship.
    assert_eq!(consumed, 12, "consumed START1+cont before the refused ship");
}

#[test]
fn test_drain_at_cap_keeps_fd() {
    // More than MAX_LINES_PER_POLL committable lines in one record forces the
    // per-poll cap (StopReason::Cap): the fd is retained and the still-incomplete
    // record is NOT force-shipped.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.log");
    {
        let mut f = File::create(&path).unwrap();
        writeln!(f, "START head").unwrap();
        for _ in 0..watcher::MAX_LINES_PER_POLL + 5 {
            writeln!(f, "c").unwrap();
        }
    }
    let ml = opts("^START", 60_000, 8 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);
    t.retained_fd = Some(File::open(&path).unwrap());

    multiline::drain(&mut t, &sink, PLAIN, &ml);
    assert!(
        t.retained_fd.is_some(),
        "fd retained after hitting the line cap"
    );
    assert!(
        bodies(&cap).is_empty(),
        "the incomplete record is not force-shipped at the cap"
    );
}

#[test]
fn test_drain_no_fd_returns_zero() {
    let (_d, path) = write_temp("x.log", b"data\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (_cap, sink) = capture();
    let mut t = make_tailed(&path);
    assert_eq!(multiline::drain(&mut t, &sink, PLAIN, &ml), 0);
}

// --- R7: flush_final ---

#[test]
fn test_flush_final_ships_pending_record() {
    let (_d, path) = write_temp("app.log", b"START x\ncont\n");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    assert!(bodies(&cap).is_empty());
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["START x\ncont"]);
}

#[test]
fn test_flush_final_folds_trailing_partial() {
    // A file whose last line has no trailing newline: the sub-line partial is
    // folded into the record on the final flush (R7).
    let (_d, path) = write_temp("app.log", b"START x\ntrailing no newline");
    let ml = opts("^START", 60_000, 1 << 20);
    let (cap, sink) = capture();
    let mut t = make_tailed(&path);

    multiline::read_live(File::open(&path).unwrap(), &mut t, &sink, PLAIN, &ml);
    multiline::flush_final(&mut t, &sink, PLAIN, &ml);
    assert_eq!(bodies(&cap), vec!["START x\ntrailing no newline"]);
}
