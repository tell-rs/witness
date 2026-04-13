//! Log file tailer — watches configured paths and ships lines via the Tell SDK.
//!
//! Uses polling with adaptive backoff (not inotify/kqueue) for reliability
//! under kernel queue pressure. 250ms base latency is irrelevant for log shipping.
//!
//! Features:
//! - Offset persistence across restarts (atomic write + fsync)
//! - Periodic glob re-scan to discover new log files
//! - Log rotation detection (inode tracking + truncation detection)

pub mod journal;
pub mod syslog;
pub mod watcher;

pub use journal::tail_journal;
pub use watcher::tail_files;

#[cfg(test)]
mod journal_test;
#[cfg(test)]
mod syslog_test;
#[cfg(test)]
mod watcher_test;
