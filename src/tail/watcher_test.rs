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
    watcher::emit_line(b"hello world", &mut partial, "test.log", &sink);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_with_partial() {
    let sink = discard_sink();
    let mut partial = "start of ".to_string();
    watcher::emit_line(b"line", &mut partial, "test.log", &sink);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_empty_skipped() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"", &mut partial, "test.log", &sink);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_whitespace_only_skipped() {
    let sink = discard_sink();
    let mut partial = String::new();
    watcher::emit_line(b"   \t  ", &mut partial, "test.log", &sink);
    assert!(partial.is_empty());
}

#[test]
fn emit_line_to_dry_run_sink() {
    let sink = Sink::dry_run(DryRun::new(), Default::default());
    let mut partial = String::new();
    watcher::emit_line(b"log message here", &mut partial, "app.log", &sink);
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
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink());
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
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink());
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
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink());
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
    let bytes = watcher::read_lines(file, &mut tailed, &discard_sink());
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
        path: original.clone(),
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
        path: path.clone(),
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
        path: path.clone(),
        pos: 0,
        id: FileId { dev: 0, ino: 0 },
        partial: String::new(),
        open_failures: 0,
        retained_fd: Some(fd),
        last_active: Instant::now(),
    };

    let bytes = watcher::drain_retained(&mut tailed, &discard_sink());
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
    let bytes = watcher::drain_retained(&mut tailed, &discard_sink());
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
        path: path.clone(),
        pos: 0,
        id: FileId { dev: 0, ino: 0 },
        partial: String::new(),
        open_failures: 0,
        retained_fd: Some(fd),
        last_active: Instant::now(),
    };

    let bytes = watcher::drain_retained(&mut tailed, &discard_sink());
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
            watcher::tail_files(&[pattern], sink, rx).await;
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
        watcher::tail_files(&["/tmp/tell_test_nonexistent_*.log".to_string()], sink, rx).await;
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
        watcher::tail_files(&[pattern], sink, rx).await;
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
        watcher::tail_files(&[pattern], sink, rx).await;
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
        watcher::tail_files(&[pattern], sink, rx).await;
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
