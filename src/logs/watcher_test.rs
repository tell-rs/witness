use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use super::watcher;
use super::watcher::{FileId, SavedOffset, TailedFile};
use crate::sink::{DryRun, Sink};

fn discard_sink() -> Sink {
    Sink::discard()
}

fn make_tailed(path: &std::path::Path) -> TailedFile {
    let meta = std::fs::metadata(path).unwrap();
    TailedFile {
        path: path.to_path_buf(),
        pos: 0,
        id: FileId::from_metadata(&meta),
        partial: String::new(),
        open_failures: 0,
        retained_fd: None,
        last_active: Instant::now(),
    }
}

// --- resolve_globs ---

#[test]
fn resolve_globs_literal_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("app.log");
    std::fs::write(&file, "line\n").unwrap();

    let patterns = vec![file.to_string_lossy().to_string()];
    let result = watcher::resolve_globs(&patterns);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], file);
}

#[test]
fn resolve_globs_missing_literal() {
    let patterns = vec!["/tmp/nonexistent_tell_test_file.log".to_string()];
    let result = watcher::resolve_globs(&patterns);
    assert!(result.is_empty());
}

#[test]
fn resolve_globs_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.log"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.log"), "b\n").unwrap();
    std::fs::write(dir.path().join("c.txt"), "c\n").unwrap();

    let pattern = format!("{}/*.log", dir.path().display());
    let result = watcher::resolve_globs(&[pattern]);
    assert_eq!(result.len(), 2);
    assert!(result.iter().all(|p| p.extension().unwrap() == "log"));
}

#[test]
fn resolve_globs_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let pattern = format!("{}/*.xyz", dir.path().display());
    let result = watcher::resolve_globs(&[pattern]);
    assert!(result.is_empty());
}

#[test]
fn resolve_globs_skips_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("subdir.log")).unwrap();
    std::fs::write(dir.path().join("real.log"), "data\n").unwrap();

    let pattern = format!("{}/*.log", dir.path().display());
    let result = watcher::resolve_globs(&[pattern]);
    assert_eq!(result.len(), 1);
    assert!(result[0].file_name().unwrap() == "real.log");
}

// --- emit_line ---

#[test]
fn emit_line_simple() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"hello world", &mut partial, &sink, false);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_with_partial() {
    let sink = discard_sink();
    let mut partial = "start of ".to_string();
    watcher::emit_line(b"line", &mut partial, &sink, false);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_empty_skipped() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"", &mut partial, &sink, false);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_whitespace_only_skipped() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"   \t  ", &mut partial, &sink, false);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_to_dry_run_sink() {
    let sink = Sink::dry_run(DryRun::new(), Default::default());
    let mut partial = String::new();
    watcher::emit_line(b"log message here", &mut partial, &sink, false);
    assert!(partial.is_empty());
}

// --- flush_partial ---

#[test]
fn flush_partial_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.log");
    std::fs::write(&file, "data\n").unwrap();

    let mut tailed = make_tailed(&file);
    tailed.partial = "buffered content".to_string();

    watcher::flush_partial(&mut tailed, &discard_sink());
    assert!(tailed.partial.is_empty());
}

#[test]
fn flush_partial_empty_noop() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.log");
    std::fs::write(&file, "data\n").unwrap();

    let mut tailed = make_tailed(&file);
    watcher::flush_partial(&mut tailed, &discard_sink());
    assert!(tailed.partial.is_empty());
}

#[test]
fn flush_partial_whitespace_only() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.log");
    std::fs::write(&file, "data\n").unwrap();

    let mut tailed = make_tailed(&file);
    tailed.partial = "   \n\t  ".to_string();

    watcher::flush_partial(&mut tailed, &discard_sink());
    assert!(tailed.partial.is_empty());
}

// --- read_lines ---

#[test]
fn read_lines_complete_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "line one\nline two\nline three\n").unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert_eq!(tailed.pos, bytes);
    assert!(tailed.partial.is_empty());
}

#[test]
fn read_lines_partial_line_buffered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    // No trailing newline — last chunk is a partial line
    std::fs::write(&path, "line one\nincomplete").unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert_eq!(tailed.partial, "incomplete");
}

