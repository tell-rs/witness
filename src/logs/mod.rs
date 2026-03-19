//! Log file tailer — watches configured paths and ships lines via the Tell SDK.
//!
//! Uses polling with adaptive backoff (not inotify/kqueue) for reliability
//! under kernel queue pressure. 250ms base latency is irrelevant for log shipping.
//!
//! Features:
//! - Offset persistence across restarts (atomic write + fsync)
//! - Periodic glob re-scan to discover new log files
//! - Log rotation detection (inode tracking + truncation detection)

pub mod watcher;

pub use watcher::tail_files;

#[cfg(test)]
mod watcher_test;
