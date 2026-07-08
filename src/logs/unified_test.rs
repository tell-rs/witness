use std::time::{Duration, Instant};

use super::unified;
use super::unified_parse::{self as parse, Checkpoint, LogEntry, UnifiedResult};
use crate::sink::{Capture, Recorded, Sink};

// Real NDJSON lines captured live from `/usr/bin/log show --style ndjson` on
// macOS 26 (see spec 001 "Background"). Kept verbatim as parser fixtures.
const ERROR_LINE: &str = r#"{"timezoneName": "", "messageType": "Error", "eventType": "logEvent", "source": null, "formatString": "sending to %@ failed: %{darwin.errno}d", "userID": 65, "activityIdentifier": 0, "subsystem": "com.apple.mdns", "category": "resolver", "threadID": 82295663, "senderImageUUID": "DC303AA1-3B96-387C-8C31-1D5AC3485DF1", "backtrace": {"frames": [{"imageOffset": 879788, "imageUUID": "DC303AA1-3B96-387C-8C31-1D5AC3485DF1"}]}, "bootUUID": "28179658-9E7F-4BC6-BF41-B01E1AEE4DF1", "processImagePath": "/usr/sbin/mDNSResponder", "senderImagePath": "/usr/sbin/mDNSResponder", "timestamp": "2026-07-07 16:02:38.911816+0200", "machTimestamp": 78974278837578, "eventMessage": "sending to <IPv4:BBSiKqae> failed: [32: Broken pipe]", "processImageUUID": "DC303AA1-3B96-387C-8C31-1D5AC3485DF1", "traceID": 5549548751687684, "processID": 488, "senderProgramCounter": 879788, "parentActivityIdentifier": 0}"#;

const FAULT_LINE: &str = r#"{"messageType": "Fault", "eventType": "logEvent", "subsystem": "com.apple.runningboard", "category": "process", "threadID": 82277467, "bootUUID": "28179658-9E7F-4BC6-BF41-B01E1AEE4DF1", "processImagePath": "/usr/libexec/runningboardd", "timestamp": "2026-07-07 16:02:39.109492+0200", "machTimestamp": 78974283581802, "eventMessage": "Two equal instances have unequal identities.", "processID": 410}"#;

const ACTIVITY_LINE: &str = r#"{"timezoneName": "", "eventType": "activityCreateEvent", "subsystem": "", "category": "", "threadID": 82277467, "bootUUID": "28179658-9E7F-4BC6-BF41-B01E1AEE4DF1", "processImagePath": "/usr/libexec/runningboardd", "timestamp": "2026-07-07 16:02:39.108625+0200", "machTimestamp": 78974283561000, "eventMessage": "lookupHandleForPredicate", "processID": 410}"#;

const DEFAULT_LINE: &str = r#"{"messageType": "Default", "eventType": "logEvent", "subsystem": "com.apple.mdns", "category": "resolver", "bootUUID": "28179658-9E7F-4BC6-BF41-B01E1AEE4DF1", "processImagePath": "/usr/sbin/mDNSResponder", "timestamp": "2026-07-07 16:02:38.911841+0200", "machTimestamp": 78974278838164, "eventMessage": "Sent 27-byte query", "processID": 488}"#;

const ERROR_MACH: u64 = 78974278837578;
const BOOT: &str = "28179658-9E7F-4BC6-BF41-B01E1AEE4DF1";

fn capture() -> (Capture, Sink) {
    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), Default::default());
    (cap, sink)
}

fn parse_entry(line: &str) -> LogEntry {
    serde_json::from_str::<LogEntry>(line).expect("fixture parses")
}

// --- R1: parse & event-type filtering ---

