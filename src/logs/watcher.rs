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
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tracing::{info, warn};

use super::multiline::{self, MultilineOpts};
use super::structured::{FileParseOpts, classify_line};
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
pub(crate) const MAX_LINES_PER_POLL: usize = 32_000;
/// When draining a large backlog (pos far behind file end), use faster 50ms
/// polls instead of the default 250ms base.
const POLL_CATCHUP_MS: u64 = 50;
/// Offset state file name (lives under the platform state directory).
const STATE_FILE_NAME: &str = "offsets";

fn state_file_path() -> std::path::PathBuf {
    std::path::Path::new(crate::config::state_dir()).join(STATE_FILE_NAME)
}

// --- File identity ---

/// Unique file identity: device + inode (Unix), volume serial + folded 128-bit
/// file id via `FILE_ID_INFO` (Windows), or path hash (other non-Unix). The
/// `{dev, ino}` shape and the `PATH\tPOS\tDEV\tINO` offset format are identical
/// across platforms — Windows folds `FILE_ID_INFO` into it (spec 006 R4).
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

    /// Path-hash identity — cannot detect rotation (a renamed file keeps its
    /// path-derived id, a replacement at the same path looks identical). Used
    /// on non-Unix-non-Windows platforms and as the Windows degraded fallback
    /// when the `GetFileInformationByHandleEx` syscall fails.
    #[cfg(not(unix))]
    pub(crate) fn from_path(path: &Path) -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        Self {
            dev: 0,
            ino: h.finish(),
        }
    }

    /// Real Windows file identity via `GetFileInformationByHandleEx(FileIdInfo)`
    /// on an open handle, folded into `{dev, ino}` (spec 006 R4). Falls back to
    /// the path hash (logged once) on syscall failure so tailing keeps working,
    /// with degraded rotation detection, rather than dropping the file.
    #[cfg(target_os = "windows")]
    pub(crate) fn from_file(file: &File, path: &Path) -> Self {
        match windows_file_id(file) {
            Some((volume_serial, id)) => fold_file_id(volume_serial, id),
            None => {
                warn_file_id_fallback_once(path);
                Self::from_path(path)
            }
        }
    }
}

/// Fold a `FILE_ID_INFO` (`VolumeSerialNumber` + 128-bit `FileId`) into the
/// `{dev, ino}` pair the offset store uses (spec 006 R4). Pure; unit-tested on
/// any platform.
///
/// `dev = volume_serial`. `ino =` the low 64 bits of the file id when its high
/// 64 bits are zero (the NTFS common case — a 64-bit MFT reference
/// zero-extended, lossless); otherwise a stable 64-bit hash of all 16 id bytes
/// (ReFS, where the full 128 bits are significant). A hash collision is
/// astronomically unlikely and documented.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[must_use]
pub(crate) fn fold_file_id(volume_serial: u64, id: [u8; 16]) -> FileId {
    let low = u64::from_le_bytes([id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7]]);
    let high = u64::from_le_bytes([id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]]);
    let ino = if high == 0 {
        low
    } else {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        id.hash(&mut h);
        h.finish()
    };
    FileId {
        dev: volume_serial,
        ino,
    }
}

/// Byte-order-mark classification of a file's leading bytes (spec 006 R6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bom {
    /// UTF-16 little-endian (`FF FE`).
    Utf16Le,
    /// UTF-16 big-endian (`FE FF`).
    Utf16Be,
    /// UTF-8 (`EF BB BF`).
    Utf8,
    /// No recognized BOM.
    None,
}

impl Bom {
    /// Whether this BOM marks a UTF-16 file that the byte-oriented tailer must
    /// skip (v1 supports UTF-8/lossy only — spec 006 R6).
    #[must_use]
    pub(crate) fn is_utf16(self) -> bool {
        matches!(self, Bom::Utf16Le | Bom::Utf16Be)
    }
}

/// Classify a file's leading bytes. Pure; unit-tested with byte-slice fixtures.
/// The UTF-8 BOM (`EF BB BF`) must never be mistaken for UTF-16.
#[must_use]
pub(crate) fn detect_bom(bytes: &[u8]) -> Bom {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        Bom::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Bom::Utf16Be
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Bom::Utf8
    } else {
        Bom::None
    }
}

/// Whether the file at `path` begins with a UTF-16 BOM (and must be skipped).
/// Reads only the leading bytes; a read error is treated as "not UTF-16" (the
/// normal open/read path handles a truly unreadable file).
fn file_is_utf16(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 3];
    let n = f.read(&mut buf).unwrap_or(0);
    detect_bom(&buf[..n]).is_utf16()
}

/// Query `FILE_ID_INFO` for an open handle: `(VolumeSerialNumber, FileId bytes)`.
#[cfg(target_os = "windows")]
fn windows_file_id(file: &File) -> Option<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let handle = HANDLE(file.as_raw_handle());
    let mut info = FILE_ID_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok.is_ok() {
        Some((info.VolumeSerialNumber, info.FileId.Identifier))
    } else {
        None
    }
}

