//! File tailer — polls log files and ships new lines via Tell SDK.
//!
//! Design decisions (learned from Vector):
//! - **Polling with backoff**, not inotify/kqueue. More reliable — inotify can
//!   miss events under kernel queue pressure. For log shipping, 250ms latency
//!   is irrelevant.
//! - **Files tracked by (dev, inode)**, not path. This is the key to correct
//!   rotation handling — when logrotate renames syslog→syslog.1, we keep
//!   reading the OLD inode to EOF before switching.
//! - **Retained file descriptors** — on rename rotation, we locate the rotated
//!   file by inode (common suffixes, then a directory scan), open it, and hold
//!   that fd until it is drained to EOF. The kernel keeps the file alive as
//!   long as the fd is open, even if the path is unlinked meanwhile.
//! - **Partial line buffering** — hold bytes until a newline delimiter is seen.
//!   Prevents emitting truncated lines when reading mid-write.
//! - **Atomic checkpoints** — write → fsync → rename for crash safety.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use tracing::{info, warn};

use crate::sink::Sink;

/// How often to poll files for new content (ms). Backs off when idle.
const POLL_BASE_MS: u64 = 250;
/// Maximum backoff when all files are idle (ms).
const POLL_MAX_MS: u64 = 2000;
/// How often to re-scan glob patterns for new files (seconds).
const GLOB_RESCAN_SECS: u64 = 10;
/// How often to save offsets to disk (seconds).
const OFFSET_SAVE_SECS: u64 = 10;
/// Remove a file entry after this many consecutive open failures.
const STALE_THRESHOLD: u32 = 30;
/// Skip files not modified in this many seconds during discovery (24h).
const IGNORE_OLDER_SECS: u64 = 86400;
/// Minimum seconds between offset saves while actively shipping data.
/// Bounds fsync frequency during catch-up; the crash-recovery duplicate
/// window stays at most this long.
const OFFSET_SAVE_MIN_SECS: u64 = 1;
/// Maximum partial line buffer size (1 MB). Lines longer than this are truncated
/// to prevent OOM from binary/malformed log files.
const MAX_PARTIAL_BYTES: usize = 1024 * 1024;
/// Maximum lines to read per poll cycle. Yield back to the async loop after
/// this many lines so the runtime can service other tasks. The primary
/// backpressure mechanism is try_log() returning false (channel full), not
/// this cap — it only prevents monopolising the executor under sustained load.
const MAX_LINES_PER_POLL: usize = 32_000;
/// When draining a large backlog (pos far behind file end), use faster 50ms
/// polls instead of the default 250ms base.
const POLL_CATCHUP_MS: u64 = 50;
/// Offset state file name (lives under the platform state directory).
const STATE_FILE_NAME: &str = "offsets";

fn state_file_path() -> std::path::PathBuf {
    std::path::Path::new(crate::config::state_dir()).join(STATE_FILE_NAME)
}

// --- File identity ---

/// Unique file identity based on device + inode (Unix) or path hash (non-Unix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileId {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

impl FileId {
    #[cfg(unix)]
    pub(crate) fn from_metadata(m: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: m.dev(),
            ino: m.ino(),
        }
    }

    #[cfg(not(unix))]
    fn from_path(path: &Path) -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        Self {
            dev: 0,
            ino: h.finish(),
        }
    }
}

// --- Per-file state ---

pub(crate) struct TailedFile {
    /// Current path (may change on rotation).
    pub(crate) path: PathBuf,
    /// Read position in bytes.
    pub(crate) pos: u64,
    /// File identity for rotation detection.
    pub(crate) id: FileId,
    /// Partial line buffer — bytes read but no newline yet.
    pub(crate) partial: String,
    /// Consecutive open failures (for stale cleanup).
    pub(crate) open_failures: u32,
    /// Retained file descriptor for draining after rotation.
    pub(crate) retained_fd: Option<File>,
}

// --- Main loop ---

