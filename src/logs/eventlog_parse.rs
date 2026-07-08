//! Pure parsing and per-event processing for the Windows Event Log source.
//!
//! Platform-agnostic: this module compiles and is fully fixture-tested on the
//! macOS dev box. It turns one rendered Event XML string (from
//! `EvtRender(EvtRenderEventXml)`) into a structured [`EventEntry`], maps the
//! numeric event Level to a [`tell::LogLevel`], derives the service name and a
//! curated structured payload, synthesizes a body when no formatted message is
//! available, filters witness's own events, and ships via the sink — mirroring
//! `journal::process_entry` and `unified_parse::process_entry`.
//!
//! The Windows-only `Evt*` FFI pump lives in `eventlog.rs`; it calls
//! [`process_entry`] and persists the opaque bookmark via [`save_bookmark`].
//!
//! XXE safety: `quick-xml` resolves no DTDs or external/custom entities (it
//! decodes only the five predefined entities and numeric char refs), so
//! attacker-influenceable `<Data>` values cannot trigger entity expansion,
//! billion-laughs, or file/SSRF disclosure (spec 004 R7 / threat model).
//!
//! Most items are consumed only by the Windows-only `eventlog.rs` pump (and by
//! the fixture tests on every platform), so they read as dead code on a
//! non-Windows non-test build — allowed there, mirroring the config module's
//! platform-gated `dead_code` pattern.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use serde_json::{Map, Value};

use super::eventlog_filter::EventFilter;
use crate::sink::Sink;

/// Security-audit `Keywords` mask for a failed audit (`AUDIT_FAILURE` |
/// `CLASSIC`). Checked before the numeric Level so failed logons (all Level 0)
/// ship as `Error`, not `Info` (the NXLog `AUDIT_FAILURE → ERROR` model).
const AUDIT_FAILURE: u64 = 0x8010_0000_0000_0000;
/// Security-audit `Keywords` mask for a successful audit.
const AUDIT_SUCCESS: u64 = 0x8020_0000_0000_0000;

/// Maximum rendered-XML size we will parse. A malicious application can emit
/// huge event strings; over-cap events are dropped as [`ProcessResult::ParseFailed`]
/// rather than buffered unbounded (spec 004 threat model). Analogue of the
/// journald/unified `MAX_LINE_LEN`.
const MAX_XML_LEN: usize = 256 * 1024;

/// Witness's own provider identifier, filtered to prevent feedback loops
/// (spec 004 R5). Defense-in-depth: witness never registers an Event Log
/// provider (spec 005 R5), so this normally never matches.
const SELF_PROVIDER: &str = "witness";

/// Built-in default query: Level 0–4 (excludes 5 = Verbose) so the source is
/// actionable rather than a firehose (spec 004 R6). A configured
/// `eventlog_query` REPLACES this entirely. Passed verbatim to `EvtSubscribe`,
/// never through a shell.
const DEFAULT_QUERY: &str = "*[System[(Level=0 or Level=1 or Level=2 or Level=3 or Level=4)]]";

// ─── Types ───────────────────────────────────────────────────────────

/// Structured view of one rendered Event XML document: the shallow `System`
/// fields plus the ordered `EventData`/`UserData` value pairs. Field names are
/// snake_case and Winlogbeat/ECS-derived so the payload reads uniformly with
/// the journald / unified-log sources.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct EventEntry {
    /// `Provider Name` attribute.
    pub(crate) provider: String,
    /// `EventID` element text, masked to the low 16 bits for classic providers
    /// (see [`mask_event_id`]).
    pub(crate) event_id: String,
    /// `EventID` `Qualifiers` attribute (classic providers), if present.
    pub(crate) qualifiers: String,
    /// `Level` element as a number, if present and parseable.
    pub(crate) level: Option<u8>,
    /// `Task` element text.
    pub(crate) task: String,
    /// `Opcode` element text.
    pub(crate) opcode: String,
    /// `Keywords` element text (hex string like `0x8010000000000000`, verbatim).
    pub(crate) keywords: String,
    /// `Version` element text.
    pub(crate) version: String,
    /// `TimeCreated SystemTime` attribute, verbatim.
    pub(crate) time_created: String,
    /// `Computer` element text.
    pub(crate) computer: String,
    /// `Channel` element text.
    pub(crate) channel: String,
    /// `EventRecordID` element text.
    pub(crate) record_id: String,
    /// `Correlation ActivityID` attribute.
    pub(crate) activity_id: String,
    /// `Correlation RelatedActivityID` attribute.
    pub(crate) related_activity_id: String,
    /// `Execution ProcessID` attribute.
    pub(crate) process_id: String,
    /// `Execution ThreadID` attribute.
    pub(crate) thread_id: String,
    /// `Security UserID` attribute (the SID).
    pub(crate) user_sid: String,
    /// Ordered `(Name, value)` pairs from `EventData`/`UserData`. `Name` is the
    /// `<Data Name=...>` attribute (EventData), the element's local name
    /// (UserData), or a positional `paramN` for an unnamed `<Data>`.
    pub(crate) data: Vec<(Option<String>, String)>,
}

