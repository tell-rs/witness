//! Structured extraction and severity detection for log lines.
//!
//! Shared by the journald source (`journal.rs`) and the file tailer
//! (`watcher.rs`). Two concerns live here:
//!
//! - **Structured extraction** ([`split_message`]): pull the event phrase and a
//!   flat key/value payload out of a message that is itself JSON or logfmt. This
//!   is the journald `MESSAGE` parser — kept byte-identical when the file tailer
//!   reuses it, so both sources ship the same clean body + structured fields.
//! - **Severity classification** ([`classify_line`]): the file tailer has no
//!   out-of-band priority (journald has `PRIORITY`, the Event Log has `Level`),
//!   so it recovers a level from the line itself — first from a structured
//!   `level`/`severity`/`lvl` field, else from a cheap token scan.
//!
//! Everything here is pure and fixture-tested. No allocations on the common
//! borrowed path; the body is a [`Cow`] borrowed from the input when possible.

use std::borrow::Cow;

use serde_json::Value;

/// Which parsing steps the file tailer applies to each line. Bundled into one
/// `Copy` struct rather than three positional `bool`s so the plumbing through
/// `tail_files` → `poll_all` → `read_lines` → `try_emit_line` stays readable.
#[derive(Debug, Clone, Copy)]
pub struct FileParseOpts {
    /// Strip an RFC 3164 / ISO 8601 syslog envelope, extracting the program
    /// name as the service and the remainder as the body (config `parse_syslog`).
    pub syslog: bool,
    /// Extract JSON / logfmt structure out of the (envelope-stripped) body —
    /// event phrase as the body, remaining keys as a structured payload
    /// (config `parse_structured`).
    pub structured: bool,
    /// Recover a log level from the line: a structured `level`/`severity`/`lvl`
    /// field first, else a heuristic token scan (config `detect_levels`).
    pub levels: bool,
}

/// The classified result of a single log line, ready to hand to the sink.
pub(crate) struct Classified<'a> {
    /// Severity — defaults to `Info` when nothing indicates otherwise.
    pub level: tell::LogLevel,
    /// Clean message body (event phrase), borrowed when possible.
    pub body: Cow<'a, str>,
    /// Service/program name, when a syslog envelope was parsed.
    pub service: Option<&'a str>,
    /// Structured fields extracted from the body, if any.
    pub payload: Option<Value>,
}

/// Classify a trimmed file log line into `(level, body, service, payload)`.
///
/// Pipeline (order matters — this is the file-tailer quality bar, matched to
/// the journald path):
/// 1. **Syslog envelope** first (when `opts.syslog`): `program` + inner body.
/// 2. **Structured extraction** on the body (when `opts.structured`): JSON /
///    logfmt → event phrase + fields, consuming a `level`/`severity`/`lvl` field
///    into the severity so it is not duplicated in the payload.
/// 3. **Heuristic severity** (when `opts.levels` and no structured level was
///    found): a token scan of the body.
pub(crate) fn classify_line(line: &str, opts: FileParseOpts) -> Classified<'_> {
    // 1. Syslog envelope first, so structured extraction runs on the inner body.
    let (body_src, service): (&str, Option<&str>) = if opts.syslog {
        match super::syslog::parse(line) {
            Some(p) => (p.body, Some(p.program)),
            None => (line, None),
        }
    } else {
        (line, None)
    };

    // 2. Structured extraction + structured-level consumption.
    let mut level = tell::LogLevel::Info;
    let mut level_found = false;
    let (body, payload): (Cow<'_, str>, Option<Value>) = if opts.structured {
        let (b, mut fields) = split_message(body_src);
        if opts.levels
            && let Some(Value::Object(obj)) = fields.as_mut()
            && let Some(l) = take_level(obj)
        {
            level = l;
            level_found = true;
        }
        // Removing the level key may have emptied the object.
        let payload = match fields {
            Some(Value::Object(o)) if o.is_empty() => None,
            other => other,
        };
        (b, payload)
    } else {
        (Cow::Borrowed(body_src), None)
    };

    // 3. Heuristic severity when structure gave us none.
    if opts.levels
        && !level_found
        && let Some(l) = scan_line_level(&body)
    {
        level = l;
    }

    Classified {
        level,
        body,
        service,
        payload,
    }
}