/// Tail log files. Blocks until cancellation.
pub async fn tail_files(
    patterns: &[String],
    sink: Sink,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    parse_syslog: bool,
) {
    let saved = load_offsets();
    let mut files: HashMap<FileId, TailedFile> = HashMap::new();
    // Map from path → FileId for quick lookup during glob scan.
    let mut path_index: HashMap<PathBuf, FileId> = HashMap::new();

    // Initial discovery
    for path in resolve_globs(patterns) {
        register_file(&mut files, &mut path_index, &path, &saved, None);
    }

    if files.is_empty() {
        warn!("tail: no files matched configured patterns");
    }

    let mut poll_interval_ms = POLL_BASE_MS;
    let mut last_save = Instant::now();
    let mut glob_interval = tokio::time::interval(std::time::Duration::from_secs(GLOB_RESCAN_SECS));
    let mut save_interval = tokio::time::interval(std::time::Duration::from_secs(OFFSET_SAVE_SECS));
    glob_interval.tick().await;
    save_interval.tick().await;

    loop {
        tokio::select! {
            // Poll all files
            _ = tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)) => {
                let total_bytes = poll_all(&mut files, &mut path_index, &sink, parse_syslog);

                // Fast catchup when data is flowing, backoff when idle
                if total_bytes > 0 {
                    // Use fast poll when actively draining to maximize throughput.
                    // 50ms polls * 32,000 lines/poll bounds the per-cycle work.
                    poll_interval_ms = POLL_CATCHUP_MS;
                    // Save offsets soon after shipping lines to keep the crash
                    // duplicate window small, but rate-limit the fsync so a
                    // catch-up burst doesn't hit the disk every 50ms.
                    if last_save.elapsed().as_secs() >= OFFSET_SAVE_MIN_SECS {
                        save_offsets(&files);
                        last_save = Instant::now();
                    }
                } else {
                    // No data — back off to reduce CPU when idle
                    poll_interval_ms = (poll_interval_ms * 2).min(POLL_MAX_MS);
                }

                // Clean up entries whose file disappeared (repeated open failures)
                let removable: Vec<FileId> = files.iter()
                    .filter(|(_, f)| f.open_failures >= STALE_THRESHOLD && f.retained_fd.is_none())
                    .map(|(&id, _)| id)
                    .collect();
                for id in removable {
                    if let Some(f) = files.remove(&id) {
                        if path_index.get(&f.path) == Some(&f.id) {
                            path_index.remove(&f.path);
                        }
                        info!(path = %f.path.display(), "tail: evicted");
                    }
                }
            }

            // Discover new files
            _ = glob_interval.tick() => {
                for path in resolve_globs(patterns) {
                    if !path_index.contains_key(&path) {
                        register_file(&mut files, &mut path_index, &path, &HashMap::new(), None);
                        info!(path = %path.display(), "tail: discovered");
                    }
                }
            }

            // Persist offsets
            _ = save_interval.tick() => {
                save_offsets(&files);
                last_save = Instant::now();
            }

            // Shutdown
            _ = cancel.changed() => {
                // Final drain — read all remaining lines including retained fds
                poll_all(&mut files, &mut path_index, &sink, parse_syslog);
                for f in files.values_mut() {
                    flush_partial(f, &sink);
                }
                save_offsets(&files);
                return;
            }
        }
    }
}

// --- File registration ---

/// Track a file. `initial_pos` overrides the starting offset — `Some(0)` for
/// files that replace a rotated one (their entire content is new data);
/// `None` for discovery, which resumes from a saved offset or tails from EOF.
pub(crate) fn register_file(
    files: &mut HashMap<FileId, TailedFile>,
    path_index: &mut HashMap<PathBuf, FileId>,
    path: &Path,
    saved: &HashMap<String, SavedOffset>,
    initial_pos: Option<u64>,
) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };

    #[cfg(unix)]
    let id = FileId::from_metadata(&metadata);
    #[cfg(not(unix))]
    let id = FileId::from_path(path);

    // Already tracking this inode
    if files.contains_key(&id) {
        return;
    }

    let has_checkpoint = saved
        .get(&path.to_string_lossy().to_string())
        .is_some_and(|s| s.dev == id.dev && s.ino == id.ino);

    // Skip files not modified recently (unless we have a checkpoint for them,
    // or they replace a rotated file and must be read from the start)
    if initial_pos.is_none()
        && !has_checkpoint
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = SystemTime::now().duration_since(modified)
        && age.as_secs() > IGNORE_OLDER_SECS
    {
        return;
    }

    let pos = initial_pos.unwrap_or_else(|| {
        saved
            .get(&path.to_string_lossy().to_string())
            .filter(|s| s.dev == id.dev && s.ino == id.ino)
            .map(|s| s.pos.min(metadata.len()))
            .unwrap_or(metadata.len())
    });

    path_index.insert(path.to_path_buf(), id);
    files.insert(
        id,
        TailedFile {
            path: path.to_path_buf(),
            pos,
            id,
            partial: String::new(),
            open_failures: 0,
            retained_fd: None,
        },
    );
}