#[test]
fn test_process_error_line_ships_with_service_and_checkpoint() {
    let (cap, sink) = capture();
    let result = parse::process_entry(ERROR_LINE, &sink, None);
    match result {
        UnifiedResult::Handled(Some(cp)) => {
            assert_eq!(cp.mach_timestamp, ERROR_MACH);
            assert_eq!(cp.boot_uuid, BOOT);
            assert_eq!(cp.wall_timestamp, "2026-07-07 16:02:38.911816+0200");
        }
        other => panic!("expected Handled(Some), got {other:?}"),
    }
    let events = cap.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        Recorded::Log { message, service } => {
            assert_eq!(
                message,
                "sending to <IPv4:BBSiKqae> failed: [32: Broken pipe]"
            );
            assert_eq!(service.as_deref(), Some("com.apple.mdns"));
        }
        other => panic!("expected Log, got {other:?}"),
    }
}

#[test]
fn test_activity_event_skipped_but_checkpoint_advances() {
    let (cap, sink) = capture();
    let result = parse::process_entry(ACTIVITY_LINE, &sink, None);
    // Structural noise: not shipped, but the checkpoint still advances past it.
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(cap.events().is_empty(), "activity events must not ship");
}

#[test]
fn test_garbage_line_is_parse_failed() {
    let sink = Sink::discard();
    assert_eq!(
        parse::process_entry("{not valid json", &sink, None),
        UnifiedResult::ParseFailed
    );
}

// --- R2: severity mapping ---

#[test]
fn test_message_type_to_level_all() {
    use tell::LogLevel;
    assert!(matches!(
        parse::message_type_to_level(Some("Fault")),
        LogLevel::Critical
    ));
    assert!(matches!(
        parse::message_type_to_level(Some("Error")),
        LogLevel::Error
    ));
    assert!(matches!(
        parse::message_type_to_level(Some("Default")),
        LogLevel::Notice
    ));
    assert!(matches!(
        parse::message_type_to_level(Some("Info")),
        LogLevel::Info
    ));
    assert!(matches!(
        parse::message_type_to_level(Some("Debug")),
        LogLevel::Debug
    ));
}

#[test]
fn test_message_type_missing_or_unknown_is_info() {
    use tell::LogLevel;
    assert!(matches!(parse::message_type_to_level(None), LogLevel::Info));
    assert!(matches!(
        parse::message_type_to_level(Some("Bogus")),
        LogLevel::Info
    ));
}

// --- R3: service name & structured payload ---

#[test]
fn test_service_prefers_subsystem() {
    let entry = parse_entry(ERROR_LINE);
    assert_eq!(parse::service_of(&entry), "com.apple.mdns");
}

#[test]
fn test_service_falls_back_to_process_basename() {
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"","processImagePath":"/usr/libexec/runningboardd","eventMessage":"x","bootUUID":"b","machTimestamp":1,"timestamp":"t"}"#;
    let entry = parse_entry(line);
    assert_eq!(parse::service_of(&entry), "runningboardd");
}

#[test]
fn test_service_unknown_when_no_subsystem_or_path() {
    let line = r#"{"eventType":"logEvent","messageType":"Error","eventMessage":"x"}"#;
    let entry = parse_entry(line);
    assert_eq!(parse::service_of(&entry), "unknown");
}

#[test]
fn test_payload_includes_curated_excludes_internals() {
    let entry = parse_entry(ERROR_LINE);
    let payload = parse::build_payload(&entry).expect("payload");
    let obj = payload.as_object().expect("object");
    // Curated fields present.
    assert_eq!(obj.get("category"), Some(&serde_json::json!("resolver")));
    assert_eq!(obj.get("pid"), Some(&serde_json::json!(488)));
    assert_eq!(
        obj.get("subsystem"),
        Some(&serde_json::json!("com.apple.mdns"))
    );
    assert_eq!(
        obj.get("process"),
        Some(&serde_json::json!("mDNSResponder"))
    );
    // Internals excluded (spec 001 R3 acceptance).
    assert!(!obj.contains_key("threadid"));
    assert!(!obj.contains_key("senderimageuuid"));
    assert!(!obj.contains_key("backtrace"));
    assert!(!obj.contains_key("machtimestamp"));
}

// --- R4: self-feedback prevention ---

