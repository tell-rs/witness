use super::platform_windows::{
    Call, MockPlatform, ServiceSpec, ServiceState, WindowsInstallError, WindowsServicePlatform,
    image_path_command_line,
};
use std::path::PathBuf;

fn spec() -> ServiceSpec {
    ServiceSpec {
        name: "witness".into(),
        display_name: "Witness Agent".into(),
        description: "desc".into(),
        binary_path: PathBuf::from(r"C:\Program Files\witness\witness.exe"),
        config_path: PathBuf::from(r"C:\ProgramData\witness\config.toml"),
    }
}

#[test]
fn mock_records_verb_and_name() {
    let mock = MockPlatform::new();
    mock.create(&spec()).expect("ok");
    mock.start("witness").expect("ok");
    mock.stop("witness").expect("ok");
    mock.delete("witness").expect("ok");

    let calls = mock.calls();
    assert_eq!(
        calls,
        vec![
            Call {
                verb: "create".into(),
                name: "witness".into()
            },
            Call {
                verb: "start".into(),
                name: "witness".into()
            },
            Call {
                verb: "stop".into(),
                name: "witness".into()
            },
            Call {
                verb: "delete".into(),
                name: "witness".into()
            },
        ]
    );
}

#[test]
fn mock_failing_verb_returns_command_failed() {
    let mock = MockPlatform::new().failing("create");
    let err = mock.create(&spec()).expect_err("should fail");
    assert!(matches!(err, WindowsInstallError::CommandFailed { .. }));
    // Non-failing verbs still succeed.
    assert!(mock.start("witness").is_ok());
}

#[test]
fn mock_query_and_exists_configurable() {
    let mock = MockPlatform::new()
        .existing(true)
        .state(ServiceState::Running);
    assert!(mock.exists("witness"));
    assert_eq!(mock.query_state("witness"), ServiceState::Running);
}

#[test]
fn image_path_command_line_is_fully_quoted() {
    // Unquoted-service-path escalation guard (spec 005 threat model): the
    // binary path with spaces must be quoted.
    let line = image_path_command_line(&spec());
    assert_eq!(
        line,
        r#""C:\Program Files\witness\witness.exe" --config "C:\ProgramData\witness\config.toml""#
    );
    assert!(line.starts_with('"'));
}

#[test]
fn windows_install_error_command_failed_shows_context() {
    let err = WindowsInstallError::CommandFailed {
        verb: "create".into(),
        message: "Access is denied.".into(),
    };
    let text = err.to_string();
    assert!(text.contains("create"));
    assert!(text.contains("Access is denied."));
}

#[test]
fn windows_install_error_not_elevated_message() {
    let text = WindowsInstallError::NotElevated.to_string();
    assert!(text.contains("Administrator"));
}