// --- Polling ---

/// Poll all tracked files. Returns total bytes read across all files.
fn poll_all(
    files: &mut HashMap<FileId, TailedFile>,
    path_index: &mut HashMap<PathBuf, FileId>,
    sink: &Sink,
    parse_syslog: bool,
) -> u64 {
    let mut total_bytes = 0u64;

    let ids: Vec<FileId> = files.keys().copied().collect();

    for id in ids {
        let Some(tailed) = files.get_mut(&id) else {
            continue;
        };

        // Entries with a retained fd are rotated-out files: their only job is
        // draining to EOF. The path is owned by the new-inode entry registered
        // at rotation time, so once drained, remove the entry.
        if tailed.retained_fd.is_some() {
            total_bytes += drain_retained(tailed, sink, parse_syslog);
            if tailed.retained_fd.is_none()
                && let Some(f) = files.remove(&id)
                && path_index.get(&f.path) == Some(&f.id)
            {
                path_index.remove(&f.path);
            }
            continue;
        }

        match check_and_read(tailed, sink, parse_syslog) {
            ReadResult::Bytes(n) => {
                tailed.open_failures = 0;
                total_bytes += n;
            }
            ReadResult::Rotated { old_fd } => {
                // Path now points to a different inode.
                // Retain the old fd so we can drain remaining lines.
                tailed.retained_fd = Some(old_fd);
                tailed.open_failures = 0;

                // Register the new file at this path, reading from the start —
                // everything in it was written after rotation.
                let path = tailed.path.clone();
                if path_index.get(&path) == Some(&id) {
                    path_index.remove(&path);
                }
                register_file(files, path_index, &path, &HashMap::new(), Some(0));
            }
            ReadResult::RotatedLost => {
                // Old inode vanished before we could drain it (deleted or
                // compressed). Accept the loss; track the new file from the
                // start — everything in it was written after rotation.
                let path = tailed.path.clone();
                files.remove(&id);
                path_index.remove(&path);
                register_file(files, path_index, &path, &HashMap::new(), Some(0));
            }
            ReadResult::OpenFailed => {
                tailed.open_failures += 1;
            }
            ReadResult::Idle => {}
        }
    }

    total_bytes
}

enum ReadResult {
    /// Read N bytes of new content.
    Bytes(u64),
    /// Path now points to a different inode. Old fd returned for draining.
    Rotated { old_fd: File },
    /// Path points to a different inode and the old file cannot be found.
    RotatedLost,
    /// File could not be opened.
    OpenFailed,
    /// No new data.
    Idle,
}

fn check_and_read(tailed: &mut TailedFile, sink: &Sink, parse_syslog: bool) -> ReadResult {
    let Ok(file) = File::open(&tailed.path) else {
        return ReadResult::OpenFailed;
    };

    let Ok(metadata) = file.metadata() else {
        return ReadResult::OpenFailed;
    };

    #[cfg(unix)]
    let current_id = FileId::from_metadata(&metadata);
    #[cfg(not(unix))]
    let current_id = FileId::from_path(&tailed.path);

    // Rotation detected — the path now points to a different inode, so the
    // fd we just opened is the NEW file. We open by path each poll, so we
    // never held the old file's fd; find the rotated original by inode
    // (common suffixes like `.1`, then a directory scan) and hand its fd to
    // the caller for draining. The caller registers the new file separately.
    if current_id != tailed.id {
        flush_partial(tailed, sink);

        if let Some(old_fd) = find_rotated_file(tailed) {
            return ReadResult::Rotated { old_fd };
        }

        return ReadResult::RotatedLost;
    }

    let current_len = metadata.len();

    // Detect truncation (copytruncate rotation)
    if current_len < tailed.pos {
        flush_partial(tailed, sink);
        tailed.pos = 0;
    }

    if current_len <= tailed.pos {
        return ReadResult::Idle;
    }

    let bytes_read = read_lines(file, tailed, sink, parse_syslog);
    if bytes_read > 0 {
        ReadResult::Bytes(bytes_read)
    } else {
        ReadResult::Idle
    }
}