#[test]
fn test_self_feedback_by_subsystem_bundle_id() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"rs.tell.witness","eventMessage":"x","bootUUID":"b","machTimestamp":9,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(cap.events().is_empty(), "own subsystem must not ship");
}

#[test]
fn test_self_feedback_by_process_basename() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"","processImagePath":"/usr/local/bin/witness","eventMessage":"x","bootUUID":"b","machTimestamp":9,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(cap.events().is_empty(), "own process must not ship");
}

// --- R5: default predicate ---

#[test]
fn test_effective_predicate_default_and_override() {
    let default = unified::effective_predicate(None);
    assert!(default.contains("messageType == \"error\""));
    assert!(default.contains("messageType == \"fault\""));
    assert!(default.contains("com.apple.TCC"));
    assert!(default.contains("sudo"));
    // Never widens to memory-only levels.
    assert!(!default.contains("--info"));
    assert!(!default.contains("--debug"));

    let custom = unified::effective_predicate(Some("messageType == \"fault\""));
    assert_eq!(custom, "messageType == \"fault\"");
}

// --- R6: backpressure & dedupe ---

#[test]
fn test_channel_full_returns_channel_full() {
    let sink = Sink::full();
    assert_eq!(
        parse::process_entry(ERROR_LINE, &sink, None),
        UnifiedResult::ChannelFull
    );
}

#[test]
fn test_backfill_dedupe_skips_at_or_before_checkpoint() {
    let (cap, sink) = capture();
    let cp = Checkpoint {
        boot_uuid: BOOT.to_string(),
        mach_timestamp: ERROR_MACH, // inclusive boundary
        wall_timestamp: "t".to_string(),
    };
    // mach == checkpoint.mach, same boot → already shipped, skip, don't regress.
    let result = parse::process_entry(ERROR_LINE, &sink, Some(&cp));
    assert_eq!(result, UnifiedResult::Handled(None));
    assert!(cap.events().is_empty());
}

#[test]
fn test_backfill_ships_strictly_after_checkpoint() {
    let (cap, sink) = capture();
    let cp = Checkpoint {
        boot_uuid: BOOT.to_string(),
        mach_timestamp: ERROR_MACH - 1,
        wall_timestamp: "t".to_string(),
    };
    let result = parse::process_entry(ERROR_LINE, &sink, Some(&cp));
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert_eq!(cap.events().len(), 1);
}

#[test]
fn test_backfill_dedupe_disabled_across_boots() {
    let (cap, sink) = capture();
    // Different boot: mach counters reset, so dedupe must NOT apply even though
    // the entry's mach is <= checkpoint mach.
    let cp = Checkpoint {
        boot_uuid: "DIFFERENT-BOOT".to_string(),
        mach_timestamp: ERROR_MACH + 100,
        wall_timestamp: "t".to_string(),
    };
    let result = parse::process_entry(ERROR_LINE, &sink, Some(&cp));
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert_eq!(cap.events().len(), 1, "cross-boot entries ship");
}

#[test]
fn test_reconcile_due_crosses_threshold() {
    let now = Instant::now();
    let threshold = Duration::from_secs(5);
    let stalled = now.checked_sub(Duration::from_secs(6)).expect("instant");
    let recent = now.checked_sub(Duration::from_secs(1)).expect("instant");
    assert!(unified::reconcile_due_for_test(
        Some(stalled),
        now,
        threshold
    ));
    assert!(!unified::reconcile_due_for_test(
        Some(recent),
        now,
        threshold
    ));
    assert!(!unified::reconcile_due_for_test(None, now, threshold));
}

// --- Reader bookkeeping (checkpoint advance / no-regress) ---