/// Outcome of processing one rendered event, mirroring `journal::ProcessResult`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProcessResult {
    /// Shipped or intentionally filtered. The caller advances the bookmark.
    Handled,
    /// SDK channel full — NOT shipped. Retry the same event; do not advance the
    /// bookmark past it.
    ChannelFull,
    /// Malformed or over-cap XML. Count-and-drop; the caller advances the
    /// bookmark (the event is consumed).
    ParseFailed,
}

/// XML parse failure. Deliberately opaque — the caller only distinguishes
/// success from failure (→ `ProcessResult::ParseFailed`).
#[derive(Debug, thiserror::Error)]
pub(crate) enum ParseError {
    #[error("event XML parse error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("event XML attribute error: {0}")]
    Attr(#[from] quick_xml::events::attributes::AttrError),
}

// ─── Public query knobs ──────────────────────────────────────────────

/// The built-in default XPath query (Level 0–4).
#[must_use]
pub fn default_query() -> &'static str {
    DEFAULT_QUERY
}

/// The effective query: the operator's `eventlog_query` override, else the
/// built-in default. Passed verbatim to `EvtSubscribe`.
#[must_use]
pub fn effective_query(configured: Option<&str>) -> String {
    configured
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_QUERY.to_string())
}

// ─── Level mapping ───────────────────────────────────────────────────

/// Map an event Level to a Tell log level (spec 004 R3).
///
/// `1`→Critical, `2`→Error, `3`→Warning, `4`→Info, `5`→Debug,
/// `0` (LogAlways)→Info, unknown/missing→Info.
#[must_use]
pub fn level_of(level: Option<u8>) -> tell::LogLevel {
    match level {
        Some(1) => tell::LogLevel::Critical,
        Some(2) => tell::LogLevel::Error,
        Some(3) => tell::LogLevel::Warning,
        Some(5) => tell::LogLevel::Debug,
        // 4 = Information, 0 = LogAlways, and any unknown/missing value.
        _ => tell::LogLevel::Info,
    }
}

// ─── Per-event processing ────────────────────────────────────────────

/// Process one rendered event.
///
/// `formatted_message` is the Windows layer's `EvtFormatMessage` output when a
/// publisher manifest exists, else `None` (the parser synthesizes a body).
/// `filter` applies the agent-side event-id / provider-exclusion knobs; a
/// filtered event returns [`ProcessResult::Handled`] so the bookmark advances
/// past it. Malformed/over-cap XML yields [`ProcessResult::ParseFailed`] and
/// never panics.
pub(crate) fn process_entry(
    xml: &str,
    formatted_message: Option<&str>,
    filter: &EventFilter,
    sink: &Sink,
) -> ProcessResult {
    if xml.len() > MAX_XML_LEN {
        return ProcessResult::ParseFailed;
    }
    let Ok(entry) = parse_event_xml(xml) else {
        return ProcessResult::ParseFailed;
    };

    // Self-feedback prevention (spec 004 R5): never re-ship witness's own events.
    if entry.provider == SELF_PROVIDER {
        return ProcessResult::Handled;
    }

    // Agent-side filtering (spec 004 R6): filtered events are consumed
    // (bookmark advances) but not shipped.
    if filter.excludes(&entry.provider, &entry.event_id) {
        return ProcessResult::Handled;
    }

    let (level, outcome) = resolve_severity(&entry);
    let service = service_of(&entry);
    let body = body_of(&entry, formatted_message);
    let payload = build_payload(&entry, outcome);

    if !sink.try_log_with_service(level, &body, None, Some(&service), payload) {
        return ProcessResult::ChannelFull;
    }
    ProcessResult::Handled
}