/// Try to find the rotated version of a file.
/// Checks common logrotate suffixes: .1, .0, -YYYYMMDD, etc.
pub(crate) fn find_rotated_file(tailed: &TailedFile) -> Option<File> {
    let path_str = tailed.path.to_string_lossy();

    // Common rotation suffixes in order of likelihood
    let suffixes = [".1", ".0", "-1"];

    for suffix in &suffixes {
        let rotated = PathBuf::from(format!("{path_str}{suffix}"));
        if let Ok(file) = File::open(&rotated)
            && let Ok(meta) = file.metadata()
        {
            #[cfg(unix)]
            {
                let rotated_id = FileId::from_metadata(&meta);
                if rotated_id == tailed.id {
                    return Some(file);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = meta;
                return Some(file);
            }
        }
    }

    // Also check the parent directory for any file with matching inode
    #[cfg(unix)]
    if let Some(parent) = tailed.path.parent()
        && let Ok(entries) = std::fs::read_dir(parent)
    {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path == tailed.path {
                continue;
            }
            if let Ok(meta) = entry.metadata()
                && FileId::from_metadata(&meta) == tailed.id
                && let Ok(file) = File::open(&entry_path)
            {
                return Some(file);
            }
        }
    }

    None
}

/// Drain remaining lines from a retained fd (old file after rotation).
///
/// Honors backpressure and the per-poll line cap like the live read path:
/// when the SDK channel is full or the cap is hit, the fd is put back and
/// draining resumes from `pos` on the next poll. On EOF the fd is dropped
/// and the caller removes the entry.
pub(crate) fn drain_retained(tailed: &mut TailedFile, sink: &Sink, parse_syslog: bool) -> u64 {
    let Some(file) = tailed.retained_fd.take() else {
        return 0;
    };

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(tailed.pos)).is_err() {
        return 0;
    }

    let mut bytes_read = 0u64;
    let mut lines_emitted = 0usize;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break, // EOF — old file fully drained
            Ok(n) => {
                if buf.last() == Some(&b'\n') {
                    if !try_emit_line(
                        &buf[..buf.len() - 1],
                        &mut tailed.partial,
                        sink,
                        parse_syslog,
                    ) {
                        // Channel full — keep the fd, resume next poll.
                        tailed.retained_fd = Some(reader.into_inner());
                        return bytes_read;
                    }
                    tailed.pos += n as u64;
                    bytes_read += n as u64;
                    lines_emitted += 1;
                    if lines_emitted >= MAX_LINES_PER_POLL {
                        tailed.retained_fd = Some(reader.into_inner());
                        return bytes_read;
                    }
                } else {
                    tailed.pos += n as u64;
                    bytes_read += n as u64;
                    push_partial(&mut tailed.partial, &buf);
                }
            }
            Err(_) => break,
        }
    }

    // Flush any remaining partial from the old file
    flush_partial(tailed, sink);

    bytes_read
}

/// Append bytes to the partial-line buffer, capped at [`MAX_PARTIAL_BYTES`]
/// on a UTF-8 character boundary (a plain `truncate` can panic mid-char).
fn push_partial(partial: &mut String, bytes: &[u8]) {
    if partial.len() >= MAX_PARTIAL_BYTES {
        return;
    }
    partial.push_str(&String::from_utf8_lossy(bytes));
    if partial.len() > MAX_PARTIAL_BYTES {
        partial.truncate(partial.floor_char_boundary(MAX_PARTIAL_BYTES));
    }
}

/// Read complete lines from the file, buffering partials.
///
/// Backpressure: when `try_log()` signals a full channel, we stop reading
/// **without advancing the file offset**. The unread lines stay in the file
/// and will be picked up on the next poll. This makes the filesystem the
/// natural overflow buffer — zero blocking, zero data loss.
pub(crate) fn read_lines(
    file: File,
    tailed: &mut TailedFile,
    sink: &Sink,
    parse_syslog: bool,
) -> u64 {
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(tailed.pos)).is_err() {
        return 0;
    }

    let mut bytes_read = 0u64;
    let mut lines_emitted = 0usize;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if buf.last() == Some(&b'\n') {
                    if !try_emit_line(
                        &buf[..buf.len() - 1],
                        &mut tailed.partial,
                        sink,
                        parse_syslog,
                    ) {
                        // Channel full — don't advance offset, retry next poll.
                        break;
                    }
                    tailed.pos += n as u64;
                    bytes_read += n as u64;
                    lines_emitted += 1;
                    if lines_emitted >= MAX_LINES_PER_POLL {
                        break;
                    }
                } else {
                    // Partial line — buffer it, don't emit yet.
                    // Always advance offset for partials (they're buffered in memory).
                    tailed.pos += n as u64;
                    bytes_read += n as u64;
                    // Capped at MAX_PARTIAL_BYTES to prevent OOM from binary/malformed files.
                    push_partial(&mut tailed.partial, &buf);
                }
            }
            Err(_) => break,
        }
    }

    bytes_read
}

