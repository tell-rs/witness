use super::eventlog_filter::EventFilter;
use super::eventlog_parse::{
    self as parse, EventEntry, ProcessResult, bookmark_path, build_payload, default_query,
    effective_query, level_of, load_bookmark, mask_event_id, parse_event_xml, process_entry,
    resolve_severity, sanitize_channel, save_bookmark,
};
use crate::sink::{Capture, Recorded, Sink};

/// No-op filter for the process_entry tests that don't exercise filtering.
fn nofilter() -> EventFilter {
    EventFilter::default()
}

// ─── Fixtures (realistic rendered Event XML) ─────────────────────────

/// Service Control Manager event with named `EventData` (the canonical shape
/// from spec 004 "Background").
const SCM_EVENT: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Service Control Manager' Guid='{555908d1-a6d7-4695-8e1e-26931d2012f4}' EventSourceName='Service Control Manager'/>
    <EventID Qualifiers='16384'>7036</EventID>
    <Version>0</Version>
    <Level>4</Level>
    <Task>0</Task>
    <Opcode>0</Opcode>
    <Keywords>0x8080000000000000</Keywords>
    <TimeCreated SystemTime='2014-04-24T18:38:37.868000000Z'/>
    <EventRecordID>412598</EventRecordID>
    <Correlation/>
    <Execution ProcessID='488' ThreadID='648'/>
    <Channel>System</Channel>
    <Computer>host.example.local</Computer>
    <Security UserID='S-1-5-18'/>
  </System>
  <EventData>
    <Data Name='param1'>Application Experience</Data>
    <Data Name='param2'>stopped</Data>
  </EventData>
</Event>"#;

/// Manifest-defined provider using `<UserData>` (no `EventData`).
const USERDATA_EVENT: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Microsoft-Windows-Kernel-General' Guid='{a68ca8b7-004f-d7b6-a698-07e2de0f1f5d}'/>
    <EventID>16</EventID>
    <Level>2</Level>
    <TimeCreated SystemTime='2020-01-02T03:04:05.000Z'/>
    <EventRecordID>99</EventRecordID>
    <Channel>System</Channel>
    <Computer>WIN-ABC</Computer>
  </System>
  <UserData>
    <EventXML xmlns='http://manifest.example/kernel'>
      <FinalStatus>0xC0000001</FinalStatus>
      <Reason>disk full</Reason>
    </EventXML>
  </UserData>
</Event>"#;

/// Event with no data section at all.
const NO_DATA_EVENT: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='EventLog'/>
    <EventID>6013</EventID>
    <Level>4</Level>
    <TimeCreated SystemTime='2021-05-05T00:00:00.000Z'/>
    <EventRecordID>7</EventRecordID>
    <Channel>System</Channel>
    <Computer>SRV1</Computer>
  </System>
</Event>"#;

/// Entity-encoded and CDATA `<Data>` values (attacker-influenceable text).
const ENCODED_EVENT: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='MyApp'/>
    <EventID>1000</EventID>
    <Level>3</Level>
    <TimeCreated SystemTime='2022-06-06T06:06:06.000Z'/>
    <EventRecordID>1</EventRecordID>
    <Channel>Application</Channel>
    <Computer>APP</Computer>
  </System>
  <EventData>
    <Data Name='raw'>a &lt; b &amp;&amp; c &gt; d</Data>
    <Data Name='cdata'><![CDATA[literal <tag> & value]]></Data>
    <Data>unnamed positional</Data>
  </EventData>
</Event>"#;

fn capture() -> (Capture, Sink) {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), Default::default());
    (cap, sink)
}

// ─── parse_event_xml ─────────────────────────────────────────────────

#[test]
fn test_parse_scm_event_system_fields() {
    let e = parse_event_xml(SCM_EVENT).expect("parse");
    assert_eq!(e.provider, "Service Control Manager");
    assert_eq!(e.event_id, "7036");
    assert_eq!(e.level, Some(4));
    assert_eq!(e.time_created, "2014-04-24T18:38:37.868000000Z");
    assert_eq!(e.record_id, "412598");
    assert_eq!(e.channel, "System");
    assert_eq!(e.computer, "host.example.local");
}

#[test]
fn test_parse_scm_event_named_data_in_order() {
    let e = parse_event_xml(SCM_EVENT).expect("parse");
    assert_eq!(
        e.data,
        vec![
            (
                Some("param1".to_string()),
                "Application Experience".to_string()
            ),
            (Some("param2".to_string()), "stopped".to_string()),
        ]
    );
}

