//! Windows Service host — runs witness under the Service Control Manager and
//! maps SCM `Stop`/`Shutdown` onto the existing `watch<bool>` cancellation
//! token (the Windows analogue of SIGTERM). Spec 005 R1.
//!
//! The control-message → action mapping is a pure table
//! ([`map_control`]) unit-tested on any platform. The `windows-service`
//! dispatcher, control handler, and status transitions are Windows-gated and
//! verified on a Windows runner.
//!
//! Self-feedback prevention (spec 005 R5): the service host routes witness's
//! own `tracing` output to a file under `C:\ProgramData\witness\logs\`, never
//! the Event Log, so the 004 Event Log source can never read witness back.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// An SCM control witness distinguishes. A pure mirror of `windows-service`'s
/// `ServiceControl` so the mapping is testable off Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScmControl {
    /// `SERVICE_CONTROL_STOP`.
    Stop,
    /// `SERVICE_CONTROL_SHUTDOWN`.
    Shutdown,
    /// `SERVICE_CONTROL_INTERROGATE`.
    Interrogate,
    /// Any other control.
    Other,
}

/// The action the host takes for a control (spec 005 R1 decision table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Cancel the runtime and report `StopPending`.
    CancelAndStopPending,
    /// Re-report the current status.
    ReReport,
    /// `ServiceControlHandlerResult::NotImplemented`.
    NotImplemented,
}

/// Map an SCM control to the host action. Pure; unit-tested on any platform.
#[must_use]
pub fn map_control(control: ScmControl) -> ControlAction {
    match control {
        ScmControl::Stop | ScmControl::Shutdown => ControlAction::CancelAndStopPending,
        ScmControl::Interrogate => ControlAction::ReReport,
        ScmControl::Other => ControlAction::NotImplemented,
    }
}

// ─── Windows-only SCM plumbing ───────────────────────────────────────

#[cfg(target_os = "windows")]
pub use windows_host::dispatch;

/// The service watch receiver, if witness is running under the SCM. Consumed by
/// `main::wait_for_signal` so an SCM Stop/Shutdown flips the same cancellation
/// path as a Unix signal.
#[cfg(target_os = "windows")]
pub fn shutdown_receiver() -> Option<tokio::sync::watch::Receiver<bool>> {
    windows_host::SERVICE_SHUTDOWN
        .get()
        .map(tokio::sync::watch::Sender::subscribe)
}

#[cfg(target_os = "windows")]
mod windows_host {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use tokio::sync::watch;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{Error as WsError, define_windows_service, service_dispatcher};

    use super::{ControlAction, ScmControl, map_control};

    /// Service name (SCM key) — must match the install registration.
    const SERVICE_NAME: &str = "witness";
    /// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` — returned by the dispatcher
    /// when the process was not launched by the SCM (console/dev run).
    const NOT_SCM_LAUNCHED: i32 = 1063;

    /// Service watch sender: flipped by the control handler on Stop/Shutdown.
    pub(super) static SERVICE_SHUTDOWN: OnceLock<watch::Sender<bool>> = OnceLock::new();
    /// Registered status handle, stored so the control handler can report
    /// `StopPending` immediately.
    static STATUS_HANDLE: OnceLock<service_control_handler::ServiceStatusHandle> = OnceLock::new();
    /// Config path, captured from the CLI before the SCM dispatch.
    static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// Attempt to run under the SCM. Returns `true` if the SCM dispatched us
    /// (ran and stopped as a service); `false` if launched from a console (the
    /// caller then runs in the foreground).
    pub fn dispatch(config_path: PathBuf) -> bool {
        let _ = CONFIG_PATH.set(config_path);
        match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
            Ok(()) => true,
            Err(WsError::Winapi(e)) if e.raw_os_error() == Some(NOT_SCM_LAUNCHED) => false,
            Err(_) => false,
        }
    }

    /// Entry point invoked by the SCM dispatcher.
    fn service_main(_arguments: Vec<OsString>) {
        // Own logs to a file, never the Event Log (spec 005 R5).
        crate::init_windows_file_tracing();
        if let Err(e) = run_service() {
            tracing::error!("windows service host failed: {e}");
        }
    }

    fn run_service() -> Result<(), WsError> {
        let (shutdown_tx, _rx) = watch::channel(false);
        let _ = SERVICE_SHUTDOWN.set(shutdown_tx);

        let status_handle = service_control_handler::register(SERVICE_NAME, control_handler)?;
        let _ = STATUS_HANDLE.set(status_handle);

        report(
            status_handle,
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        );

        // Run the agent until the control handler flips the watch. `main`'s
        // `wait_for_signal` observes `shutdown_receiver()`, so the existing
        // drain/flush path runs unchanged.
        let config = CONFIG_PATH
            .get()
            .cloned()
            .unwrap_or_else(|| PathBuf::from(crate::install::windows_config_path()));
        match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(crate::agent_loop(&config)),
            Err(e) => tracing::error!("failed to build runtime: {e}"),
        }

        report(
            status_handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
        );
        Ok(())
    }

    /// SCM control handler: applies the pure [`map_control`] decision.
    fn control_handler(control: ServiceControl) -> ServiceControlHandlerResult {
        let mapped = match control {
            ServiceControl::Stop => ScmControl::Stop,
            ServiceControl::Shutdown => ScmControl::Shutdown,
            ServiceControl::Interrogate => ScmControl::Interrogate,
            _ => ScmControl::Other,
        };
        match map_control(mapped) {
            ControlAction::CancelAndStopPending => {
                if let Some(tx) = SERVICE_SHUTDOWN.get() {
                    let _ = tx.send(true);
                }
                if let Some(handle) = STATUS_HANDLE.get() {
                    report(
                        *handle,
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                    );
                }
                ServiceControlHandlerResult::NoError
            }
            ControlAction::ReReport => ServiceControlHandlerResult::NoError,
            ControlAction::NotImplemented => ServiceControlHandlerResult::NotImplemented,
        }
    }

    fn report(
        handle: service_control_handler::ServiceStatusHandle,
        state: ServiceState,
        accepts: ServiceControlAccept,
    ) {
        let _ = handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepts,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    }
}