#[test]
fn test_state_advances_and_holds_checkpoint() {
    let mut probe = unified::StateProbe::new(None);
    let cp = Checkpoint {
        boot_uuid: BOOT.to_string(),
        mach_timestamp: ERROR_MACH,
        wall_timestamp: "t".to_string(),
    };
    // A shipped entry advances the checkpoint.
    assert!(probe.record_advanced(UnifiedResult::Handled(Some(cp.clone()))));
    assert_eq!(probe.last(), Some(cp.clone()));
    // A dedupe skip (Handled(None)) advances the loop but does not regress.
    assert!(probe.record_advanced(UnifiedResult::Handled(None)));
    assert_eq!(probe.last(), Some(cp.clone()));
    // A parse failure is handled (advanced) without touching the checkpoint.
    assert!(probe.record_advanced(UnifiedResult::ParseFailed));
    assert_eq!(probe.last(), Some(cp));
    // Backpressure does not advance.
    assert!(!probe.record_advanced(UnifiedResult::ChannelFull));
}

// --- fault & default fixtures parse and ship at the mapped level ---

#[test]
fn test_fault_and_default_fixtures_ship() {
    let (cap, sink) = capture();
    assert!(matches!(
        parse::process_entry(FAULT_LINE, &sink, None),
        UnifiedResult::Handled(Some(_))
    ));
    assert!(matches!(
        parse::process_entry(DEFAULT_LINE, &sink, None),
        UnifiedResult::Handled(Some(_))
    ));
    let events = cap.events();
    assert_eq!(events.len(), 2);
    // Level mapping is verified separately via message_type_to_level.
    let entry = parse_entry(FAULT_LINE);
    assert!(matches!(
        parse::message_type_to_level(Some("Fault")),
        tell::LogLevel::Critical
    ));
    assert_eq!(parse::service_of(&entry), "com.apple.runningboard");
}

// --- QA: additional R1 parse edge cases (malformed / adversarial NDJSON) ---

#[test]
fn test_trace_event_ships_like_log_event() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"traceEvent","messageType":"Error","subsystem":"com.apple.foo","eventMessage":"trace body","bootUUID":"b","machTimestamp":1,"timestamp":"t","processID":1}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    let events = cap.events();
    assert_eq!(events.len(), 1, "traceEvent must ship, same as logEvent");
    match &events[0] {
        Recorded::Log { message, .. } => assert_eq!(message, "trace body"),
        other => panic!("expected Log, got {other:?}"),
    }
}

#[test]
fn test_missing_event_type_is_structural_noise() {
    let (cap, sink) = capture();
    // No `eventType` key at all: `unwrap_or_default()` yields "", which is
    // neither "logEvent" nor "traceEvent" — handled-and-skipped, checkpoint
    // still advances, exactly like an explicit non-log eventType.
    let line = r#"{"messageType":"Error","eventMessage":"no event type","bootUUID":"b","machTimestamp":1,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(cap.events().is_empty());
}

#[test]
fn test_truncated_json_line_is_parse_failed() {
    let sink = Sink::discard();
    let truncated = &ERROR_LINE[..ERROR_LINE.len() / 2];
    assert_eq!(
        parse::process_entry(truncated, &sink, None),
        UnifiedResult::ParseFailed
    );
}

#[test]
fn test_mach_timestamp_wrong_type_is_parse_failed() {
    let sink = Sink::discard();
    let line = r#"{"eventType":"logEvent","messageType":"Error","eventMessage":"x","bootUUID":"b","machTimestamp":"not-a-number","timestamp":"t"}"#;
    assert_eq!(
        parse::process_entry(line, &sink, None),
        UnifiedResult::ParseFailed
    );
}

#[test]
fn test_boot_uuid_wrong_type_is_parse_failed() {
    let sink = Sink::discard();
    let line = r#"{"eventType":"logEvent","messageType":"Error","eventMessage":"x","bootUUID":12345,"machTimestamp":1,"timestamp":"t"}"#;
    assert_eq!(
        parse::process_entry(line, &sink, None),
        UnifiedResult::ParseFailed
    );
}