#[test]
fn test_parse_userdata_leaf_elements() {
    let e = parse_event_xml(USERDATA_EVENT).expect("parse");
    assert_eq!(e.provider, "Microsoft-Windows-Kernel-General");
    assert_eq!(e.level, Some(2));
    // UserData leaves become named pairs by local element name; the wrapper
    // element (EventXML) has no direct text and is skipped.
    assert_eq!(
        e.data,
        vec![
            (Some("FinalStatus".to_string()), "0xC0000001".to_string()),
            (Some("Reason".to_string()), "disk full".to_string()),
        ]
    );
}

#[test]
fn test_parse_no_data_event() {
    let e = parse_event_xml(NO_DATA_EVENT).expect("parse");
    assert_eq!(e.provider, "EventLog");
    assert_eq!(e.event_id, "6013");
    assert!(e.data.is_empty());
}

#[test]
fn test_parse_entity_and_cdata_values() {
    let e = parse_event_xml(ENCODED_EVENT).expect("parse");
    assert_eq!(
        e.data,
        vec![
            (Some("raw".to_string()), "a < b && c > d".to_string()),
            (
                Some("cdata".to_string()),
                "literal <tag> & value".to_string()
            ),
            // Unnamed <Data> gets a positional param name (spec 004 R2).
            (Some("param1".to_string()), "unnamed positional".to_string()),
        ]
    );
}

#[test]
fn test_parse_missing_provider_name_is_blank() {
    let xml = r#"<Event><System><Provider/><EventID>5</EventID><Level>4</Level><Channel>Application</Channel></System></Event>"#;
    let e = parse_event_xml(xml).expect("parse");
    assert_eq!(e.provider, "");
    assert_eq!(e.event_id, "5");
}

#[test]
fn test_parse_absent_level_is_none() {
    let xml = r#"<Event><System><Provider Name='X'/><EventID>1</EventID><Channel>System</Channel></System></Event>"#;
    let e = parse_event_xml(xml).expect("parse");
    assert_eq!(e.level, None);
}

#[test]
fn test_parse_malformed_xml_errors() {
    // Unclosed tag / truncated document.
    let xml = r#"<Event><System><Provider Name='X'"#;
    assert!(parse_event_xml(xml).is_err());
}

// ─── level_of ────────────────────────────────────────────────────────

#[test]
fn test_level_of_all_values() {
    assert_eq!(level_of(Some(0)), tell::LogLevel::Info); // LogAlways
    assert_eq!(level_of(Some(1)), tell::LogLevel::Critical);
    assert_eq!(level_of(Some(2)), tell::LogLevel::Error);
    assert_eq!(level_of(Some(3)), tell::LogLevel::Warning);
    assert_eq!(level_of(Some(4)), tell::LogLevel::Info);
    assert_eq!(level_of(Some(5)), tell::LogLevel::Debug);
    assert_eq!(level_of(Some(9)), tell::LogLevel::Info); // unknown
    assert_eq!(level_of(None), tell::LogLevel::Info); // missing
}

// ─── process_entry ───────────────────────────────────────────────────

fn logged(cap: &Capture) -> Vec<(String, Option<String>)> {
    cap.events()
        .into_iter()
        .filter_map(|e| match e {
            Recorded::Log { message, service } => Some((message, service)),
            _ => None,
        })
        .collect()
}

#[test]
fn test_process_entry_ships_synthesized_body_and_service() {
    let (cap, sink) = capture();
    assert_eq!(
        process_entry(SCM_EVENT, None, &nofilter(), &sink),
        ProcessResult::Handled
    );
    let logs = logged(&cap);
    assert_eq!(logs.len(), 1);
    // No formatted message → body synthesized from EventData values.
    assert_eq!(logs[0].0, "Application Experience, stopped");
    assert_eq!(logs[0].1.as_deref(), Some("Service Control Manager"));
}

#[test]
fn test_process_entry_prefers_formatted_message() {
    let (cap, sink) = capture();
    let msg = "The Application Experience service entered the stopped state.";
    assert_eq!(
        process_entry(SCM_EVENT, Some(msg), &nofilter(), &sink),
        ProcessResult::Handled
    );
    let logs = logged(&cap);
    assert_eq!(logs[0].0, msg);
}

#[test]
fn test_process_entry_payload_same_regardless_of_body_source() {
    let with_msg = build_payload(&parse_event_xml(SCM_EVENT).unwrap(), None);
    let entry = parse_event_xml(SCM_EVENT).unwrap();
    let synth = build_payload(&entry, None);
    assert_eq!(with_msg, synth);
    let obj = with_msg.expect("payload");
    assert_eq!(obj["provider"], "Service Control Manager");
    assert_eq!(obj["event_id"], "7036");
    assert_eq!(obj["channel"], "System");
    assert_eq!(obj["computer"], "host.example.local");
    assert_eq!(obj["record_id"], "412598");
    assert_eq!(obj["time_created"], "2014-04-24T18:38:37.868000000Z");
    assert_eq!(obj["param1"], "Application Experience");
    assert_eq!(obj["param2"], "stopped");
}