/// Warn (once per process) that the Windows file-id syscall failed and the
/// tailer fell back to the path hash for a file.
#[cfg(target_os = "windows")]
fn warn_file_id_fallback_once(path: &Path) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        warn!(
            path = %path.display(),
            "tail: GetFileInformationByHandleEx failed — falling back to path-hash \
             identity (degraded rotation detection)"
        );
    });
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
    /// UTF-16 file: registered as a marker so the rescan neither re-registers
    /// nor re-warns, but never read (v1 ships UTF-8/lossy only — spec 006 R6).
    pub(crate) skip_utf16: bool,
    /// In-flight multiline record aggregation state (spec 008). `None` in the
    /// single-line default; lazily created by the multiline path on first read.
    /// Never persisted — a crash re-derives the in-flight record from `pos`.
    pub(crate) agg: Option<multiline::Aggregator>,
}

// --- Main loop ---

/// Tail log files. Blocks until cancellation.
///
/// `ml` carries the compiled multiline aggregation settings (spec 008) when
/// `multiline_start_pattern` is configured; `None` runs the single-line-per-entry
/// path unchanged. The `Arc` is compiled once at startup and shared read-only.
pub async fn tail_files(
    patterns: &[String],
    sink: Sink,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    opts: FileParseOpts,
    ml: Option<Arc<MultilineOpts>>,
) {
    let ml = ml.as_deref();
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
    // Remaining time to the nearest pending multiline record's inactivity flush
    // (spec 008 R4). Caps the poll sleep so an idle record still ships on time.
    let mut pending_flush: Option<Duration> = None;
    let mut last_save = Instant::now();
    let mut glob_interval = tokio::time::interval(std::time::Duration::from_secs(GLOB_RESCAN_SECS));
    let mut save_interval = tokio::time::interval(std::time::Duration::from_secs(OFFSET_SAVE_SECS));
    glob_interval.tick().await;
    save_interval.tick().await;

    loop {
        let sleep = poll_delay(poll_interval_ms, pending_flush);
        tokio::select! {
            // Poll all files
            _ = tokio::time::sleep(sleep) => {
                let total_bytes = poll_all(&mut files, &mut path_index, &sink, opts, ml);
                pending_flush = flush_expired_records(&mut files, &sink, opts, ml);

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
                poll_all(&mut files, &mut path_index, &sink, opts, ml);
                for f in files.values_mut() {
                    match ml {
                        // Ship any in-flight multiline record before exit (R7).
                        Some(ml) => multiline::flush_final(f, &sink, opts, ml),
                        None => flush_partial(f, &sink),
                    }
                }
                save_offsets(&files);
                return;
            }
        }
    }
}

/// The next poll sleep: the adaptive interval, capped by the nearest pending
/// multiline inactivity flush (spec 008 R4) so a buffered record is not held
/// past its timeout while the loop backs off. Floored at [`POLL_CATCHUP_MS`] to
/// avoid a busy-loop when a record is already due but could not ship (R3).
fn poll_delay(poll_interval_ms: u64, pending_flush: Option<Duration>) -> Duration {
    let base = Duration::from_millis(poll_interval_ms);
    match pending_flush {
        Some(remaining) => base
            .min(remaining)
            .max(Duration::from_millis(POLL_CATCHUP_MS)),
        None => base,
    }
}

/// Flush every file whose in-flight multiline record has passed its inactivity
/// timeout, returning the smallest remaining time to any still-pending record's
/// timeout (for the poll-delay cap). `None` when aggregation is off or no record
/// is pending.
fn flush_expired_records(
    files: &mut HashMap<FileId, TailedFile>,
    sink: &Sink,
    opts: FileParseOpts,
    ml: Option<&MultilineOpts>,
) -> Option<Duration> {
    let ml = ml?;
    let mut nearest: Option<Duration> = None;
    for f in files.values_mut() {
        if let Some(remaining) = multiline::flush_if_expired(f, sink, opts, ml) {
            nearest = Some(nearest.map_or(remaining, |n| n.min(remaining)));
        }
    }
    nearest
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

    let id = file_id_of(path, &metadata);

    // Already tracking this inode
    if files.contains_key(&id) {
        return;
    }

    // UTF-16 detection (spec 006 R6): the byte-oriented tailer would emit
    // mojibake. Register a skip marker (deduped by id) so we neither ship
    // garbage nor re-warn on every rescan; UTF-8 (incl. a UTF-8 BOM) is fine.
    let skip_utf16 = file_is_utf16(path);
    if skip_utf16 {
        warn!(
            path = %path.display(),
            "tail: UTF-16 file skipped (only UTF-8/lossy supported in v1)"
        );
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
            skip_utf16,
            agg: None,
        },
    );
}

/// Compute the file identity for a path, given its metadata. On Windows this
/// opens the file to query `FILE_ID_INFO`; on Unix it uses the metadata; other
/// platforms fall back to the path hash.
fn file_id_of(path: &Path, metadata: &std::fs::Metadata) -> FileId {
    #[cfg(unix)]
    {
        let _ = path;
        FileId::from_metadata(metadata)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = metadata;
        match File::open(path) {
            Ok(f) => FileId::from_file(&f, path),
            Err(_) => FileId::from_path(path),
        }
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = metadata;
        FileId::from_path(path)
    }
}

