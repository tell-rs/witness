use super::platform_windows::{Call, MockPlatform, ServiceState};
use super::windows::{
    SERVICE_NAME, StatusReport, install_sequence, service_spec, status_report, uninstall_sequence,
};
use std::path::PathBuf;

fn test_spec() -> super::platform_windows::ServiceSpec {
    service_spec(PathBuf::from(r"C:\Program Files\witness\witness.exe"))
}

fn verbs(calls: &[Call]) -> Vec<String> {
    calls.iter().map(|c| c.verb.clone()).collect()
}

// ─── install_sequence (spec 005 R3) ──────────────────────────────────

#[test]
fn install_fresh_creates_then_starts() {
    let mock = MockPlatform::new().existing(false);
    let created = install_sequence(&mock, &test_spec(), false).expect("ok");
    assert!(created);
    assert_eq!(verbs(&mock.calls()), vec!["exists", "create", "start"]);
}

#[test]
fn install_existing_without_force_is_noop() {
    let mock = MockPlatform::new().existing(true);
    let created = install_sequence(&mock, &test_spec(), false).expect("ok");
    assert!(!created, "existing service left untouched without --force");
    // Only the existence check ran — no create/start (no clobber).
    assert_eq!(verbs(&mock.calls()), vec!["exists"]);
}

#[test]
fn install_existing_with_force_recreates() {
    let mock = MockPlatform::new().existing(true);
    let created = install_sequence(&mock, &test_spec(), true).expect("ok");
    assert!(created);
    assert_eq!(
        verbs(&mock.calls()),
        vec!["exists", "delete", "create", "start"]
    );
}

#[test]
fn install_create_failure_propagates() {
    let mock = MockPlatform::new().existing(false).failing("create");
    let err = install_sequence(&mock, &test_spec(), false);
    assert!(err.is_err());
    // start must not run after a failed create.
    assert_eq!(verbs(&mock.calls()), vec!["exists", "create"]);
}

#[test]
fn install_start_failure_propagates() {
    let mock = MockPlatform::new().existing(false).failing("start");
    assert!(install_sequence(&mock, &test_spec(), false).is_err());
    assert_eq!(verbs(&mock.calls()), vec!["exists", "create", "start"]);
}

// ─── uninstall_sequence (spec 005 R3) ────────────────────────────────

#[test]
fn uninstall_running_stops_then_deletes() {
    let mock = MockPlatform::new().state(ServiceState::Running);
    uninstall_sequence(&mock, SERVICE_NAME).expect("ok");
    assert_eq!(verbs(&mock.calls()), vec!["query_state", "stop", "delete"]);
}

#[test]
fn uninstall_stopped_deletes_only() {
    let mock = MockPlatform::new().state(ServiceState::Stopped);
    uninstall_sequence(&mock, SERVICE_NAME).expect("ok");
    assert_eq!(verbs(&mock.calls()), vec!["query_state", "delete"]);
}

#[test]
fn uninstall_delete_failure_propagates() {
    let mock = MockPlatform::new()
        .state(ServiceState::Stopped)
        .failing("delete");
    assert!(uninstall_sequence(&mock, SERVICE_NAME).is_err());
}

// ─── status_report (spec 005 R3) ─────────────────────────────────────

#[test]
fn status_not_installed() {
    let mock = MockPlatform::new().state(ServiceState::NotInstalled);
    assert_eq!(
        status_report(&mock, SERVICE_NAME),
        StatusReport {
            loaded: false,
            running: false
        }
    );
}

#[test]
fn status_running() {
    let mock = MockPlatform::new().state(ServiceState::Running);
    assert_eq!(
        status_report(&mock, SERVICE_NAME),
        StatusReport {
            loaded: true,
            running: true
        }
    );
}

#[test]
fn status_stopped_is_loaded_not_running() {
    let mock = MockPlatform::new().state(ServiceState::Stopped);
    assert_eq!(
        status_report(&mock, SERVICE_NAME),
        StatusReport {
            loaded: true,
            running: false
        }
    );
}

#[test]
fn service_spec_uses_fixed_paths() {
    let s = test_spec();
    assert_eq!(s.name, "witness");
    assert_eq!(
        s.config_path,
        PathBuf::from(r"C:\ProgramData\witness\config.toml")
    );
}
