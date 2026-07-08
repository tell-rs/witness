use super::structured;
use super::structured::FileParseOpts;
use tell::LogLevel;

// --- opts helpers ---

const ALL: FileParseOpts = FileParseOpts {
    syslog: true,
    structured: true,
    levels: true,
};

/// Structured + level detection, no syslog envelope — isolates the JSON/logfmt
/// field path from syslog parsing.
const STRUCT: FileParseOpts = FileParseOpts {
    syslog: false,
    structured: true,
    levels: true,
};

fn classify(line: &str, opts: FileParseOpts) -> (LogLevel, String, Option<String>, Option<String>) {
    let c = structured::classify_line(line, opts);
    let payload = c.payload.map(|v| v.to_string());
    (
        c.level,
        c.body.into_owned(),
        c.service.map(str::to_string),
        payload,
    )
}

// ─── scan_line_level: real logger fixtures ──────────────────────────────────

#[test]
fn test_scan_nginx_error_line() {
    // Real nginx error log line — level in `[error]` brackets.
    let line = "2026/07/07 12:00:00 [error] 31#31: *1 open() \"/x\" failed (2: No such \
                file or directory), client: 1.2.3.4";
    assert!(matches!(
        structured::scan_line_level(line),
        Some(LogLevel::Error)
    ));
}

#[test]
fn test_scan_nginx_warn_line() {
    let line = "2026/07/07 12:00:00 [warn] 31#31: *1 upstream server temporarily disabled";
    assert!(matches!(
        structured::scan_line_level(line),
        Some(LogLevel::Warning)
    ));
}

#[test]
fn test_scan_java_log4j_error() {
    let line = "2026-07-07 12:00:00 ERROR com.foo.Bar - could not connect to database";
    assert!(matches!(
        structured::scan_line_level(line),
        Some(LogLevel::Error)
    ));
}

#[test]
fn test_scan_env_logger_error() {
    // env_logger default: [timestamp LEVEL module] message
    let line = "[2026-07-07T12:00:00Z ERROR my_crate::db] connection refused";
    assert!(matches!(
        structured::scan_line_level(line),
        Some(LogLevel::Error)
    ));
}

#[test]
fn test_scan_env_logger_debug() {
    let line = "[2026-07-07T12:00:00Z DEBUG my_crate] entering handler";
    assert!(matches!(
        structured::scan_line_level(line),
        Some(LogLevel::Debug)
    ));
}

#[test]
fn test_scan_level_equals_style() {
    assert!(matches!(
        structured::scan_line_level("ts=2026 level=error msg=boom"),
        Some(LogLevel::Error)
    ));
}

#[test]
fn test_scan_all_level_tokens() {
    let cases = [
        ("x TRACE y", LogLevel::Debug),
        ("x trace y", LogLevel::Debug),
        ("x DEBUG y", LogLevel::Debug),
        ("x INFO y", LogLevel::Info),
        ("x NOTICE y", LogLevel::Notice),
        ("x WARN y", LogLevel::Warning),
        ("x WARNING y", LogLevel::Warning),
        ("x warn y", LogLevel::Warning),
        ("x ERR y", LogLevel::Error),
        ("x ERROR y", LogLevel::Error),
        ("x CRIT y", LogLevel::Critical),
        ("x CRITICAL y", LogLevel::Critical),
        ("x FATAL y", LogLevel::Critical),
        ("x PANIC y", LogLevel::Critical),
        ("x panic y", LogLevel::Critical),
    ];
    for (line, want) in cases {
        let got = structured::scan_line_level(line);
        assert_eq!(
            std::mem::discriminant(&got.unwrap()),
            std::mem::discriminant(&want),
            "line {line:?}"
        );
    }
}

#[test]
fn test_scan_first_token_wins() {
    // INFO appears before error — the info classification wins.
    assert!(matches!(
        structured::scan_line_level("INFO installing error handler"),
        Some(LogLevel::Info)
    ));
}

// ─── scan_line_level: false-positive guards ─────────────────────────────────

