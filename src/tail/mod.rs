//! Log file tailer — watches configured paths and ships lines via the Tell SDK.
//!
//! Uses OS-native file watching (inotify on Linux, FSEvents on macOS) via the
//! `notify` crate. Falls back gracefully if watching fails.
//!
//! Features:
//! - Offset persistence across restarts (JSON state file)
//! - Periodic glob re-scan to discover new log files
//! - Log rotation detection (truncation + inode change on Unix)

pub mod watcher;

#[cfg(test)]
mod watcher_test;
