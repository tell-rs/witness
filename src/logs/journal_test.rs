use super::journal;
use crate::sink::{DryRun, Sink};

// --- process_entry ---

#[test]
fn test_process_entry_valid() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"Connection accepted","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_missing_message() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0); // Not shipped — no message
}

#[test]
fn test_process_entry_empty_message() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"","SYSLOG_IDENTIFIER":"sshd","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0);
}

#[test]
fn test_process_entry_filters_witness() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"journal watcher starting","SYSLOG_IDENTIFIER":"witness","PRIORITY":"6","__CURSOR":"s=abc"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=abc".to_string())));
    assert_eq!(dr.count(), 0); // Filtered — not shipped
}

#[test]
fn test_process_entry_malformed_json() {
    let sink = Sink::discard();
    assert_eq!(journal::process_entry("not json at all", &sink), None);
}

#[test]
fn test_process_entry_empty_json() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // Empty object — no MESSAGE, no cursor
    assert_eq!(journal::process_entry("{}", &sink), Some(None));
    assert_eq!(dr.count(), 0);
}

#[test]
fn test_process_entry_no_cursor() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = r#"{"MESSAGE":"hello","SYSLOG_IDENTIFIER":"sshd","PRIORITY":"6"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(None)); // Parsed but no cursor
    assert_eq!(dr.count(), 1); // Still shipped
}

#[test]
fn test_process_entry_comm_fallback() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // No SYSLOG_IDENTIFIER — falls back to _COMM
    let json = r#"{"MESSAGE":"hello","_COMM":"myapp","PRIORITY":"3","__CURSOR":"s=x"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=x".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_default_priority() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // No PRIORITY — defaults to Info
    let json = r#"{"MESSAGE":"hello","SYSLOG_IDENTIFIER":"sshd","__CURSOR":"s=y"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=y".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_ignores_unknown_fields() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    // Extra fields like _SYSTEMD_UNIT are silently ignored
    let json = r#"{"MESSAGE":"started","SYSLOG_IDENTIFIER":"nginx","_SYSTEMD_UNIT":"nginx.service","_PID":"123","__CURSOR":"s=z"}"#;
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=z".to_string())));
    assert_eq!(dr.count(), 1);
}

#[test]
fn test_process_entry_whitespace_trimmed() {
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), Default::default());
    let json = "  {\"MESSAGE\":\"hello\",\"SYSLOG_IDENTIFIER\":\"sshd\",\"__CURSOR\":\"s=t\"}  \n";
    let result = journal::process_entry(json, &sink);
    assert_eq!(result, Some(Some("s=t".to_string())));
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