// ─── Structured extraction (moved verbatim from journal.rs) ──────────────────

/// Extract the event phrase (body) and structured fields from a message.
///
/// Detects JSON if the text starts with `{`; otherwise tries logfmt. On
/// any parse failure, returns the full message as body with no fields —
/// witness stays a forwarder, never drops data.
///
/// Body is borrowed when possible (logfmt slice) and owned when not
/// (JSON value extracted from a parsed object).
pub fn split_message(message: &str) -> (std::borrow::Cow<'_, str>, Option<serde_json::Value>) {
    use std::borrow::Cow;

    let trimmed = message.trim_start();
    if trimmed.starts_with('{') {
        if let Ok(serde_json::Value::Object(mut obj)) = serde_json::from_str(trimmed) {
            let body = obj
                .remove("msg")
                .or_else(|| obj.remove("message"))
                .map(|v| match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
                .map_or(Cow::Borrowed(message), Cow::Owned);
            let fields = if obj.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(obj))
            };
            return (body, fields);
        }
    }

    if let Some(split_at) = logfmt_field_start(message) {
        let body = &message[..split_at];
        let rest = &message[split_at + 1..];
        let fields = parse_logfmt_fields(rest);
        let fields_v = if fields.is_empty() {
            None
        } else {
            let obj: serde_json::Map<String, serde_json::Value> = fields
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            Some(serde_json::Value::Object(obj))
        };
        return (Cow::Borrowed(body), fields_v);
    }

    (Cow::Borrowed(message), None)
}

/// Scan for the byte offset of the space preceding the first `key=value`
/// token. Returns `None` if no logfmt-shaped tail is present.
fn logfmt_field_start(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            // Look ahead for `<ident>=` at bytes[i+1..]
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && is_logfmt_key_byte(bytes[j]) {
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b'=' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn is_logfmt_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

/// Parse a logfmt tail (the part after the event phrase) into key/value pairs.
fn parse_logfmt_fields(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read key.
        let key_start = i;
        while i < bytes.len() && is_logfmt_key_byte(bytes[i]) {
            i += 1;
        }
        if i == key_start || i >= bytes.len() || bytes[i] != b'=' {
            // Malformed — skip to next space.
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            continue;
        }
        let key = s[key_start..i].to_string();
        i += 1; // past '='
        // Read value: quoted or bare. Copy via `&str` slices between escape
        // points — never byte-by-byte casts, which would mangle multi-byte
        // UTF-8 characters. Slice bounds always land on `"` or `\`, which in
        // valid UTF-8 can only be standalone ASCII bytes.
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let mut v = String::new();
            let mut run_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    match bytes[i + 1] {
                        b'"' => {
                            v.push_str(&s[run_start..i]);
                            v.push('"');
                            i += 2;
                            run_start = i;
                        }
                        b'\\' => {
                            v.push_str(&s[run_start..i]);
                            v.push('\\');
                            i += 2;
                            run_start = i;
                        }
                        b'n' => {
                            v.push_str(&s[run_start..i]);
                            v.push('\n');
                            i += 2;
                            run_start = i;
                        }
                        // Unknown escape — keep the backslash and whatever
                        // follows literally (it may be multi-byte).
                        _ => i += 1,
                    }
                } else {
                    i += 1;
                }
            }
            v.push_str(&s[run_start..i.min(bytes.len())]);
            i += 1; // past closing '"'
            v
        } else {
            let v_start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            s[v_start..i].to_string()
        };
        out.push((key, value));
    }
    out
}

// ─── Severity classification ─────────────────────────────────────────────────

/// Structured payload keys that carry a log level. Matched case-insensitively.
const LEVEL_KEYS: [&str; 3] = ["level", "severity", "lvl"];

/// Remove and map a `level`/`severity`/`lvl` field from a structured payload.
///
/// Returns the mapped [`tell::LogLevel`] and drops the key so it is not
/// duplicated in the forwarded payload. If a candidate key is present but its
/// value is not a recognizable level, the key is left in place (we only consume
/// what we understand).
fn take_level(obj: &mut serde_json::Map<String, Value>) -> Option<tell::LogLevel> {
    let key = obj
        .iter()
        .find(|(k, _)| LEVEL_KEYS.iter().any(|c| k.eq_ignore_ascii_case(c)))
        .map(|(k, _)| k.clone())?;
    let level = level_from_value(obj.get(&key)?)?;
    obj.remove(&key);
    Some(level)
}