/// Resolve the Tell log level and an optional audit `outcome`, checking the
/// Security-audit `Keywords` BEFORE the numeric Level (spec 004 R1 / NXLog
/// model): a failed audit is `Error`+`failure`, a successful audit is
/// `Info`+`success`, otherwise the plain [`level_of`] mapping with no outcome.
pub(crate) fn resolve_severity(entry: &EventEntry) -> (tell::LogLevel, Option<&'static str>) {
    if let Some(keywords) = parse_keywords_hex(&entry.keywords) {
        if keywords & AUDIT_FAILURE == AUDIT_FAILURE {
            return (tell::LogLevel::Error, Some("failure"));
        }
        if keywords & AUDIT_SUCCESS == AUDIT_SUCCESS {
            return (tell::LogLevel::Info, Some("success"));
        }
    }
    (level_of(entry.level), None)
}

/// Parse a `Keywords` hex string (`0x8010000000000000`) to `u64`. `None` when
/// absent or malformed.
fn parse_keywords_hex(s: &str) -> Option<u64> {
    let hex = s.trim();
    let hex = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X"))?;
    u64::from_str_radix(hex, 16).ok()
}

/// Mask an `EventID` to its low 16 bits when it parses as a number `> 0xFFFF`:
/// classic providers pack qualifiers into the upper bits, and users expect the
/// low-16 ID (spec 004 R2). Non-numeric or in-range values pass through.
#[must_use]
pub(crate) fn mask_event_id(raw: &str) -> String {
    match raw.parse::<u32>() {
        Ok(n) if n > 0xFFFF => (n & 0xFFFF).to_string(),
        _ => raw.to_string(),
    }
}

/// Service name: the provider when non-empty, else `"unknown"` (analogue of
/// the journald `SYSLOG_IDENTIFIER` / unified-log `subsystem`).
pub(crate) fn service_of(entry: &EventEntry) -> String {
    if entry.provider.is_empty() {
        "unknown".to_string()
    } else {
        entry.provider.clone()
    }
}

/// Event body: the formatted message when present and non-empty; else the
/// non-empty `EventData` values joined by `", "` (Datadog `interpret_messages`
/// convention); else `"<provider> event <event_id>"`. Witness stays a
/// forwarder — never drops an event for lack of a manifest (spec 004 R3).
pub(crate) fn body_of(entry: &EventEntry, formatted_message: Option<&str>) -> String {
    if let Some(msg) = formatted_message {
        let trimmed = msg.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let mut parts = entry.data.iter().filter_map(|(_, v)| {
        let t = v.trim();
        (!t.is_empty()).then_some(t)
    });
    if let Some(first) = parts.next() {
        let mut body = first.to_string();
        for p in parts {
            body.push_str(", ");
            body.push_str(p);
        }
        return body;
    }

    let provider = if entry.provider.is_empty() {
        "unknown"
    } else {
        &entry.provider
    };
    format!("{provider} event {}", entry.event_id)
}

/// Curated structured payload: the System fields (snake_case, Winlogbeat/ECS
/// derived) plus the audit `outcome` and each named `EventData`/`UserData`
/// pair. Unnamed `<Data>` values are named positionally (`param1`, …) at parse
/// time, so they appear here too. Data keys are lowercased to read uniformly
/// with the journald / unified-log payloads. Returns `None` when nothing
/// survives (spec 004 R2/R3).
pub(crate) fn build_payload(entry: &EventEntry, outcome: Option<&str>) -> Option<Value> {
    let mut obj = Map::new();
    insert_non_empty(&mut obj, "provider", &entry.provider);
    insert_non_empty(&mut obj, "event_id", &entry.event_id);
    insert_non_empty(&mut obj, "qualifiers", &entry.qualifiers);
    if let Some(level) = entry.level {
        obj.insert("level".to_string(), Value::from(level));
    }
    insert_non_empty(&mut obj, "task", &entry.task);
    insert_non_empty(&mut obj, "opcode", &entry.opcode);
    insert_non_empty(&mut obj, "keywords", &entry.keywords);
    insert_non_empty(&mut obj, "version", &entry.version);
    insert_non_empty(&mut obj, "channel", &entry.channel);
    insert_non_empty(&mut obj, "computer", &entry.computer);
    insert_non_empty(&mut obj, "record_id", &entry.record_id);
    insert_non_empty(&mut obj, "time_created", &entry.time_created);
    insert_non_empty(&mut obj, "activity_id", &entry.activity_id);
    insert_non_empty(&mut obj, "related_activity_id", &entry.related_activity_id);
    insert_non_empty(&mut obj, "process_id", &entry.process_id);
    insert_non_empty(&mut obj, "thread_id", &entry.thread_id);
    insert_non_empty(&mut obj, "user_sid", &entry.user_sid);
    if let Some(o) = outcome {
        obj.insert("outcome".to_string(), Value::String(o.to_string()));
    }

    for (name, value) in &entry.data {
        if let Some(n) = name {
            let key = n.to_lowercase();
            // Reserved System keys win over app-named data on collision.
            obj.entry(key)
                .or_insert_with(|| Value::String(value.clone()));
        }
    }

    if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    }
}

