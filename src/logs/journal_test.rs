use super::journal;
use super::journal::{ProcessResult, ServiceFilter};
use crate::sink::{DryRun, Sink};

/// No-op service filter for tests that don't exercise service filtering.
fn nofilter() -> ServiceFilter {
    ServiceFilter::default()
}

// --- process_entry ---

#[test]
fn test_process_entry_valid() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"Connection accepted","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_missing_message() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0); // Not shipped — no message
}

#[test]
fn test_process_entry_empty_message() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"","SYSLOG_IDENTIFIER":"sshd","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0);
}

#[test]
fn test_process_entry_filters_witness() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"journal watcher starting","SYSLOG_IDENTIFIER":"witness","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0); // Filtered — not shipped
}

#[test]
fn test_process_entry_malformed_json() {
    let sink = Sink::discard();
    assert_eq!(
        journal::process_entry("not json at all", &nofilter(), &sink),
        ProcessResult::ParseFailed
    );
}

#[test]
fn test_process_entry_empty_json() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // Empty object — no MESSAGE, no cursor
    assert_eq!(
        journal::process_entry("{}", &nofilter(), &sink),
        ProcessResult::Handled(None)
    );
    assert_eq!(dr.count(), 0);
}

#[test]
fn test_process_entry_no_cursor() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"hello","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(None)); // Parsed but no cursor
    assert_eq!(dr.count(), 1); // Still shipped
}

#[test]
fn test_process_entry_comm_fallback() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // No SYSLOG_IDENTIFIER — falls back to _COMM
    let json = r#"{"MESSAGE":"hello","_COMM":"myapp","PRIORITY":"3","__CURSOR":"s=x"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=x".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_default_priority() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // No PRIORITY — defaults to Info
    let json = r#"{"MESSAGE":"hello","SYSLOG_IDENTIFIER":"sshd","__CURSOR":"s=y"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=y".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_ignores_unknown_fields() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // systemd-trusted fields (_*) land in extras but are filtered
    // out of the outgoing payload by app_fields_payload.
    let json = r#"{"MESSAGE":"started","SYSLOG_IDENTIFIER":"nginx","_SYSTEMD_UNIT":"nginx.service","_PID":"123","__CURSOR":"s=z"}"#;
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=z".to_string())));
    assert_eq!(dr.count(), 1);
}

// --- app_fields_payload ---

#[test]
fn test_app_fields_empty_returns_none() {
    let extras = std::collections::HashMap::new();
    assert!(journal::app_fields_payload(extras).is_none());
}

#[test]
fn test_app_fields_filters_systemd_trusted() {
    // Only _*-prefixed fields present — all should be filtered.
    let extras: std::collections::HashMap<String, serde_json::Value> = [
        ("_PID".to_string(), serde_json::json!("123")),
        (
            "_SYSTEMD_UNIT".to_string(),
            serde_json::json!("foo.service"),
        ),
        ("__REALTIME_TIMESTAMP".to_string(), serde_json::json!("1")),
    ]
    .into_iter()
    .collect();
    assert!(journal::app_fields_payload(extras).is_none());
}

#[test]
fn test_app_fields_lowercases_app_keys() {
    let extras: std::collections::HashMap<String, serde_json::Value> = [
        ("IP".to_string(), serde_json::json!("1.2.3.4")),
        ("JAIL".to_string(), serde_json::json!("sshd")),
        ("BAN_TIME".to_string(), serde_json::json!("3600")),
        ("_PID".to_string(), serde_json::json!("42")), // should drop
    ]
    .into_iter()
    .collect();

    let payload = journal::app_fields_payload(extras).expect("has app fields");
    let obj = payload.as_object().expect("object");

    assert_eq!(obj.len(), 3);
    assert_eq!(obj.get("ip"), Some(&serde_json::json!("1.2.3.4")));
    assert_eq!(obj.get("jail"), Some(&serde_json::json!("sshd")));
    assert_eq!(obj.get("ban_time"), Some(&serde_json::json!("3600")));
    assert!(!obj.contains_key("IP"));
    assert!(!obj.contains_key("_pid"));
    assert!(!obj.contains_key("_PID"));
}

#[test]
fn test_app_fields_filters_systemd_metadata_denylist() {
    // SYSLOG_FACILITY/PID/RAW are systemd-emitted and carry no structured
    // value beyond what PRIORITY/service already cover. Stay out of Tell.
    let extras: std::collections::HashMap<String, serde_json::Value> = [
        ("IP".to_string(), serde_json::json!("1.2.3.4")),
        ("SYSLOG_FACILITY".to_string(), serde_json::json!("3")),
        ("SYSLOG_PID".to_string(), serde_json::json!("12345")),
        ("SYSLOG_RAW".to_string(), serde_json::json!("<5>raw line")),
        // These are intentionally kept.
        (
            "MESSAGE_ID".to_string(),
            serde_json::json!("8d45620c1a4348dbb17410da57c60c66"),
        ),
        ("CODE_FILE".to_string(), serde_json::json!("src/main.rs")),
    ]
    .into_iter()
    .collect();

    let payload = journal::app_fields_payload(extras).expect("has app fields");
    let obj = payload.as_object().expect("object");

    assert!(obj.contains_key("ip"));
    assert!(obj.contains_key("message_id"));
    assert!(obj.contains_key("code_file"));
    assert!(!obj.contains_key("syslog_facility"));
    assert!(!obj.contains_key("syslog_pid"));
    assert!(!obj.contains_key("syslog_raw"));
}

