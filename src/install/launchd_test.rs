use std::path::Path;

use super::launchd::{self, DOMAIN, LABEL};
use super::platform::{Call, InstallError, MockPlatform};
use super::{InstallArgs, UninstallArgs};

const PLIST: &str = "/Library/LaunchDaemons/rs.tell.witness.plist";

fn install_args() -> InstallArgs {
    InstallArgs {
        token: None,
        server: "https://tell.rs".to_string(),
        endpoint: None,
        offline: false,
        force: false,
    }
}

// --- R2/R3: launchctl sequences ---

#[test]
fn load_daemon_bootstraps_then_enables_in_order() {
    let mock = MockPlatform::new();
    launchd::load_daemon(&mock, Path::new(PLIST)).expect("load ok");
    let calls = mock.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        Call {
            verb: "bootstrap".into(),
            domain: DOMAIN.into(),
            target: PLIST.into(),
        }
    );
    assert_eq!(
        calls[1],
        Call {
            verb: "enable".into(),
            domain: DOMAIN.into(),
            target: LABEL.into(),
        }
    );
}

#[test]
fn load_daemon_stops_at_bootstrap_failure() {
    let mock = MockPlatform::new().failing("bootstrap");
    let err = launchd::load_daemon(&mock, Path::new(PLIST)).expect_err("should fail");
    assert!(matches!(err, InstallError::CommandFailed { .. }));
    // enable must NOT be issued after a bootstrap failure.
    assert_eq!(mock.calls().len(), 1);
}

#[test]
fn load_daemon_stops_at_enable_failure_after_bootstrap_ran() {
    let mock = MockPlatform::new().failing("enable");
    let err = launchd::load_daemon(&mock, Path::new(PLIST)).expect_err("should fail");
    assert!(matches!(err, InstallError::CommandFailed { .. }));
    // bootstrap already ran (and is not retried); enable was attempted once
    // and failed — this is the partial-install state a real failure leaves.
    let calls = mock.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].verb, "bootstrap");
    assert_eq!(calls[1].verb, "enable");
}

#[test]
fn unload_daemon_issues_bootout() {
    let mock = MockPlatform::new();
    launchd::unload_daemon(&mock, PLIST).expect("bootout ok");
    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].verb, "bootout");
    assert_eq!(calls[0].target, PLIST);
}

// --- R3: install root check (no partial writes / no launchctl before it) ---

#[test]
fn install_as_non_root_returns_not_root_with_no_calls() {
    if unsafe { libc::geteuid() } == 0 {
        return; // running as root: the root check is a no-op, nothing to assert
    }
    let mock = MockPlatform::new();
    let err = launchd::install(&install_args(), &mock).expect_err("non-root fails");
    assert!(matches!(err, InstallError::NotRoot));
    assert!(
        mock.calls().is_empty(),
        "no launchctl calls before the root check"
    );
}

// --- R3: status summary ---

#[test]
fn status_report_loaded_running_last_exit() {
    let output = "\
com.apple.xpc.launchd.domain.system = {
    state = running
    last exit code = 0
}
";
    let mock = MockPlatform::new().loaded(true).print_output(output);
    let report = launchd::status_report(&mock);
    assert!(report.loaded);
    assert!(report.running);
    assert_eq!(report.last_exit.as_deref(), Some("0"));
}

#[test]
fn status_report_not_loaded_skips_print() {
    let mock = MockPlatform::new().loaded(false);
    let report = launchd::status_report(&mock);
    assert!(!report.loaded);
    assert!(!report.running);
    assert!(report.last_exit.is_none());
    assert!(
        !mock.calls().iter().any(|c| c.verb == "print"),
        "print must not run when the daemon is not loaded"
    );
}

#[test]
fn status_report_print_failure_defaults_to_not_running() {
    // Loaded, but `launchctl print` itself fails (e.g. a teardown race).
    // status_report must not panic and must report a safe default instead of
    // propagating the print error.
    let mock = MockPlatform::new().loaded(true).failing("print");
    let report = launchd::status_report(&mock);
    assert!(report.loaded);
    assert!(!report.running);
    assert!(report.last_exit.is_none());
}

#[test]
fn status_report_not_running_state_is_parsed() {
    let output = "\
com.apple.xpc.launchd.domain.system = {
    state = not running
}
";
    let mock = MockPlatform::new().loaded(true).print_output(output);
    let report = launchd::status_report(&mock);
    assert!(report.loaded);
    assert!(!report.running);
    assert!(report.last_exit.is_none());
}

#[test]
fn status_report_missing_last_exit_line_is_none() {
    let output = "\
com.apple.xpc.launchd.domain.system = {
    state = running
}
";
    let mock = MockPlatform::new().loaded(true).print_output(output);
    let report = launchd::status_report(&mock);
    assert!(report.running);
    assert!(
        report.last_exit.is_none(),
        "no 'last exit code' line in output must not panic or fabricate a value"
    );
}

// --- R1: plist content + plutil lint ---

#[test]
fn plist_has_required_keys() {
    let plist = launchd::plist_contents("/usr/local/bin/witness", "/etc/witness/config.toml");
    assert!(plist.contains("<string>rs.tell.witness</string>"));
    assert!(plist.contains("<string>/usr/local/bin/witness</string>"));
    assert!(plist.contains("<string>--config</string>"));
    assert!(plist.contains("<string>/etc/witness/config.toml</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<key>StandardOutPath</key>"));
    assert!(plist.contains("<key>StandardErrorPath</key>"));
    assert!(plist.contains("/Library/Logs/witness/witness.out.log"));
    assert!(plist.contains("/Library/Logs/witness/witness.err.log"));
    assert!(plist.contains("<key>ProcessType</key>"));
}

#[test]
fn plist_passes_plutil_lint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rs.tell.witness.plist");
    std::fs::write(
        &path,
        launchd::plist_contents("/usr/local/bin/witness", "/etc/witness/config.toml"),
    )
    .expect("write plist");

    let output = std::process::Command::new("plutil")
        .arg("-lint")
        .arg(&path)
        .output()
        .expect("run plutil");
    assert!(
        output.status.success(),
        "plutil -lint failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

// UninstallArgs is exercised by the module; touch it so the surface stays live.
#[test]
fn uninstall_args_purge_flag() {
    let args = UninstallArgs { purge: true };
    assert!(args.purge);
}

#[test]
fn test_plist_contents_escapes_xml_special_chars_in_paths() {
    let plist = launchd::plist_contents("/opt/wit&ness/bin<'w'>", "/etc/\"witness\"/config.toml");
    assert!(plist.contains("/opt/wit&amp;ness/bin&lt;&apos;w&apos;&gt;"));
    assert!(plist.contains("/etc/&quot;witness&quot;/config.toml"));
    assert!(!plist.contains("wit&ness"));

    // Still lint-clean XML after escaping.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("escape.plist");
    std::fs::write(&path, &plist).unwrap();
    let out = std::process::Command::new("plutil")
        .arg("-lint")
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "plutil rejected escaped plist");
}