#[test]
fn test_missing_mach_timestamp_ships_but_does_not_advance_checkpoint() {
    let (cap, sink) = capture();
    let cp = Checkpoint {
        boot_uuid: BOOT.to_string(),
        mach_timestamp: ERROR_MACH,
        wall_timestamp: "t".to_string(),
    };
    // No `machTimestamp` field: the backfill dedupe check can't apply (it
    // needs a mach position to compare), and the entry itself carries no
    // resume position (`checkpoint()` requires boot+mach+wall together), so
    // it ships but must not advance the checkpoint.
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"com.apple.foo","eventMessage":"no mach","bootUUID":"28179658-9E7F-4BC6-BF41-B01E1AEE4DF1","timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, Some(&cp));
    assert_eq!(result, UnifiedResult::Handled(None));
    assert_eq!(cap.events().len(), 1, "entry still ships");
}

#[test]
fn test_empty_event_message_is_skipped() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","eventMessage":"","bootUUID":"b","machTimestamp":1,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(cap.events().is_empty(), "empty message must not ship");
}

#[test]
fn test_missing_event_message_is_skipped() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","bootUUID":"b","machTimestamp":1,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(
        cap.events().is_empty(),
        "entry with no eventMessage must not ship"
    );
}

// --- QA: R4 self-feedback boundary (pin the intended, if loose, semantics) ---

#[test]
fn test_self_subsystem_prefix_matches_even_without_separator() {
    // Pins the literal `starts_with` semantics from spec 001 R4 ("subsystem
    // begins with the witness bundle identifier"): a subsystem that merely
    // starts with the prefix — with no separating '.' — is still filtered.
    // Per the threat model this is intentional (worst case an attacker only
    // suppresses their own entry), but the loose match deserves an explicit
    // pin so a future tightening is a deliberate change, not a regression.
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"rs.tell.witnessworker","eventMessage":"x","bootUUID":"b","machTimestamp":9,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert!(
        cap.events().is_empty(),
        "a subsystem merely starting with the bundle id is filtered as self"
    );
}

#[test]
fn test_subsystem_short_of_full_prefix_is_not_filtered() {
    let (cap, sink) = capture();
    // One character short of the bundle id prefix: must NOT be treated as self.
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"rs.tell.witnes","eventMessage":"x","bootUUID":"b","machTimestamp":9,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert_eq!(cap.events().len(), 1, "non-prefix subsystem must ship");
}

// --- QA: R3 payload curation edge cases ---

#[test]
fn test_payload_filters_internal_fields_case_insensitively() {
    // threadID/senderImageUUID are not named `LogEntry` fields, so they land
    // in `extras` verbatim-cased; the filter must still catch them regardless
    // of case. `subsystem` keeps the payload non-empty so this isolates the
    // extras-filtering behavior from the "no curated fields → None" case.
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"com.apple.foo","eventMessage":"x","bootUUID":"b","machTimestamp":1,"timestamp":"t","ThreadID":123,"SenderImageUUID":"abc"}"#;
    let entry = parse_entry(line);
    let payload = parse::build_payload(&entry).expect("payload");
    let obj = payload.as_object().expect("object");
    assert!(!obj.contains_key("threadid"));
    assert!(!obj.contains_key("ThreadID"));
    assert!(!obj.contains_key("senderimageuuid"));
    assert!(!obj.contains_key("SenderImageUUID"));
}

#[test]
fn test_payload_forwards_unknown_extras_lowercased() {
    let line = r#"{"eventType":"logEvent","messageType":"Error","eventMessage":"x","bootUUID":"b","machTimestamp":1,"timestamp":"t","CustomField":"abc"}"#;
    let entry = parse_entry(line);
    let payload = parse::build_payload(&entry).expect("payload");
    let obj = payload.as_object().expect("object");
    assert_eq!(obj.get("customfield"), Some(&serde_json::json!("abc")));
}