/// Try to emit a complete line, returning `false` if the SDK channel is full.
///
/// On failure, `partial` is left unchanged so the caller can re-read the same
/// file bytes on the next poll and re-assemble identically.
pub(crate) fn try_emit_line(
    line_bytes: &[u8],
    partial: &mut String,
    sink: &Sink,
    parse_syslog: bool,
) -> bool {
    let line_lossy = String::from_utf8_lossy(line_bytes);

    let complete: std::borrow::Cow<'_, str> = if partial.is_empty() {
        std::borrow::Cow::Borrowed(line_lossy.as_ref())
    } else {
        let mut assembled = String::with_capacity(partial.len() + line_lossy.len());
        assembled.push_str(partial);
        assembled.push_str(&line_lossy);
        std::borrow::Cow::Owned(assembled)
    };

    let trimmed = complete.trim_end();
    if trimmed.is_empty() {
        partial.clear();
        return true;
    }

    let ok = if parse_syslog {
        if let Some(parsed) = super::syslog::parse(trimmed) {
            sink.try_log_with_service(
                tell::LogLevel::Info,
                parsed.body,
                None,
                Some(parsed.program),
                None::<()>,
            )
        } else {
            sink.try_log(tell::LogLevel::Info, trimmed, None, None::<()>)
        }
    } else {
        sink.try_log(tell::LogLevel::Info, trimmed, None, None::<()>)
    };

    if ok {
        partial.clear();
        true
    } else {
        // partial unchanged — offset won't advance — next poll re-reads
        // the same file bytes and re-assembles identically.
        false
    }
}

/// Emit any buffered partial line (called on rotation/shutdown).
pub(crate) fn flush_partial(tailed: &mut TailedFile, sink: &Sink) {
    if tailed.partial.is_empty() {
        return;
    }

    let line = std::mem::take(&mut tailed.partial);
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        sink.log(tell::LogLevel::Info, trimmed, None, None::<()>);
    }
}

// --- Glob resolution ---

pub(crate) fn resolve_globs(patterns: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for pattern in patterns {
        if pattern.contains(['*', '?', '[']) {
            if let Ok(entries) = glob::glob(pattern) {
                for entry in entries.flatten() {
                    if entry.is_file() {
                        result.push(entry);
                    }
                }
            }
        } else {
            let path = PathBuf::from(pattern);
            if path.is_file() {
                result.push(path);
            }
        }
    }
    result
}

// --- Offset persistence ---
//
// Line format: PATH\tPOS\tDEV\tINO\n
// Atomic write: tmp → fsync → rename.

pub(crate) struct SavedOffset {
    pub(crate) pos: u64,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

pub(crate) fn save_offsets(files: &HashMap<FileId, TailedFile>) {
    let state_file = state_file_path();
    if let Some(parent) = state_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let tmp = state_file.with_extension("tmp");
    let Ok(file) = File::create(&tmp) else {
        return;
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }

    let mut w = std::io::BufWriter::new(file);
    for tailed in files.values() {
        let _ = writeln!(
            w,
            "{}\t{}\t{}\t{}",
            tailed.path.display(),
            tailed.pos,
            tailed.id.dev,
            tailed.id.ino,
        );
    }

    // fsync before rename for crash safety
    if let Ok(inner) = w.into_inner() {
        let _ = inner.sync_all();
    }

    let _ = std::fs::rename(&tmp, &state_file);
}

pub(crate) fn load_offsets() -> HashMap<String, SavedOffset> {
    let Ok(contents) = std::fs::read_to_string(state_file_path()) else {
        return HashMap::new();
    };

    let mut map = HashMap::new();
    for line in contents.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() == 4
            && let (Ok(pos), Ok(dev), Ok(ino)) =
                (parts[1].parse(), parts[2].parse(), parts[3].parse())
        {
            map.insert(parts[0].to_string(), SavedOffset { pos, dev, ino });
        }
    }
    map
}
