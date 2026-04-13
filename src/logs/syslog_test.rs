use super::syslog;

// --- Core formats ---

#[test]
fn test_parse_rfc3164() {
    let line = "Apr 12 23:50:00 api sshd[2336811]: Connection reset by authenticating user root 80.66.66.70 port 4578 [preauth]";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "sshd");
    assert_eq!(
        p.body,
        "Connection reset by authenticating user root 80.66.66.70 port 4578 [preauth]"
    );
}

#[test]
fn test_parse_iso8601() {
    let line = "2026-04-12T23:50:00.860800+00:00 api sshd[2336811]: Connection reset by authenticating user root";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "sshd");
    assert_eq!(p.body, "Connection reset by authenticating user root");
}

#[test]
fn test_parse_kernel_no_pid() {
    let line = "Apr 12 23:50:00 api kernel: [UFW BLOCK] IN=eth0 OUT=";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "kernel");
    assert_eq!(p.body, "[UFW BLOCK] IN=eth0 OUT=");
}

#[test]
fn test_parse_cron() {
    let line = "Apr 12 23:50:00 api CRON[12345]: (root) CMD (command -v debian-sa1 > /dev/null)";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "CRON");
    assert_eq!(p.body, "(root) CMD (command -v debian-sa1 > /dev/null)");
}

#[test]
fn test_parse_systemd() {
    let line = "Apr 13 08:50:17 api systemd[1]: Started systemd-collect.service - systemd activity accounting tool";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "systemd");
    assert_eq!(
        p.body,
        "Started systemd-collect.service - systemd activity accounting tool"
    );
}

#[test]
fn test_parse_fail2ban() {
    let line = "2026-04-12T23:44:39.403249+00:00 api fail2ban-rq[497824]: some message here";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "fail2ban-rq");
    assert_eq!(p.body, "some message here");
}

// --- Rejection cases ---

#[test]
fn test_parse_no_separator() {
    assert!(syslog::parse("just a plain line with no colon-space pattern").is_none());
}

#[test]
fn test_parse_no_space_before_tag() {
    // No whitespace before the tag — rfind(' ') returns None
    assert!(syslog::parse("nospace: data").is_none());
}

#[test]
fn test_parse_empty_program_bracket_only() {
    // Tag is just "[123]" — program after stripping brackets is empty
    assert!(syslog::parse("Apr 12 23:50:00 host [123]: msg").is_none());
}

#[test]
fn test_parse_rejects_nginx_error_format() {
    // nginx error: "1234#0" is not a valid program name (starts with digit, contains #)
    let line = "2024/01/01 12:00:00 [error] 1234#0: *1 connect() failed (111: Connection refused)";
    assert!(syslog::parse(line).is_none());
}

#[test]
fn test_parse_rejects_numeric_tag() {
    // Pure numeric "program" should be rejected
    let line = "some prefix 42: value here";
    assert!(syslog::parse(line).is_none());
}

// --- Edge cases ---

#[test]
fn test_parse_multiple_colon_space() {
    // First `: ` delimits tag from body; subsequent are part of the body
    let line = "Apr 12 08:31:21 api sudo: pam_unix(sudo:session): session opened for user root";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "sudo");
    assert_eq!(
        p.body,
        "pam_unix(sudo:session): session opened for user root"
    );
}

#[test]
fn test_parse_sudo_command() {
    let line = "Apr 12 08:31:21 api sudo: user1 : TTY=pts/0 ; PWD=/home ; COMMAND=/bin/ls";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "sudo");
    assert_eq!(p.body, "user1 : TTY=pts/0 ; PWD=/home ; COMMAND=/bin/ls");
}

#[test]
fn test_parse_dhclient() {
    let line = "Apr 13 00:00:01 server dhclient[876]: DHCPREQUEST on eth0 to 10.0.0.1 port 67";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "dhclient");
    assert_eq!(p.body, "DHCPREQUEST on eth0 to 10.0.0.1 port 67");
}

#[test]
fn test_parse_empty_body() {
    // Valid syslog with empty body after separator
    let line = "Apr 12 23:50:00 host sshd[1]: ";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "sshd");
    assert_eq!(p.body, "");
}

#[test]
fn test_parse_colon_space_only() {
    // Degenerate: just ": " — no space before it, rfind returns None
    assert!(syslog::parse(": ").is_none());
}

#[test]
fn test_parse_program_with_hyphens() {
    let line =
        "Apr 12 23:50:00 host dbus-daemon[456]: activating service name='org.freedesktop.nm'";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "dbus-daemon");
    assert_eq!(p.body, "activating service name='org.freedesktop.nm'");
}

#[test]
fn test_parse_program_with_dot() {
    let line = "Apr 12 23:50:00 host ntpd.util[789]: adjusting clock";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "ntpd.util");
    assert_eq!(p.body, "adjusting clock");
}

#[test]
fn test_parse_rsyslog_tag() {
    let line = "Apr 13 00:00:01 host rsyslogd: [origin software=\"rsyslogd\"] start";
    let p = syslog::parse(line).unwrap();
    assert_eq!(p.program, "rsyslogd");
    assert_eq!(p.body, "[origin software=\"rsyslogd\"] start");
}