#[test]
fn test_scan_no_false_positive_terror() {
    // "Terror" contains "error" but is a single word — must NOT match.
    assert!(structured::scan_line_level("Terror alert raised at the border").is_none());
}

#[test]
fn test_scan_no_false_positive_mid_word() {
    // "errors" — the trailing 's' makes it a different word.
    assert!(structured::scan_line_level("there were errors during the run").is_none());
    assert!(structured::scan_line_level("the software warnings list is empty").is_none());
    assert!(structured::scan_line_level("informative note about the system").is_none());
}

#[test]
fn test_scan_no_false_positive_title_case() {
    // Case-sensitive: title-case prose is not a level token.
    assert!(structured::scan_line_level("Error occurred while parsing").is_none());
    assert!(structured::scan_line_level("Warning signs were ignored").is_none());
}

#[test]
fn test_scan_plain_line_none() {
    assert!(structured::scan_line_level("connection accepted from remote host").is_none());
    assert!(structured::scan_line_level("").is_none());
}

#[test]
fn test_scan_window_bounded() {
    // A level token past the 96-byte window is not seen.
    let line = format!("{} ERROR trailing", "x".repeat(100));
    assert!(structured::scan_line_level(&line).is_none());
}

#[test]
fn test_scan_multibyte_before_window_edge_no_panic() {
    // Multi-byte chars near the cut point must not panic (byte slice, not char).
    let line = format!("{}é ERROR", "ok ".repeat(30));
    let _ = structured::scan_line_level(&line);
}

// ─── classify_line: structured field → severity (consumed) ──────────────────

#[test]
fn test_classify_go_slog_json() {
    // Go slog JSON — level from field, consumed; msg becomes body.
    let line = r#"{"time":"2026-07-07T12:00:00Z","level":"ERROR","msg":"failed to connect","err":"timeout"}"#;
    let (level, body, service, payload) = classify(line, STRUCT);
    assert!(matches!(level, LogLevel::Error));
    assert_eq!(body, "failed to connect");
    assert_eq!(service, None);
    let p = payload.expect("payload");
    assert!(p.contains("timeout"));
    assert!(p.contains("2026-07-07"));
    assert!(!p.contains("ERROR"), "level field must be consumed: {p}");
    assert!(!p.contains("msg"));
}

#[test]
fn test_classify_json_numeric_level() {
    let line = r#"{"level":3,"msg":"boom"}"#;
    let (level, body, _, payload) = classify(line, STRUCT);
    assert!(matches!(level, LogLevel::Error)); // syslog 3 = err
    assert_eq!(body, "boom");
    assert!(payload.is_none(), "only level+msg present, both consumed");
}

#[test]
fn test_classify_logfmt_level_warn() {
    let line = "user login failed user=bob ip=1.2.3.4 level=warn";
    let (level, body, _, payload) = classify(line, STRUCT);
    assert!(matches!(level, LogLevel::Warning));
    assert_eq!(body, "user login failed");
    let p = payload.expect("payload");
    assert!(p.contains("bob"));
    assert!(!p.contains("warn"), "level consumed: {p}");
}

#[test]
fn test_classify_logfmt_numeric_level_string() {
    let line = "evt happened level=3 code=x";
    let (level, _, _, _) = classify(line, STRUCT);
    assert!(matches!(level, LogLevel::Error));
}

