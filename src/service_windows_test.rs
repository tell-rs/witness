use super::service_windows::{ControlAction, ScmControl, map_control};

#[test]
fn stop_cancels_and_reports_stop_pending() {
    assert_eq!(
        map_control(ScmControl::Stop),
        ControlAction::CancelAndStopPending
    );
}

#[test]
fn shutdown_cancels_and_reports_stop_pending() {
    assert_eq!(
        map_control(ScmControl::Shutdown),
        ControlAction::CancelAndStopPending
    );
}

#[test]
fn interrogate_re_reports() {
    assert_eq!(
        map_control(ScmControl::Interrogate),
        ControlAction::ReReport
    );
}

#[test]
fn other_controls_not_implemented() {
    assert_eq!(
        map_control(ScmControl::Other),
        ControlAction::NotImplemented
    );
}