#[test]
fn test_non_ascii_message_ships_verbatim() {
    let (cap, sink) = capture();
    let line = r#"{"eventType":"logEvent","messageType":"Error","subsystem":"com.apple.foo","eventMessage":"café ☕ 日本語","bootUUID":"b","machTimestamp":1,"timestamp":"t"}"#;
    let result = parse::process_entry(line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    let events = cap.events();
    match &events[0] {
        Recorded::Log { message, .. } => assert_eq!(message, "café ☕ 日本語"),
        other => panic!("expected Log, got {other:?}"),
    }
}

#[test]
fn test_huge_extra_field_is_forwarded_without_panic() {
    let huge = "x".repeat(300_000);
    let line = format!(
        r#"{{"eventType":"logEvent","messageType":"Error","eventMessage":"x","bootUUID":"b","machTimestamp":1,"timestamp":"t","BigField":"{huge}"}}"#
    );
    let (cap, sink) = capture();
    let result = parse::process_entry(&line, &sink, None);
    assert!(matches!(result, UnifiedResult::Handled(Some(_))));
    assert_eq!(cap.events().len(), 1);
    let entry = parse_entry(&line);
    let payload = parse::build_payload(&entry).expect("payload");
    let obj = payload.as_object().expect("object");
    assert_eq!(
        obj.get("bigfield").and_then(|v| v.as_str()).map(str::len),
        Some(300_000)
    );
}

// --- QA: R6 reconcile boundary ---

#[test]
fn test_reconcile_due_at_exact_threshold_is_true() {
    let now = Instant::now();
    let threshold = Duration::from_secs(5);
    let exact = now.checked_sub(threshold).expect("instant");
    assert!(
        unified::reconcile_due_for_test(Some(exact), now, threshold),
        "the reconcile threshold must be inclusive (>=)"
    );
}

// --- QA: Checkpoint serde round-trip & corrupt-data handling ---
//
// `unified::load_checkpoint` is private and reads from a fixed,
// non-injectable path (`crate::config::state_dir()`), so its corrupt-file
// fallback-to-`None` behavior (spec 001 threat model: "parse failures fall
// back to start from now, never crash") cannot be driven end-to-end from a
// test without either a test hook or path injection in production code.
// These tests pin the exact serde mechanism `load_checkpoint` relies on
// (`serde_json::from_slice(..).ok()` turning any deserialize error into
// `None`), which is the load-bearing part.

#[test]
fn test_checkpoint_serde_round_trip() {
    let cp = Checkpoint {
        boot_uuid: BOOT.to_string(),
        mach_timestamp: ERROR_MACH,
        wall_timestamp: "2026-07-07 16:02:38.911816+0200".to_string(),
    };
    let bytes = serde_json::to_vec(&cp).expect("serialize");
    let round_tripped: Checkpoint = serde_json::from_slice(&bytes).expect("deserialize");
    assert_eq!(round_tripped, cp);
}

#[test]
fn test_checkpoint_corrupt_bytes_fail_to_deserialize() {
    // Garbage bytes: the exact input `load_checkpoint`'s `.ok()` converts to `None`.
    assert!(serde_json::from_slice::<Checkpoint>(b"not json at all").is_err());
    assert!(serde_json::from_slice::<Checkpoint>(b"").is_err());
    assert!(serde_json::from_slice::<Checkpoint>(b"{\"boot_uuid\": \"b\"}").is_err());
}

#[test]
fn test_checkpoint_wrong_shape_fails_to_deserialize() {
    // A JSON array (valid JSON, wrong shape) must not deserialize into Checkpoint.
    assert!(serde_json::from_slice::<Checkpoint>(b"[1,2,3]").is_err());
    // mach_timestamp as a string instead of u64.
    let bad = br#"{"boot_uuid":"b","mach_timestamp":"1","wall_timestamp":"t"}"#;
    assert!(serde_json::from_slice::<Checkpoint>(bad).is_err());
}

#[test]
fn test_checkpoint_path_is_deterministic_filename_under_state_dir() {
    let path = unified::checkpoint_path();
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("unified_log_checkpoint")
    );
    assert_eq!(
        path.parent(),
        Some(std::path::Path::new(crate::config::state_dir()))
    );
}