#[test]
fn test_classify_severity_and_lvl_keys() {
    for key in ["severity", "lvl", "SEVERITY", "Level"] {
        let line = format!(r#"{{"{key}":"critical","msg":"down"}}"#);
        let (level, body, _, payload) = classify(&line, STRUCT);
        assert!(matches!(level, LogLevel::Critical), "key {key}");
        assert_eq!(body, "down");
        assert!(payload.is_none(), "key {key} consumed");
    }
}

#[test]
fn test_classify_unknown_level_value_kept() {
    // A level field we can't map is left in the payload; level stays Info.
    let line = r#"{"level":"banana","msg":"hello"}"#;
    let (level, body, _, payload) = classify(line, STRUCT);
    assert!(matches!(level, LogLevel::Info));
    assert_eq!(body, "hello");
    assert!(payload.expect("payload").contains("banana"));
}

// ─── classify_line: heuristic fallback when no structured level ─────────────

#[test]
fn test_classify_nginx_plain_line_heuristic() {
    let line = "2026/07/07 12:00:00 [error] 31#31: *1 open() failed, client: 1.2.3.4";
    let (level, body, service, payload) = classify(line, ALL);
    assert!(matches!(level, LogLevel::Error));
    assert_eq!(body, line);
    assert_eq!(service, None);
    assert!(payload.is_none());
}

#[test]
fn test_classify_plain_line_stays_info() {
    let line = "just a normal informative-sounding message with no level token";
    let (level, body, _, payload) = classify(line, ALL);
    assert!(matches!(level, LogLevel::Info));
    assert_eq!(body, line);
    assert!(payload.is_none());
}

// ─── classify_line: syslog envelope THEN structured/heuristic on the body ───

#[test]
fn test_classify_syslog_then_heuristic() {
    let line = "Apr 12 23:50:00 host nginx[1]: [error] upstream timed out";
    let (level, body, service, _) = classify(line, ALL);
    assert!(matches!(level, LogLevel::Error));
    assert_eq!(body, "[error] upstream timed out");
    assert_eq!(service.as_deref(), Some("nginx"));
}

#[test]
fn test_classify_syslog_then_structured() {
    let line = "Apr 12 23:50:00 host myapp[9]: request done status=200 dur=5 level=error";
    let (level, body, service, payload) = classify(line, ALL);
    assert!(matches!(level, LogLevel::Error));
    assert_eq!(body, "request done");
    assert_eq!(service.as_deref(), Some("myapp"));
    let p = payload.expect("payload");
    assert!(p.contains("200"));
    assert!(!p.contains("level"), "level consumed: {p}");
}

// ─── classify_line: knob toggles ────────────────────────────────────────────

#[test]
fn test_classify_structured_off_ships_whole_body() {
    let opts = FileParseOpts {
        syslog: false,
        structured: false,
        levels: false,
    };
    let line = r#"{"level":"error","msg":"boom","k":"v"}"#;
    let (level, body, _, payload) = classify(line, opts);
    assert!(matches!(level, LogLevel::Info));
    assert_eq!(
        body, line,
        "no structured extraction — whole line is the body"
    );
    assert!(payload.is_none());
}

#[test]
fn test_classify_levels_off_keeps_level_field() {
    // detect_levels off: the level field is NOT consumed and stays Info.
    let opts = FileParseOpts {
        syslog: false,
        structured: true,
        levels: false,
    };
    let line = r#"{"level":"error","msg":"boom","k":"v"}"#;
    let (level, body, _, payload) = classify(line, opts);
    assert!(matches!(level, LogLevel::Info));
    assert_eq!(body, "boom");
    let p = payload.expect("payload");
    assert!(p.contains("error"), "level field retained: {p}");
    assert!(p.contains("\"k\""));
}

#[test]
fn test_classify_all_off_is_plain_info() {
    let opts = FileParseOpts {
        syslog: false,
        structured: false,
        levels: false,
    };
    let line = "Apr 12 23:50:00 host sshd[1]: ERROR bad thing";
    let (level, body, service, payload) = classify(line, opts);
    assert!(matches!(level, LogLevel::Info));
    assert_eq!(body, line);
    assert_eq!(service, None);
    assert!(payload.is_none());
}

// ─── split_message re-export sanity (moved from journal.rs) ─────────────────

#[test]
fn test_split_message_still_parses_json() {
    let (body, fields) = structured::split_message(r#"{"msg":"banned","ip":"1.2.3.4"}"#);
    assert_eq!(body, "banned");
    assert_eq!(fields.expect("fields")["ip"], "1.2.3.4");
}

#[test]
fn test_split_message_still_parses_logfmt() {
    let (body, fields) = structured::split_message("banned ip=1.2.3.4 jail=sshd");
    assert_eq!(body, "banned");
    let f = fields.expect("fields");
    assert_eq!(f["ip"], "1.2.3.4");
    assert_eq!(f["jail"], "sshd");
}