#[test]
fn read_lines_from_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "skip this\nread this\n").unwrap();

    let mut tailed = make_tailed(&path);
    tailed.pos = 10; // skip "skip this\n"
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert_eq!(tailed.pos, 20); // "skip this\n" (10) + "read this\n" (10)
}

#[test]
fn read_lines_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.log");
    std::fs::write(&path, "").unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert_eq!(bytes, 0);
}

// --- register_file ---

#[test]
fn register_file_new() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, "content\n").unwrap();

    let mut files = HashMap::new();
    let mut path_index = HashMap::new();
    let saved = HashMap::new();

    watcher::register_file(&mut files, &mut path_index, &path, &saved);

    assert_eq!(files.len(), 1);
    assert_eq!(path_index.len(), 1);
    let tailed = files.values().next().unwrap();
    // Starts at EOF for new files
    assert_eq!(tailed.pos, 8); // "content\n" = 8 bytes
}

#[test]
fn register_file_with_saved_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, "0123456789").unwrap();

    let meta = std::fs::metadata(&path).unwrap();
    let id = FileId::from_metadata(&meta);

    let mut saved = HashMap::new();
    saved.insert(
        path.to_string_lossy().to_string(),
        SavedOffset {
            pos: 5,
            dev: id.dev,
            ino: id.ino,
        },
    );

    let mut files = HashMap::new();
    let mut path_index = HashMap::new();

    watcher::register_file(&mut files, &mut path_index, &path, &saved);

    let tailed = files.values().next().unwrap();
    assert_eq!(tailed.pos, 5); // Resumes from saved offset
}

#[test]
fn register_file_saved_offset_clamped_to_file_len() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, "short").unwrap(); // 5 bytes

    let meta = std::fs::metadata(&path).unwrap();
    let id = FileId::from_metadata(&meta);

    let mut saved = HashMap::new();
    saved.insert(
        path.to_string_lossy().to_string(),
        SavedOffset {
            pos: 999, // bigger than file
            dev: id.dev,
            ino: id.ino,
        },
    );

    let mut files = HashMap::new();
    let mut path_index = HashMap::new();

    watcher::register_file(&mut files, &mut path_index, &path, &saved);

    let tailed = files.values().next().unwrap();
    assert_eq!(tailed.pos, 5); // Clamped to file length
}

#[test]
fn register_file_duplicate_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, "content\n").unwrap();

    let mut files = HashMap::new();
    let mut path_index = HashMap::new();
    let saved = HashMap::new();

    watcher::register_file(&mut files, &mut path_index, &path, &saved);
    watcher::register_file(&mut files, &mut path_index, &path, &saved);

    assert_eq!(files.len(), 1); // Still just one entry
}

#[test]
fn register_file_nonexistent_ignored() {
    let mut files = HashMap::new();
    let mut path_index = HashMap::new();
    let saved = HashMap::new();

    watcher::register_file(
        &mut files,
        &mut path_index,
        std::path::Path::new("/tmp/tell_test_does_not_exist.log"),
        &saved,
    );

    assert!(files.is_empty());
}

// --- find_rotated_file ---

#[test]
fn find_rotated_file_with_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("app.log");
    let rotated = dir.path().join("app.log.1");

    std::fs::write(&original, "original content\n").unwrap();
    let original_meta = std::fs::metadata(&original).unwrap();
    let original_id = FileId::from_metadata(&original_meta);

    // Simulate rotation: rename original → .1, create new at original path
    std::fs::rename(&original, &rotated).unwrap();
    std::fs::write(&original, "new content\n").unwrap();

    // TailedFile still points to old path but with old inode
    let tailed = TailedFile {
        path: original,
        pos: 0,
        id: original_id,
        partial: String::new(),
        open_failures: 0,
        retained_fd: None,
        last_active: Instant::now(),
    };

    let found = watcher::find_rotated_file(&tailed);
    assert!(found.is_some());
}