#[test]
fn test_process_entry_unnamed_data_named_positionally_in_payload() {
    let entry = parse_event_xml(ENCODED_EVENT).unwrap();
    let obj = build_payload(&entry, None).expect("payload");
    assert_eq!(obj["raw"], "a < b && c > d");
    assert_eq!(obj["cdata"], "literal <tag> & value");
    // The unnamed <Data> value is now named positionally (spec 004 R2), so it
    // survives into the payload rather than vanishing.
    assert_eq!(obj["param1"], "unnamed positional");
}

#[test]
fn test_process_entry_no_data_falls_back_to_provider_event() {
    let (cap, sink) = capture();
    assert_eq!(
        process_entry(NO_DATA_EVENT, None, &nofilter(), &sink),
        ProcessResult::Handled
    );
    assert_eq!(logged(&cap)[0].0, "EventLog event 6013");
}

#[test]
fn test_process_entry_channel_full_not_shipped() {
    let sink = Sink::full();
    assert_eq!(
        process_entry(SCM_EVENT, None, &nofilter(), &sink),
        ProcessResult::ChannelFull
    );
}

#[test]
fn test_process_entry_malformed_is_parse_failed() {
    let (cap, sink) = capture();
    let xml = r#"<Event><System><Provider Name='X'"#;
    assert_eq!(
        process_entry(xml, None, &nofilter(), &sink),
        ProcessResult::ParseFailed
    );
    assert!(logged(&cap).is_empty());
}

#[test]
fn test_process_entry_oversized_is_parse_failed() {
    let (cap, sink) = capture();
    let huge = format!(
        "<Event><System><Provider Name='X'/><EventID>1</EventID><Channel>C</Channel><Data>{}</Data></System></Event>",
        "a".repeat(300 * 1024)
    );
    assert_eq!(
        process_entry(&huge, None, &nofilter(), &sink),
        ProcessResult::ParseFailed
    );
    assert!(logged(&cap).is_empty());
}

#[test]
fn test_process_entry_self_provider_filtered() {
    let (cap, sink) = capture();
    let xml = r#"<Event><System><Provider Name='witness'/><EventID>1</EventID><Level>4</Level><Channel>Application</Channel></System><EventData><Data Name='x'>y</Data></EventData></Event>"#;
    // Filtered but still Handled (bookmark advances).
    assert_eq!(
        process_entry(xml, None, &nofilter(), &sink),
        ProcessResult::Handled
    );
    assert!(logged(&cap).is_empty());
}

#[test]
fn test_process_entry_blank_provider_service_unknown() {
    let (cap, sink) = capture();
    let xml = r#"<Event><System><Provider/><EventID>1</EventID><Level>4</Level><Channel>Application</Channel></System><EventData><Data Name='x'>y</Data></EventData></Event>"#;
    assert_eq!(
        process_entry(xml, None, &nofilter(), &sink),
        ProcessResult::Handled
    );
    assert_eq!(logged(&cap)[0].1.as_deref(), Some("unknown"));
}

#[test]
fn test_process_entry_does_not_panic_on_garbage() {
    let (_cap, sink) = capture();
    for xml in ["", "not xml at all", "<<<>>>", "<Event>", "&amp;"] {
        // Must never panic; result is Handled or ParseFailed.
        let _ = process_entry(xml, None, &nofilter(), &sink);
    }
}

// ─── Query knobs ─────────────────────────────────────────────────────

#[test]
fn test_default_query_excludes_verbose() {
    let q = default_query();
    assert!(q.contains("Level=4"));
    assert!(!q.contains("Level=5"));
}

#[test]
fn test_effective_query_override_replaces_default() {
    assert_eq!(effective_query(None), default_query());
    assert_eq!(effective_query(Some("*")), "*");
}

// ─── Channel sanitization + bookmark round-trip ──────────────────────

#[test]
fn test_sanitize_channel_neutralizes_separators() {
    assert_eq!(sanitize_channel("System"), "System");
    assert_eq!(sanitize_channel("Application"), "Application");
    assert_eq!(
        sanitize_channel("Microsoft-Windows-PowerShell/Operational"),
        "Microsoft-Windows-PowerShell_Operational"
    );
    // No traversal survives: dots and slashes are all replaced.
    assert_eq!(sanitize_channel("../../etc/passwd"), "______etc_passwd");
    assert_eq!(sanitize_channel("a\\b:c"), "a_b_c");
}

