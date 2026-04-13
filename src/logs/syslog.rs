//! Lightweight syslog line parser.
//!
//! Extracts program name and message body from RFC 3164 (BSD) syslog lines
//! and ISO 8601-prefixed variants. Zero-regex, byte-level scanning.
//!
//! Supported formats:
//!   RFC 3164:  `"Apr 12 23:50:00 hostname sshd[1234]: Connection reset"`
//!   ISO 8601:  `"2026-04-12T23:50:00.860800+00:00 hostname sshd[1234]: Connection reset"`
//!
//! Returns `(program, body)` — e.g. `("sshd", "Connection reset")`.
//! Returns `None` if no `: ` separator is found (not a parseable syslog line).

/// Parsed fields from a syslog line.
pub struct SyslogParsed<'a> {
    /// Program/service name (e.g. "sshd", "kernel", "CRON").
    pub program: &'a str,
    /// Message body after the syslog envelope.
    pub body: &'a str,
}

/// Parse a syslog line, extracting program name and message body.
///
/// Strategy: find the first `: ` (colon-space) — this is the standard syslog
/// separator between the program tag and the message. Then scan backwards
/// from the colon to extract the program name (stripping PID brackets).
///
/// Complexity: single pass, O(n), no allocation, no regex.
pub fn parse(line: &str) -> Option<SyslogParsed<'_>> {
    // Find ": " separator
    let sep = line.find(": ")?;
    let body = &line[sep + 2..];

    // Work backwards from the separator to find the program name.
    // The tag is the last whitespace-delimited token before ": ",
    // optionally with a PID in brackets: "sshd[1234]"
    let before_sep = &line[..sep];
    let tag_start = before_sep.rfind(' ')? + 1;
    let tag = &before_sep[tag_start..];

    // Strip [PID] suffix if present: "sshd[1234]" → "sshd"
    let program = match tag.find('[') {
        Some(bracket) => &tag[..bracket],
        None => tag,
    };

    if !is_valid_program(program) {
        return None;
    }

    Some(SyslogParsed { program, body })
}

/// Program names start with an ASCII letter, then `[a-zA-Z0-9_.-]`.
/// Rejects numeric fragments (`1234#0`), PIDs, and other garbage that
/// slip through when non-syslog lines happen to contain `": "`.
fn is_valid_program(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