fn insert_non_empty(obj: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        obj.insert(key.to_string(), Value::String(value.to_string()));
    }
}

// ─── Bookmark checkpoint (opaque round-trip) ─────────────────────────

/// Per-channel bookmark file path under [`crate::config::state_dir`]. The
/// channel name is sanitized so a channel like
/// `Microsoft-Windows-.../Operational` cannot escape the state dir
/// (spec 004 R4 / threat model).
#[must_use]
pub(crate) fn bookmark_path(channel: &str) -> PathBuf {
    Path::new(crate::config::state_dir())
        .join(format!("eventlog_bookmark_{}", sanitize_channel(channel)))
}

/// Replace every character that is not ASCII-alphanumeric, `-`, or `_` with
/// `_`. This neutralizes path separators (`/`, `\`) and `.` (so `..` cannot
/// form), keeping the derived file inside the state dir.
#[must_use]
pub(crate) fn sanitize_channel(channel: &str) -> String {
    channel
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Load a persisted bookmark XML string. A missing, empty, or non-UTF-8 file
/// yields `None` (start from future events, never a crash — spec 004 R4).
#[must_use]
pub(crate) fn load_bookmark(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist a bookmark XML string atomically via the shared checkpoint writer.
pub(crate) fn save_bookmark(path: &Path, xml: &str) {
    super::source::write_checkpoint(path, xml.as_bytes());
}

// ─── XML parsing ─────────────────────────────────────────────────────

/// Which top-level `System`/`EventData`/`UserData` section we are inside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    System,
    EventData,
    UserData,
}

/// Parse one rendered Event XML document into an [`EventEntry`].
///
/// Streaming pull parse — no DTD/external-entity resolution (XXE-safe). Handles
/// namespaces (via local names), self-closing elements, entity-encoded and
/// CDATA `<Data>` values, and missing fields.
pub(crate) fn parse_event_xml(xml: &str) -> Result<EventEntry, ParseError> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text(false);

    let mut entry = EventEntry::default();
    let mut section = Section::None;
    let mut cur_name: Vec<u8> = Vec::new();
    let mut data_attr_name: Option<Option<String>> = None;
    let mut unnamed_count: u32 = 0;
    let mut text = String::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let local = local_name(e.name().as_ref());
                enter_section(&mut section, &local);
                on_open(&e, &local, section, &mut entry, &mut data_attr_name)?;
                cur_name = local;
                text.clear();
            }
            Event::Empty(e) => {
                let local = local_name(e.name().as_ref());
                enter_section(&mut section, &local);
                on_open(&e, &local, section, &mut entry, &mut data_attr_name)?;
                // Self-closing element carries no text.
                on_close(
                    &local,
                    section,
                    "",
                    &mut entry,
                    &mut data_attr_name,
                    &mut unnamed_count,
                );
                text.clear();
            }
            Event::Text(e) => {
                // Literal text between markup. quick-xml 0.41 emits entity
                // references as separate `GeneralRef` events, so `Text` needs
                // no unescaping.
                if let Ok(t) = e.decode() {
                    text.push_str(&t);
                }
            }
            Event::GeneralRef(e) => {
                // An entity/char reference (`&lt;`, `&#60;`, …). Resolve only the
                // five predefined entities and numeric char refs — no DTD or
                // custom/external entities are ever expanded (XXE-safe).
                if let Ok(name) = e.decode()
                    && let Ok(resolved) = quick_xml::escape::unescape(&format!("&{name};"))
                {
                    text.push_str(&resolved);
                }
            }
            Event::CData(e) => {
                text.push_str(&String::from_utf8_lossy(&e.into_inner()));
            }
            Event::End(e) => {
                let local = local_name(e.name().as_ref());
                if local == cur_name {
                    on_close(
                        &local,
                        section,
                        &text,
                        &mut entry,
                        &mut data_attr_name,
                        &mut unnamed_count,
                    );
                }
                text.clear();
                cur_name.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(entry)
}