// --- Polling ---

/// Poll all tracked files. Returns total bytes read across all files.
fn poll_all(
    files: &mut HashMap<FileId, TailedFile>,
    path_index: &mut HashMap<PathBuf, FileId>,
    sink: &Sink,
    opts: FileParseOpts,
    ml: Option<&MultilineOpts>,
) -> u64 {
    let mut total_bytes = 0u64;

    let ids: Vec<FileId> = files.keys().copied().collect();

    for id in ids {
        let Some(tailed) = files.get_mut(&id) else {
            continue;
        };

        // UTF-16 marker: never read (would ship mojibake) — spec 006 R6.
        if tailed.skip_utf16 {
            continue;
        }

        // Entries with a retained fd are rotated-out files: their only job is
        // draining to EOF. The path is owned by the new-inode entry registered
        // at rotation time, so once drained, remove the entry.
        if tailed.retained_fd.is_some() {
            // Multiline mode continues the in-flight record into the drain and
            // ships it at EOF (spec 008 R7); single-line mode drains lines.
            total_bytes += match ml {
                Some(ml) => multiline::drain(tailed, sink, opts, ml),
                None => drain_retained(tailed, sink, opts),
            };
            if tailed.retained_fd.is_none()
                && let Some(f) = files.remove(&id)
                && path_index.get(&f.path) == Some(&f.id)
            {
                path_index.remove(&f.path);
            }
            continue;
        }

        match check_and_read(tailed, sink, opts, ml) {
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

fn check_and_read(
    tailed: &mut TailedFile,
    sink: &Sink,
    opts: FileParseOpts,
    ml: Option<&MultilineOpts>,
) -> ReadResult {
    let Ok(file) = File::open(&tailed.path) else {
        return ReadResult::OpenFailed;
    };

    let Ok(metadata) = file.metadata() else {
        return ReadResult::OpenFailed;
    };

    #[cfg(unix)]
    let current_id = FileId::from_metadata(&metadata);
    #[cfg(target_os = "windows")]
    let current_id = FileId::from_file(&file, &tailed.path);
    #[cfg(not(any(unix, target_os = "windows")))]
    let current_id = FileId::from_path(&tailed.path);

    // Rotation detected — the path now points to a different inode, so the
    // fd we just opened is the NEW file. We open by path each poll, so we
    // never held the old file's fd; find the rotated original by inode
    // (common suffixes like `.1`, then a directory scan) and hand its fd to
    // the caller for draining. The caller registers the new file separately.
    if current_id != tailed.id {
        // Multiline: leave the in-flight record and sub-line partial intact —
        // the retained-fd drain continues them from `read_ahead` and ships the
        // record at the old file's EOF (spec 008 R7). Single-line: flush the
        // sub-line partial now, as before.
        if ml.is_none() {
            flush_partial(tailed, sink);
        }

        if let Some(old_fd) = find_rotated_file(tailed) {
            return ReadResult::Rotated { old_fd };
        }

        return ReadResult::RotatedLost;
    }

    let current_len = metadata.len();

    // Detect truncation (copytruncate rotation)
    if current_len < tailed.pos {
        tailed.pos = 0;
        match ml {
            // The buffered bytes are gone from the file — discard the in-flight
            // record and reset the read cursor to 0 (spec 008 R6).
            Some(_) => multiline::reset(tailed),
            None => flush_partial(tailed, sink),
        }
    }

    if current_len <= tailed.pos {
        return ReadResult::Idle;
    }

    let bytes_read = match ml {
        Some(ml) => multiline::read_live(file, tailed, sink, opts, ml),
        None => read_lines(file, tailed, sink, opts),
    };
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
pub(crate) fn drain_retained(tailed: &mut TailedFile, sink: &Sink, opts: FileParseOpts) -> u64 {
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
                    if !try_emit_line(&buf[..buf.len() - 1], &mut tailed.partial, sink, opts) {
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
pub(crate) fn push_partial(partial: &mut String, bytes: &[u8]) {
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
    opts: FileParseOpts,
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
                    if !try_emit_line(&buf[..buf.len() - 1], &mut tailed.partial, sink, opts) {
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
    opts: FileParseOpts,
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

    // Strip a leading UTF-8 BOM (U+FEFF) — it survives `from_utf8_lossy` on the
    // first line of a UTF-8-with-BOM file and must not leak into the body
    // (spec 006 R6). CRLF is handled by `trim_end` (strips the trailing `\r`).
    let trimmed = complete.trim_end();
    let trimmed = trimmed.strip_prefix('\u{feff}').unwrap_or(trimmed);
    if trimmed.is_empty() {
        partial.clear();
        return true;
    }

    // Syslog envelope → structured extraction → severity, all in the shared
    // `structured` module (the journald quality bar). The syslog envelope is
    // parsed FIRST, then structure is extracted from the inner body.
    let classified = classify_line(trimmed, opts);
    let ok = sink.try_log_with_service(
        classified.level,
        &classified.body,
        None,
        classified.service,
        classified.payload,
    );

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
