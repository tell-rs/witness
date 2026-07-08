use super::platform::{Call, InstallError, MockPlatform, Platform};
use std::path::Path;

#[test]
fn mock_records_verb_domain_target() {
    let mock = MockPlatform::new();
    mock.bootstrap(
        "system",
        Path::new("/Library/LaunchDaemons/rs.tell.witness.plist"),
    )
    .expect("ok");
    mock.enable("system", "rs.tell.witness").expect("ok");
    mock.bootout("system", "rs.tell.witness").expect("ok");

    let calls = mock.calls();
    assert_eq!(
        calls,
        vec![
            Call {
                verb: "bootstrap".into(),
                domain: "system".into(),
                target: "/Library/LaunchDaemons/rs.tell.witness.plist".into(),
            },
            Call {
                verb: "enable".into(),
                domain: "system".into(),
                target: "rs.tell.witness".into(),
            },
            Call {
                verb: "bootout".into(),
                domain: "system".into(),
                target: "rs.tell.witness".into(),
            },
        ]
    );
}

#[test]
fn mock_failing_verb_returns_command_failed() {
    let mock = MockPlatform::new().failing("bootstrap");
    let err = mock
        .bootstrap("system", Path::new("/x.plist"))
        .expect_err("should fail");
    assert!(matches!(err, InstallError::CommandFailed { .. }));
    // Non-failing verbs still succeed.
    assert!(mock.enable("system", "rs.tell.witness").is_ok());
}

#[test]
fn mock_is_loaded_and_print_configurable() {
    let mock = MockPlatform::new()
        .loaded(true)
        .print_output("state = running");
    assert!(mock.is_loaded("system", "rs.tell.witness"));
    assert_eq!(
        mock.print("system", "rs.tell.witness").expect("ok"),
        "state = running"
    );
}

#[test]
fn install_error_command_failed_shows_stderr() {
    let err = InstallError::CommandFailed {
        cmd: "launchctl bootstrap".into(),
        stderr: "Bootstrap failed: 5: Input/output error".into(),
    };
    let text = err.to_string();
    assert!(text.contains("launchctl bootstrap"));
    assert!(text.contains("Input/output error"));
}