/// Local name (strip any `prefix:`) of an element name.
fn local_name(name: &[u8]) -> Vec<u8> {
    match name.iter().position(|&b| b == b':') {
        Some(i) => name[i + 1..].to_vec(),
        None => name.to_vec(),
    }
}

/// Update the current section when a container element opens.
fn enter_section(section: &mut Section, local: &[u8]) {
    match local {
        b"System" => *section = Section::System,
        b"EventData" => *section = Section::EventData,
        b"UserData" => *section = Section::UserData,
        _ => {}
    }
}

/// Handle an element's opening: capture attribute-borne System fields
/// (`Provider Name`, `EventID Qualifiers`, `TimeCreated SystemTime`,
/// `Correlation` / `Execution` / `Security` attributes) and the `Data Name`.
fn on_open(
    e: &quick_xml::events::BytesStart<'_>,
    local: &[u8],
    section: Section,
    entry: &mut EventEntry,
    data_attr_name: &mut Option<Option<String>>,
) -> Result<(), ParseError> {
    match (section, local) {
        (Section::System, b"Provider") => set_from_attr(e, b"Name", &mut entry.provider)?,
        (Section::System, b"EventID") => set_from_attr(e, b"Qualifiers", &mut entry.qualifiers)?,
        (Section::System, b"TimeCreated") => {
            set_from_attr(e, b"SystemTime", &mut entry.time_created)?;
        }
        (Section::System, b"Correlation") => {
            set_from_attr(e, b"ActivityID", &mut entry.activity_id)?;
            set_from_attr(e, b"RelatedActivityID", &mut entry.related_activity_id)?;
        }
        (Section::System, b"Execution") => {
            set_from_attr(e, b"ProcessID", &mut entry.process_id)?;
            set_from_attr(e, b"ThreadID", &mut entry.thread_id)?;
        }
        (Section::System, b"Security") => set_from_attr(e, b"UserID", &mut entry.user_sid)?,
        (Section::EventData, b"Data") => {
            *data_attr_name = Some(attr(e, b"Name")?);
        }
        _ => {}
    }
    Ok(())
}

/// Assign `field` from a named attribute if present (leaving it untouched
/// otherwise).
fn set_from_attr(
    e: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    field: &mut String,
) -> Result<(), ParseError> {
    if let Some(v) = attr(e, name)? {
        *field = v;
    }
    Ok(())
}

/// Handle an element's close: assign the accumulated text to the right field.
/// `unnamed_count` numbers unnamed `<Data>` values positionally (`param1`, …).
fn on_close(
    local: &[u8],
    section: Section,
    text: &str,
    entry: &mut EventEntry,
    data_attr_name: &mut Option<Option<String>>,
    unnamed_count: &mut u32,
) {
    let trimmed = text.trim();
    match section {
        Section::System => match local {
            b"EventID" => entry.event_id = mask_event_id(trimmed),
            b"Level" => entry.level = trimmed.parse().ok(),
            b"Task" => entry.task = trimmed.to_string(),
            b"Opcode" => entry.opcode = trimmed.to_string(),
            b"Keywords" => entry.keywords = trimmed.to_string(),
            b"Version" => entry.version = trimmed.to_string(),
            b"EventRecordID" => entry.record_id = trimmed.to_string(),
            b"Channel" => entry.channel = trimmed.to_string(),
            b"Computer" => entry.computer = trimmed.to_string(),
            _ => {}
        },
        Section::EventData if local == b"Data" => {
            // Unnamed <Data> get a positional param name (the universal
            // convention) so they survive into the payload (spec 004 R2).
            let name = data_attr_name.take().flatten().unwrap_or_else(|| {
                *unnamed_count += 1;
                format!("param{unnamed_count}")
            });
            entry.data.push((Some(name), trimmed.to_string()));
        }
        Section::UserData if local != b"UserData" && !trimmed.is_empty() => {
            let name = String::from_utf8_lossy(local).into_owned();
            entry.data.push((Some(name), trimmed.to_string()));
        }
        _ => {}
    }
}

/// Fetch and decode a named attribute's value, if present.
fn attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Result<Option<String>, ParseError> {
    match e.try_get_attribute(name)? {
        // `normalized_value` decodes and resolves the predefined entities per
        // XML attribute-value normalization (no external entities — XXE-safe).
        Some(a) => Ok(Some(
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                .into_owned(),
        )),
        None => Ok(None),
    }
}