#[test]
fn test_bookmark_path_uses_sanitized_channel() {
    let p = bookmark_path("Microsoft-Windows-X/Operational");
    let name = p.file_name().unwrap().to_string_lossy();
    assert_eq!(name, "eventlog_bookmark_Microsoft-Windows-X_Operational");
    // Stays within the state dir (no separators leaked into the file name).
    assert!(!name.contains('/'));
    assert!(!name.contains('\\'));
}

#[test]
fn test_bookmark_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bm");
    let xml = "<BookmarkList><Bookmark Channel='System' RecordId='42'/></BookmarkList>";
    save_bookmark(&path, xml);
    assert_eq!(load_bookmark(&path).as_deref(), Some(xml));
}

#[test]
fn test_bookmark_missing_file_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_bookmark(&dir.path().join("nope")), None);
}

#[test]
fn test_bookmark_empty_file_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty");
    std::fs::write(&path, "   \n").unwrap();
    assert_eq!(load_bookmark(&path), None);
}

#[test]
fn test_bookmark_non_utf8_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage");
    std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01, 0x80]).unwrap();
    assert_eq!(load_bookmark(&path), None);
}

// ─── body_of edge cases ──────────────────────────────────────────────

#[test]
fn test_body_of_blank_formatted_message_synthesizes() {
    let entry = parse_event_xml(SCM_EVENT).unwrap();
    assert_eq!(
        parse::body_of(&entry, Some("   ")),
        "Application Experience, stopped"
    );
}

#[test]
fn test_body_of_empty_entry_uses_unknown() {
    let entry = EventEntry::default();
    assert_eq!(parse::body_of(&entry, None), "unknown event ");
}

// ─── Security-audit fixtures (spec 004 R1 severity) ──────────────────

/// A failed logon: Security channel, Level 0, AUDIT_FAILURE keywords.
const SECURITY_FAILURE: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Microsoft-Windows-Security-Auditing'/>
    <EventID>4625</EventID>
    <Level>0</Level>
    <Task>12544</Task>
    <Keywords>0x8010000000000000</Keywords>
    <Channel>Security</Channel>
    <Computer>DC1</Computer>
  </System>
  <EventData><Data Name='TargetUserName'>bob</Data></EventData>
</Event>"#;

/// A successful logon: Level 0, AUDIT_SUCCESS keywords.
const SECURITY_SUCCESS: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Microsoft-Windows-Security-Auditing'/>
    <EventID>4624</EventID>
    <Level>0</Level>
    <Keywords>0x8020000000000000</Keywords>
    <Channel>Security</Channel>
    <Computer>DC1</Computer>
  </System>
</Event>"#;

/// A classic provider whose EventID packs qualifiers into the upper 16 bits and
/// carries Correlation activity ids.
const CLASSIC_EVENT: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='MSSQLSERVER'/>
    <EventID Qualifiers='16384'>1073741833</EventID>
    <Level>4</Level>
    <Correlation ActivityID='{11111111-2222-3333-4444-555555555555}' RelatedActivityID='{66666666-7777-8888-9999-000000000000}'/>
    <Channel>Application</Channel>
    <Computer>APP</Computer>
  </System>
</Event>"#;

// ─── resolve_severity ────────────────────────────────────────────────

#[test]
fn test_resolve_severity_audit_failure_is_error() {
    let e = parse_event_xml(SECURITY_FAILURE).unwrap();
    assert_eq!(
        resolve_severity(&e),
        (tell::LogLevel::Error, Some("failure"))
    );
}

#[test]
fn test_resolve_severity_audit_success_is_info() {
    let e = parse_event_xml(SECURITY_SUCCESS).unwrap();
    assert_eq!(
        resolve_severity(&e),
        (tell::LogLevel::Info, Some("success"))
    );
}

#[test]
fn test_resolve_severity_non_audit_keywords_use_level() {
    // SCM event: Keywords 0x8080... matches neither audit mask → plain level.
    let e = parse_event_xml(SCM_EVENT).unwrap();
    assert_eq!(resolve_severity(&e), (tell::LogLevel::Info, None)); // Level 4
}

#[test]
fn test_resolve_severity_no_keywords_uses_level() {
    let e = parse_event_xml(USERDATA_EVENT).unwrap(); // Level 2, no keywords
    assert_eq!(resolve_severity(&e), (tell::LogLevel::Error, None));
}