// --- split_message ---

#[test]
fn test_split_message_json() {
    let msg = r#"{"msg":"banned","ip":"1.2.3.4","jail":"sshd","ban_count":1}"#;
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "banned");
    let obj = fields.expect("fields").as_object().expect("object").clone();
    assert_eq!(obj.get("ip"), Some(&serde_json::json!("1.2.3.4")));
    assert_eq!(obj.get("jail"), Some(&serde_json::json!("sshd")));
    assert_eq!(obj.get("ban_count"), Some(&serde_json::json!(1)));
    assert!(!obj.contains_key("msg"));
}

#[test]
fn test_split_message_logfmt() {
    let msg = "banned ip=1.2.3.4 jail=sshd ban_time=3600 reason=threshold";
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "banned");
    let obj = fields.expect("fields").as_object().expect("object").clone();
    assert_eq!(obj.get("ip"), Some(&serde_json::json!("1.2.3.4")));
    assert_eq!(obj.get("jail"), Some(&serde_json::json!("sshd")));
    assert_eq!(obj.get("ban_time"), Some(&serde_json::json!("3600")));
    assert_eq!(obj.get("reason"), Some(&serde_json::json!("threshold")));
}

#[test]
fn test_split_message_logfmt_multi_word_phrase() {
    let msg = "ban failed ip=1.2.3.4 jail=sshd";
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "ban failed");
    let obj = fields.expect("fields").as_object().expect("object").clone();
    assert_eq!(obj.get("ip"), Some(&serde_json::json!("1.2.3.4")));
    assert_eq!(obj.get("jail"), Some(&serde_json::json!("sshd")));
}

#[test]
fn test_split_message_logfmt_quoted_values() {
    let msg = r#"ban failed ip=1.2.3.4 error="nft command failed""#;
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "ban failed");
    let obj = fields.expect("fields").as_object().expect("object").clone();
    assert_eq!(obj.get("ip"), Some(&serde_json::json!("1.2.3.4")));
    assert_eq!(
        obj.get("error"),
        Some(&serde_json::json!("nft command failed"))
    );
}

#[test]
fn test_split_message_plain_text() {
    // No structured content — return the whole string as body.
    let msg = "connection accepted from remote host";
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "connection accepted from remote host");
    assert!(fields.is_none());
}

#[test]
fn test_split_message_malformed_json() {
    // Starts with `{` but isn't valid JSON — fall back to logfmt detection.
    let msg = "{not valid json";
    let (body, fields) = journal::split_message(msg);
    assert_eq!(body, "{not valid json");
    assert!(fields.is_none());
}

#[test]
fn test_app_fields_preserves_non_string_values() {
    // journalctl -o json sometimes emits arrays (for binary values) —
    // serde_json::Value round-trips whatever shape the source provided.
    let extras: std::collections::HashMap<String, serde_json::Value> = [
        ("COUNT".to_string(), serde_json::json!(42)),
        ("FLAGS".to_string(), serde_json::json!([1, 2, 3])),
    ]
    .into_iter()
    .collect();

    let payload = journal::app_fields_payload(extras).expect("has app fields");
    let obj = payload.as_object().expect("object");
    assert_eq!(obj.get("count"), Some(&serde_json::json!(42)));
    assert_eq!(obj.get("flags"), Some(&serde_json::json!([1, 2, 3])));
}

#[test]
fn test_process_entry_whitespace_trimmed() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = "  {\"MESSAGE\":\"hello\",\"SYSLOG_IDENTIFIER\":\"sshd\",\"__CURSOR\":\"s=t\"}  \n";
    let result = journal::process_entry(json, &nofilter(), &sink);
    assert_eq!(result, ProcessResult::Handled(Some("s=t".to_string())));
    assert_eq!(dr.count(), 1);
}

// --- priority_to_level ---

#[test]
fn test_priority_to_level_all_levels() {
    assert!(matches!(
        journal::priority_to_level("0"),
        Some(tell::LogLevel::Emergency)
    ));
    assert!(matches!(
        journal::priority_to_level("1"),
        Some(tell::LogLevel::Alert)
    ));
    assert!(matches!(
        journal::priority_to_level("2"),
        Some(tell::LogLevel::Critical)
    ));
    assert!(matches!(
        journal::priority_to_level("3"),
        Some(tell::LogLevel::Error)
    ));
    assert!(matches!(
        journal::priority_to_level("4"),
        Some(tell::LogLevel::Warning)
    ));
    assert!(matches!(
        journal::priority_to_level("5"),
        Some(tell::LogLevel::Notice)
    ));
    assert!(matches!(
        journal::priority_to_level("6"),
        Some(tell::LogLevel::Info)
    ));
    assert!(matches!(
        journal::priority_to_level("7"),
        Some(tell::LogLevel::Debug)
    ));
}

