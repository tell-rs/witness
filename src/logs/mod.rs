//! Log file tailer — watches configured paths and ships lines via the Tell SDK.
//!
//! Uses polling with adaptive backoff (not inotify/kqueue) for reliability
//! under kernel queue pressure. 250ms base latency is irrelevant for log shipping.
//!
//! Features:
//! - Offset persistence across restarts (atomic write + fsync)
//! - Periodic glob re-scan to discover new log files
//! - Log rotation detection (inode tracking + truncation detection)

pub mod eventlog_filter;
pub mod eventlog_parse;
pub mod journal;
pub mod multiline;
pub mod source;
pub mod structured;
pub mod syslog;
pub mod unified;
pub mod unified_parse;
pub mod watcher;

/// Windows Event Log source (`Evt*` pull pump). Windows-only plumbing; the
/// pure parser is `eventlog_parse` (all platforms).
#[cfg(target_os = "windows")]
pub mod eventlog;
#[cfg(target_os = "windows")]
pub use eventlog::tail_eventlog;

pub use journal::tail_journal;
pub use multiline::MultilineOpts;
pub use structured::FileParseOpts;
pub use unified::tail_unified_log;
pub use watcher::tail_files;

#[cfg(test)]
mod eventlog_filter_test;
#[cfg(test)]
mod eventlog_parse_test;
#[cfg(test)]
mod journal_test;
#[cfg(test)]
mod multiline_test;
#[cfg(test)]
mod source_test;
#[cfg(test)]
mod structured_test;
#[cfg(test)]
mod syslog_test;
#[cfg(test)]
mod unified_test;
#[cfg(test)]
mod watcher_test;