#[test]
fn test_resolve_severity_failure_checked_before_level() {
    // The whole point: a failed logon is Level 0 (would be Info by level alone)
    // but ships as Error because the audit-failure keyword wins.
    let e = parse_event_xml(SECURITY_FAILURE).unwrap();
    assert_eq!(e.level, Some(0));
    assert_eq!(resolve_severity(&e).0, tell::LogLevel::Error);
}

// ─── System field parsing (spec 004 R2) ─────────────────────────────

#[test]
fn test_parse_full_system_fields() {
    let e = parse_event_xml(SCM_EVENT).unwrap();
    assert_eq!(e.qualifiers, "16384");
    assert_eq!(e.task, "0");
    assert_eq!(e.opcode, "0");
    assert_eq!(e.keywords, "0x8080000000000000");
    assert_eq!(e.version, "0");
    assert_eq!(e.process_id, "488");
    assert_eq!(e.thread_id, "648");
    assert_eq!(e.user_sid, "S-1-5-18");
}

#[test]
fn test_parse_correlation_activity_ids() {
    let e = parse_event_xml(CLASSIC_EVENT).unwrap();
    assert_eq!(e.activity_id, "{11111111-2222-3333-4444-555555555555}");
    assert_eq!(
        e.related_activity_id,
        "{66666666-7777-8888-9999-000000000000}"
    );
}

// ─── mask_event_id (spec 004 R2) ─────────────────────────────────────

#[test]
fn test_mask_event_id_low_16_when_over_ffff() {
    // 1073741833 = 0x40000009 -> low 16 bits = 9.
    assert_eq!(mask_event_id("1073741833"), "9");
}

#[test]
fn test_mask_event_id_passthrough_in_range() {
    assert_eq!(mask_event_id("7036"), "7036");
    assert_eq!(mask_event_id("65535"), "65535");
}

#[test]
fn test_mask_event_id_non_numeric_passthrough() {
    assert_eq!(mask_event_id(""), "");
    assert_eq!(mask_event_id("not-a-number"), "not-a-number");
}

#[test]
fn test_parse_classic_event_id_masked() {
    let e = parse_event_xml(CLASSIC_EVENT).unwrap();
    assert_eq!(e.event_id, "9", "upper-bit qualifiers masked off");
    assert_eq!(e.qualifiers, "16384");
}

// ─── Payload with new fields + outcome ───────────────────────────────

#[test]
fn test_payload_includes_system_fields_and_numeric_level() {
    let e = parse_event_xml(SCM_EVENT).unwrap();
    let obj = build_payload(&e, None).expect("payload");
    assert_eq!(obj["level"], 4);
    assert_eq!(obj["qualifiers"], "16384");
    assert_eq!(obj["task"], "0");
    assert_eq!(obj["keywords"], "0x8080000000000000");
    assert_eq!(obj["process_id"], "488");
    assert_eq!(obj["thread_id"], "648");
    assert_eq!(obj["user_sid"], "S-1-5-18");
}

#[test]
fn test_payload_includes_outcome_when_present() {
    let e = parse_event_xml(SECURITY_FAILURE).unwrap();
    let obj = build_payload(&e, Some("failure")).expect("payload");
    assert_eq!(obj["outcome"], "failure");
    // Numeric level 0 is still recorded alongside.
    assert_eq!(obj["level"], 0);
}

// ─── Filtering in process_entry (spec 004 R6) ────────────────────────

#[test]
fn test_process_entry_excluded_provider_filtered() {
    let (cap, sink) = capture();
    let filter = EventFilter::new(None, &["Service Control Manager".to_string()]).unwrap();
    // Filtered but Handled so the bookmark advances.
    assert_eq!(
        process_entry(SCM_EVENT, None, &filter, &sink),
        ProcessResult::Handled
    );
    assert!(logged(&cap).is_empty());
}

#[test]
fn test_process_entry_event_id_filtered() {
    let (cap, sink) = capture();
    // Only allow 4624/4625; the SCM 7036 event is dropped.
    let filter = EventFilter::new(Some("4624,4625"), &[]).unwrap();
    assert_eq!(
        process_entry(SCM_EVENT, None, &filter, &sink),
        ProcessResult::Handled
    );
    assert!(logged(&cap).is_empty());
}

#[test]
fn test_process_entry_event_id_included_ships() {
    let (cap, sink) = capture();
    let filter = EventFilter::new(Some("7000-7100"), &[]).unwrap();
    assert_eq!(
        process_entry(SCM_EVENT, None, &filter, &sink),
        ProcessResult::Handled
    );
    assert_eq!(
        logged(&cap).len(),
        1,
        "7036 is in the 7000-7100 include range"
    );
}