#[test]
fn test_priority_to_level_invalid() {
    assert!(journal::priority_to_level("8").is_none());
    assert!(journal::priority_to_level("").is_none());
    assert!(journal::priority_to_level("info").is_none());
    assert!(journal::priority_to_level("-1").is_none());
}

// --- backpressure ---

#[test]
fn test_process_entry_channel_full() {
    let sink = Sink::full();
    let json = r#"{"MESSAGE":"hello","SYSLOG_IDENTIFIER":"sshd","__CURSOR":"s=abc"}"#;
    assert_eq!(
        journal::process_entry(json, &nofilter(), &sink),
        ProcessResult::ChannelFull
    );
}

#[test]
fn test_process_entry_channel_full_filtered_still_handled() {
    // Filtered entries never touch the channel — handled even when full,
    // so the cursor can advance past them.
    let sink = Sink::full();
    let json = r#"{"MESSAGE":"x","SYSLOG_IDENTIFIER":"witness","__CURSOR":"s=abc"}"#;
    assert_eq!(
        journal::process_entry(json, &nofilter(), &sink),
        ProcessResult::Handled(Some("s=abc".to_string()))
    );
}

// --- logfmt UTF-8 ---

#[test]
fn test_split_message_logfmt_quoted_multibyte_value() {
    let (body, fields) = journal::split_message(r#"login failed user="Jürgen Müller" ip=10.0.0.1"#);
    assert_eq!(body, "login failed");
    let f = fields.unwrap();
    assert_eq!(f["user"], "Jürgen Müller");
    assert_eq!(f["ip"], "10.0.0.1");
}

#[test]
fn test_split_message_logfmt_bare_multibyte_value() {
    let (_, fields) = journal::split_message("evt city=Zürich");
    assert_eq!(fields.unwrap()["city"], "Zürich");
}

#[test]
fn test_split_message_logfmt_escape_adjacent_to_multibyte() {
    let (_, fields) = journal::split_message(r#"evt msg="a\"é\"b""#);
    assert_eq!(fields.unwrap()["msg"], "a\"é\"b");
}

// --- ServiceFilter (journald parity) ---

fn sshd_line() -> &'static str {
    r#"{"MESSAGE":"accepted","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6","__CURSOR":"s=1"}"#
}

fn cron_line() -> &'static str {
    r#"{"MESSAGE":"ran job","SYSLOG_IDENTIFIER":"cron","PRIORITY":"6","__CURSOR":"s=2"}"#
}

#[test]
fn test_filter_exclude_drops_but_advances_cursor() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let filter = ServiceFilter {
        include: vec![],
        exclude: vec!["cron".to_string()],
    };
    // Filtered entry is Handled (cursor advances) but not shipped.
    assert_eq!(
        journal::process_entry(cron_line(), &filter, &sink),
        ProcessResult::Handled(Some("s=2".to_string()))
    );
    assert_eq!(dr.count(), 0);
    // A non-excluded service still ships.
    assert_eq!(
        journal::process_entry(sshd_line(), &filter, &sink),
        ProcessResult::Handled(Some("s=1".to_string()))
    );
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_filter_include_allow_list() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let filter = ServiceFilter {
        include: vec!["sshd".to_string()],
        exclude: vec![],
    };
    // In the allow-list → shipped.
    journal::process_entry(sshd_line(), &filter, &sink);
    assert_eq!(dr.count(), 1);
    // Not in the allow-list → dropped, cursor still advances.
    assert_eq!(
        journal::process_entry(cron_line(), &filter, &sink),
        ProcessResult::Handled(Some("s=2".to_string()))
    );
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_filter_empty_include_allows_all() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let filter = ServiceFilter::default();
    journal::process_entry(sshd_line(), &filter, &sink);
    journal::process_entry(cron_line(), &filter, &sink);
    assert_eq!(dr.count(), 2);
}

#[test]
fn test_filter_exclude_wins_over_include() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let filter = ServiceFilter {
        include: vec!["sshd".to_string()],
        exclude: vec!["sshd".to_string()],
    };
    // Present in both lists — exclude wins, dropped.
    journal::process_entry(sshd_line(), &filter, &sink);
    assert_eq!(dr.count(), 0);
}

#[test]
fn test_filter_case_sensitive_exact() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let filter = ServiceFilter {
        include: vec![],
        exclude: vec!["SSHD".to_string()], // wrong case — does not match "sshd"
    };
    journal::process_entry(sshd_line(), &filter, &sink);
    assert_eq!(dr.count(), 1, "exclude is case-sensitive exact");
}