#[test]
fn find_rotated_file_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, "content\n").unwrap();

    let tailed = TailedFile {
        path,
        pos: 0,
        id: FileId {
            dev: 0,
            ino: 999999999,
        }, // Bogus ID — won't match anything
        partial: String::new(),
        open_failures: 0,
        retained_fd: None,
        last_active: Instant::now(),
    };

    let found = watcher::find_rotated_file(&tailed);
    assert!(found.is_none());
}

// --- drain_retained ---

#[test]
fn drain_retained_reads_remaining_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.log");
    std::fs::write(&path, "line one\nline two\nline three\n").unwrap();

    let fd = std::fs::File::open(&path).unwrap();

    let mut tailed = TailedFile {
        path,
        pos: 0,
        id: FileId { dev: 0, ino: 0 },
        partial: String::new(),
        open_failures: 0,
        retained_fd: Some(fd),
        last_active: Instant::now(),
    };

    let bytes = watcher::drain_retained(&mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert!(tailed.retained_fd.is_none()); // Consumed
    assert!(tailed.partial.is_empty()); // Flushed
}

#[test]
fn drain_retained_no_fd_returns_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "data\n").unwrap();

    let mut tailed = make_tailed(&path);
    let bytes = watcher::drain_retained(&mut tailed, &discard_sink(), false);
    assert_eq!(bytes, 0);
}

#[test]
fn drain_retained_with_partial_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.log");
    // File ends without newline
    std::fs::write(&path, "line one\npartial").unwrap();

    let fd = std::fs::File::open(&path).unwrap();

    let mut tailed = TailedFile {
        path,
        pos: 0,
        id: FileId { dev: 0, ino: 0 },
        partial: String::new(),
        open_failures: 0,
        retained_fd: Some(fd),
        last_active: Instant::now(),
    };

    let bytes = watcher::drain_retained(&mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    // Partial should be flushed after drain
    assert!(tailed.partial.is_empty());
}

// --- tail_files integration ---