/// Map a JSON value (string level name, numeric syslog priority, or numeric
/// string) to a [`tell::LogLevel`].
fn level_from_value(v: &Value) -> Option<tell::LogLevel> {
    match v {
        Value::String(s) => level_from_str(s),
        Value::Number(n) => n.as_u64().and_then(numeric_level),
        _ => None,
    }
}

/// Map a level name (case-insensitive) or a numeric syslog string (`"0"`-`"7"`)
/// to a [`tell::LogLevel`].
fn level_from_str(s: &str) -> Option<tell::LogLevel> {
    use tell::LogLevel::{Critical, Debug, Error, Info, Notice, Warning};
    let t = s.trim();
    if let Ok(n) = t.parse::<u64>() {
        return numeric_level(n);
    }
    Some(match t.to_ascii_lowercase().as_str() {
        "trace" | "debug" => Debug,
        "info" => Info,
        "notice" => Notice,
        "warn" | "warning" => Warning,
        "err" | "error" => Error,
        "crit" | "critical" | "fatal" | "panic" => Critical,
        _ => return None,
    })
}

/// Map a numeric syslog priority (0-7) to a [`tell::LogLevel`].
fn numeric_level(n: u64) -> Option<tell::LogLevel> {
    use tell::LogLevel::{Alert, Critical, Debug, Emergency, Error, Info, Notice, Warning};
    Some(match n {
        0 => Emergency,
        1 => Alert,
        2 => Critical,
        3 => Error,
        4 => Warning,
        5 => Notice,
        6 => Info,
        7 => Debug,
        _ => return None,
    })
}

/// Number of leading bytes scanned for a level token. Real loggers put the
/// level in the prefix (after an optional timestamp), so a short window catches
/// nginx / log4j / env_logger lines without scanning whole messages.
const LEVEL_SCAN_WINDOW: usize = 96;

/// Heuristically recover a level from the start of a log line.
///
/// Scans the first [`LEVEL_SCAN_WINDOW`] bytes for a delimited level token —
/// matching the exact forms real loggers emit (` ERROR `, `[error]`,
/// `level=error`). A "token" is a maximal run of `[A-Za-z0-9_]` bounded by
/// non-word bytes (or the window edge), so mid-word occurrences never match:
/// `Terror` and `errors` are both rejected. Matching is case-sensitive to the
/// uppercase (`ERROR`) and lowercase (`error`) forms only — title-case prose
/// like `Error occurred` does not trip it. The first matching token wins.
pub(crate) fn scan_line_level(line: &str) -> Option<tell::LogLevel> {
    let window = &line.as_bytes()[..line.len().min(LEVEL_SCAN_WINDOW)];
    let mut i = 0;
    while i < window.len() {
        // Advance to the start of a delimited word.
        if !is_word_byte(window[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < window.len() && is_word_byte(window[i]) {
            i += 1;
        }
        // Left boundary guaranteed by the outer loop; right boundary is the
        // window edge or a non-word byte. Both cases: this is a full token.
        if let Some(level) = level_for_token(&window[start..i]) {
            return Some(level);
        }
    }
    None
}

/// A byte that may appear inside a token (word char). Everything else is a
/// delimiter (space, brackets, `=`, `:`, punctuation, tab).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Map a delimited token to a level. Case-sensitive: only the uppercase and
/// lowercase forms, never title-case (avoids matching English prose).
fn level_for_token(w: &[u8]) -> Option<tell::LogLevel> {
    use tell::LogLevel::{Critical, Debug, Error, Info, Notice, Warning};
    Some(match w {
        b"TRACE" | b"trace" | b"DEBUG" | b"debug" => Debug,
        b"INFO" | b"info" => Info,
        b"NOTICE" | b"notice" => Notice,
        b"WARN" | b"warn" | b"WARNING" | b"warning" => Warning,
        b"ERR" | b"err" | b"ERROR" | b"error" => Error,
        b"CRIT" | b"crit" | b"CRITICAL" | b"critical" | b"FATAL" | b"fatal" | b"PANIC"
        | b"panic" => Critical,
        _ => return None,
    })
}