#[tokio::test]
async fn tail_files_reads_appended_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("test.log");
    std::fs::write(&log_path, "initial line\n").unwrap();

    let (tx, rx) = tokio::sync::watch::channel(false);
    let sink = Sink::dry_run(DryRun::new(), Default::default());
    let pattern = log_path.to_string_lossy().to_string();

    let handle = tokio::spawn({
        let sink = sink.clone();
        async move {
            watcher::tail_files(&[pattern], sink, rx, false).await;
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap();
        writeln!(f, "new line 1").unwrap();
        writeln!(f, "new line 2").unwrap();
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    tx.send(true).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn tail_files_no_matching_patterns() {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let sink = discard_sink();

    let handle = tokio::spawn(async move {
        watcher::tail_files(
            &["/tmp/tell_test_nonexistent_*.log".to_string()],
            sink,
            rx,
            false,
        )
        .await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    tx.send(true).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn tail_files_handles_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("trunc.log");
    std::fs::write(&log_path, "initial content\n").unwrap();

    let (tx, rx) = tokio::sync::watch::channel(false);
    let sink = discard_sink();
    let pattern = log_path.to_string_lossy().to_string();

    let handle = tokio::spawn(async move {
        watcher::tail_files(&[pattern], sink, rx, false).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    std::fs::write(&log_path, "after truncation\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    tx.send(true).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn tail_files_multiple_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let log1 = dir.path().join("app.log");
    let log2 = dir.path().join("err.log");
    std::fs::write(&log1, "app\n").unwrap();
    std::fs::write(&log2, "err\n").unwrap();

    let (tx, rx) = tokio::sync::watch::channel(false);
    let sink = discard_sink();
    let pattern = format!("{}/*.log", dir.path().display());

    let handle = tokio::spawn(async move {
        watcher::tail_files(&[pattern], sink, rx, false).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    {
        let mut f1 = std::fs::OpenOptions::new()
            .append(true)
            .open(&log1)
            .unwrap();
        writeln!(f1, "app line").unwrap();

        let mut f2 = std::fs::OpenOptions::new()
            .append(true)
            .open(&log2)
            .unwrap();
        writeln!(f2, "err line").unwrap();
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    tx.send(true).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn tail_files_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("rotate.log");
    std::fs::write(&log_path, "pre-rotation\n").unwrap();

    let (tx, rx) = tokio::sync::watch::channel(false);
    let sink = discard_sink();
    let pattern = log_path.to_string_lossy().to_string();

    let handle = tokio::spawn(async move {
        watcher::tail_files(&[pattern], sink, rx, false).await;
    });

    // Wait for registration at EOF
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Simulate rename rotation: move to .1, create new file
    let rotated = dir.path().join("rotate.log.1");
    std::fs::rename(&log_path, &rotated).unwrap();

    // Append to old file (now at .1) to exercise drain
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&rotated)
            .unwrap();
        writeln!(f, "old file trailing line").unwrap();
    }

    // Create new file at original path
    std::fs::write(&log_path, "new file first line\n").unwrap();

    // Wait for rotation detection + poll
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    tx.send(true).unwrap();
    handle.await.unwrap();
}

// --- try_emit_line ---

fn full_sink() -> Sink {
    Sink::full()
}

#[test]
fn try_emit_line_success_no_partial() {
    let sink = discard_sink();
    let mut partial = String::new();
    assert!(watcher::try_emit_line(
        b"hello world",
        &mut partial,
        &sink,
        false,
    ));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_success_clears_partial() {
    let sink = discard_sink();
    let mut partial = "start of ".to_string();
    assert!(watcher::try_emit_line(b"line", &mut partial, &sink, false,));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_empty_line_skipped() {
    let sink = full_sink();
    let mut partial = String::new();
    // Empty lines return true (skipped) even when sink is full.
    assert!(watcher::try_emit_line(b"", &mut partial, &sink, false));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_fail_no_partial() {
    let sink = full_sink();
    let mut partial = String::new();
    assert!(!watcher::try_emit_line(
        b"hello",
        &mut partial,
        &sink,
        false,
    ));
    // Partial must stay empty — we'll re-read from file.
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_fail_preserves_partial() {
    let sink = full_sink();
    let mut partial = "start of ".to_string();
    assert!(!watcher::try_emit_line(b"line", &mut partial, &sink, false,));
    // Partial must be unchanged so retry re-assembles correctly.
    assert_eq!(partial, "start of ");
}

#[test]
fn try_emit_line_retry_no_partial() {
    let full = full_sink();
    let ok = discard_sink();
    let mut partial = String::new();

    // First attempt fails.
    assert!(!watcher::try_emit_line(
        b"hello",
        &mut partial,
        &full,
        false,
    ));
    assert!(partial.is_empty());

    // Retry with same bytes succeeds — no duplication.
    assert!(watcher::try_emit_line(b"hello", &mut partial, &ok, false));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_retry_with_partial() {
    let full = full_sink();
    let ok = discard_sink();
    let mut partial = "start of ".to_string();

    // Fail — partial must be preserved.
    assert!(!watcher::try_emit_line(b"line", &mut partial, &full, false,));
    assert_eq!(partial, "start of ");

    // Retry with same bytes — assembles correctly, no duplication.
    assert!(watcher::try_emit_line(b"line", &mut partial, &ok, false));
    assert!(partial.is_empty());
}

// --- read_lines backpressure ---

#[test]
fn read_lines_backpressure_no_advance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &full_sink(), false);

    // Channel full: nothing should advance.
    assert_eq!(bytes, 0);
    assert_eq!(tailed.pos, 0);
    assert!(tailed.partial.is_empty());
}

#[test]
fn read_lines_backpressure_then_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();

    let mut tailed = make_tailed(&path);

    // Poll 1: full channel — nothing read.
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &full_sink(), false);
    assert_eq!(bytes, 0);
    assert_eq!(tailed.pos, 0);

    // Poll 2: channel drained — reads everything.
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert_eq!(tailed.pos, file_len);
}

#[test]
fn read_lines_backpressure_with_prior_partial() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    // Simulate: previous poll read "start of " (no newline), offset advanced past it.
    // This poll the file has " line\nline2\n" starting at the saved offset.
    std::fs::write(&path, "start of line\nline2\n").unwrap();

    let mut tailed = make_tailed(&path);
    // Simulate partial from previous poll.
    tailed.partial = "start of ".to_string();
    tailed.pos = 10; // Past "start of " (10 bytes including space)

    // Poll with full channel — assembled "start of line" can't be sent.
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &full_sink(), false);
    assert_eq!(bytes, 0);
    assert_eq!(tailed.pos, 10); // Unchanged
    assert_eq!(tailed.partial, "start of "); // Preserved

    // Retry — now it works.
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert!(bytes > 0);
    assert_eq!(tailed.pos, 20); // All consumed: "start of line\nline2\n"
    assert!(tailed.partial.is_empty());
}

#[test]
fn read_lines_backpressure_repeated_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.log");
    std::fs::write(&path, "aaa\nbbb\nccc\nddd\neee\n").unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();

    let mut tailed = make_tailed(&path);

    // Simulate 5 consecutive full-channel polls.
    for _ in 0..5 {
        let file = std::fs::File::open(&path).unwrap();
        let bytes = watcher::read_lines(file, &mut tailed, &full_sink(), false);
        assert_eq!(bytes, 0);
        assert_eq!(tailed.pos, 0);
    }

    // Now succeed — all 5 lines must come through, no duplication.
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), false);
    assert_eq!(bytes, file_len);
    assert_eq!(tailed.pos, file_len);
}

#[test]
fn read_lines_stress_100k() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.log");

    // Write 100K lines.
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..100_000u64 {
            writeln!(f, "log line number {i:06}").unwrap();
        }
    }
    let file_len = std::fs::metadata(&path).unwrap().len();

    let mut tailed = make_tailed(&path);
    let sink = discard_sink();
    let mut polls = 0u32;

    // Read in a loop until fully consumed.
    while tailed.pos < file_len {
        let file = std::fs::File::open(&path).unwrap();
        let bytes = watcher::read_lines(file, &mut tailed, &sink, false);
        assert!(bytes > 0, "stalled at pos {}", tailed.pos);
        polls += 1;
        assert!(polls < 100, "too many polls — infinite loop?");
    }

    assert_eq!(tailed.pos, file_len);
    assert!(tailed.partial.is_empty());
    // With 32K max per poll, 100K lines should take 4 polls.
    assert!((3..=5).contains(&polls), "unexpected poll count: {polls}");
}

/// Throughput measurement — not a pass/fail test, just prints numbers.
/// Run with: cargo test bench_read_lines_throughput -- --nocapture --ignored
#[test]
#[ignore]
fn bench_read_lines_throughput() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.log");
    let num_lines = 1_000_000u64;

    // Write 1M realistic log lines.
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..num_lines {
            writeln!(
                f,
                "2026-03-18T10:00:00Z INFO req={i:08} method=GET path=/api/v1/users status=200 ms=42"
            )
            .unwrap();
        }
    }
    let file_len = std::fs::metadata(&path).unwrap().len();
    let mb = file_len as f64 / 1_048_576.0;

    // Warm FS cache.
    let _ = std::fs::read(&path).unwrap();

    // --- Measure: discard sink (happy path, no backpressure) ---
    let sink = discard_sink();
    let start = Instant::now();
    let mut tailed = make_tailed(&path);
    let mut polls = 0u32;
    while tailed.pos < file_len {
        let file = std::fs::File::open(&path).unwrap();
        watcher::read_lines(file, &mut tailed, &sink, false);
        polls += 1;
    }
    let elapsed = start.elapsed();
    let secs = elapsed.as_secs_f64();
    eprintln!();
    eprintln!("  read_lines throughput (discard sink, {num_lines} lines, {mb:.1} MB):");
    eprintln!("    time:   {secs:.3}s");
    eprintln!(
        "    rate:   {:.0}K lines/sec",
        num_lines as f64 / secs / 1000.0
    );
    eprintln!("    disk:   {:.0} MB/s", mb / secs);
    eprintln!("    polls:  {polls}");

    // --- Measure: full sink (backpressure path) ---
    let full = full_sink();
    let start = Instant::now();
    let mut tailed = make_tailed(&path);
    for _ in 0..100 {
        let file = std::fs::File::open(&path).unwrap();
        watcher::read_lines(file, &mut tailed, &full, false);
    }
    let bp_elapsed = start.elapsed();
    eprintln!();
    eprintln!("  backpressure cost (100 rejected polls):");
    eprintln!("    time:   {:.3}s", bp_elapsed.as_secs_f64());
    eprintln!("    pos:    {} (should be 0)", tailed.pos);
    assert_eq!(tailed.pos, 0, "backpressure must not advance offset");
}

// --- Syslog integration ---

#[test]
fn emit_line_syslog_parsed() {
    let sink = discard_sink();
    let mut partial = String::new();
    let line = b"Apr 12 23:50:00 host sshd[1234]: Connection accepted from 10.0.0.1";
    watcher::emit_line(line, &mut partial, &sink, true);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_syslog_fallback_non_syslog() {
    // Non-syslog line with parse_syslog=true falls through to regular log()
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(
        b"plain log message no colon pattern",
        &mut partial,
        &sink,
        true,
    );
    assert!(partial.is_empty());
}

#[test]
fn emit_line_syslog_with_partial() {
    // Partial from a previous read gets assembled into a valid syslog line
    let sink = discard_sink();
    let mut partial = "Apr 12 23:50:00 host sshd[1234".to_string();
    watcher::emit_line(b"]: Connection accepted", &mut partial, &sink, true);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_syslog_empty_skipped() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"", &mut partial, &sink, true);
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_syslog_parsed() {
    let sink = discard_sink();
    let mut partial = String::new();
    let line = b"Apr 12 23:50:00 host sshd[1234]: Connection accepted";
    assert!(watcher::try_emit_line(line, &mut partial, &sink, true));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_syslog_non_syslog_falls_through() {
    // Non-syslog line with parse_syslog=true still succeeds via regular try_log
    let sink = discard_sink();
    let mut partial = String::new();
    assert!(watcher::try_emit_line(
        b"plain message no colon pattern",
        &mut partial,
        &sink,
        true,
    ));
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_syslog_full_sink() {
    // Syslog parse succeeds, but sink is full — returns false, partial unchanged
    let sink = full_sink();
    let mut partial = String::new();
    let line = b"Apr 12 23:50:00 host sshd[1234]: Connection accepted";
    assert!(!watcher::try_emit_line(line, &mut partial, &sink, true));
    // Partial stays empty — offset won't advance, file re-read on next poll
    assert!(partial.is_empty());
}

#[test]
fn try_emit_line_syslog_full_sink_with_partial() {
    // Assembled syslog line, sink full — partial preserved for retry
    let sink = full_sink();
    let mut partial = "Apr 12 23:50:00 host sshd[1234".to_string();
    assert!(!watcher::try_emit_line(
        b"]: Connection accepted",
        &mut partial,
        &sink,
        true,
    ));
    // Partial must be unchanged so retry re-assembles correctly
    assert_eq!(partial, "Apr 12 23:50:00 host sshd[1234");
}

#[test]
fn try_emit_line_syslog_retry_succeeds() {
    let full = full_sink();
    let ok = discard_sink();
    let mut partial = String::new();
    let line = b"Apr 12 23:50:00 host sshd[1234]: Connection accepted";

    // First attempt: sink full
    assert!(!watcher::try_emit_line(line, &mut partial, &full, true));
    assert!(partial.is_empty());

    // Retry: succeeds, no duplication
    assert!(watcher::try_emit_line(line, &mut partial, &ok, true));
    assert!(partial.is_empty());
}

#[test]
fn read_lines_syslog_complete_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("syslog");
    std::fs::write(
        &path,
        "Apr 12 23:50:00 host sshd[1]: accepted\nApr 12 23:50:01 host kernel: panic\n",
    )
    .unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), true);
    assert!(bytes > 0);
    assert!(tailed.partial.is_empty());
}

#[test]
fn read_lines_syslog_mixed_formats() {
    // Mix of syslog and non-syslog lines — both should be processed
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.log");
    std::fs::write(
        &path,
        "Apr 12 23:50:00 host sshd[1]: accepted\nplain application log line\n",
    )
    .unwrap();

    let mut tailed = make_tailed(&path);
    let file = std::fs::File::open(&path).unwrap();
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink(), true);
    assert!(bytes > 0);
    assert!(tailed.partial.is_empty());
}
