//! SCM abstraction for the Windows install flow (spec 005 R2).
//!
//! A small [`WindowsServicePlatform`] trait wraps only the Service Control
//! Manager verbs the install flow needs, so the install/uninstall/status
//! sequences are unit-testable against a [`MockPlatform`] with no real SCM (the
//! macwarden / launchd model). The real [`ScmPlatform`] over `windows-service`'s
//! `ServiceManager` is Windows-gated; everything else in this module compiles
//! and is tested on the macOS dev box.
//!
//! SCM verb injection is impossible: all operations go through the typed
//! `ServiceManager` API with witness-controlled constant names/paths, never a
//! shell or user-formatted command line (spec 005 threat model).
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::PathBuf;

/// Errors from the Windows install flow. Matches the `InstallError` convention
/// (`CommandFailed`/`Io`/elevation), with a distinct `NotElevated`.
#[derive(Debug, thiserror::Error)]
pub enum WindowsInstallError {
    #[error("SCM `{verb}` failed: {message}")]
    CommandFailed { verb: String, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("witness install must be run as Administrator")]
    NotElevated,
}

/// Service registration parameters. All fields are witness-controlled constants
/// or the fixed installed binary/config paths — never user input (spec 005
/// threat model: no unquoted-path or SCM-injection surface).
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// Service name (SCM key), a constant.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Service description.
    pub description: String,
    /// Fully-qualified installed binary path.
    pub binary_path: PathBuf,
    /// Config path passed as `--config <path>`.
    pub config_path: PathBuf,
}

/// Simplified service state for `witness status` and the uninstall decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is registered and running.
    Running,
    /// The service is registered and stopped.
    Stopped,
    /// The service is not registered with the SCM.
    NotInstalled,
    /// Any other transitional/registered state.
    Other,
}

/// The SCM verbs the install flow needs. Structured args only — never a shell.
pub trait WindowsServicePlatform {
    /// Register a new service (`CreateService`).
    fn create(&self, spec: &ServiceSpec) -> Result<(), WindowsInstallError>;
    /// Remove a service (`DeleteService`).
    fn delete(&self, name: &str) -> Result<(), WindowsInstallError>;
    /// Start a service (`StartService`).
    fn start(&self, name: &str) -> Result<(), WindowsInstallError>;
    /// Stop a running service (`ControlService`).
    fn stop(&self, name: &str) -> Result<(), WindowsInstallError>;
    /// Query the current state (`QueryServiceStatus`).
    fn query_state(&self, name: &str) -> ServiceState;
    /// Whether the service is registered.
    fn exists(&self, name: &str) -> bool;
}

/// The command line that would be registered as the service `ImagePath`, fully
/// quoted. Used for verification output and pinned by a unit test so the
/// unquoted-service-path escalation (spec 005 threat model) cannot regress. The
/// real registration passes `binary_path` + `launch_arguments` structurally to
/// `windows-service`, which quotes the executable path itself.
#[must_use]
pub fn image_path_command_line(spec: &ServiceSpec) -> String {
    format!(
        "\"{}\" --config \"{}\"",
        spec.binary_path.display(),
        spec.config_path.display()
    )
}

// ─── Real SCM platform (Windows-only) ────────────────────────────────

/// Real SCM platform over `windows-service`'s `ServiceManager`.
#[cfg(target_os = "windows")]
pub struct ScmPlatform;

#[cfg(target_os = "windows")]
impl ScmPlatform {
    fn manager(
        access: windows_service::service_manager::ServiceManagerAccess,
    ) -> Result<windows_service::service_manager::ServiceManager, WindowsInstallError> {
        windows_service::service_manager::ServiceManager::local_computer(None::<&str>, access)
            .map_err(|e| WindowsInstallError::CommandFailed {
                verb: "open_manager".to_string(),
                message: e.to_string(),
            })
    }
}

#[cfg(target_os = "windows")]
impl WindowsServicePlatform for ScmPlatform {
    fn create(&self, spec: &ServiceSpec) -> Result<(), WindowsInstallError> {
        use std::ffi::OsString;
        use windows_service::service::{
            ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
        };
        use windows_service::service_manager::ServiceManagerAccess;

        let manager =
            Self::manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let info = ServiceInfo {
            name: OsString::from(&spec.name),
            display_name: OsString::from(&spec.display_name),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: spec.binary_path.clone(),
            launch_arguments: vec![
                OsString::from("--config"),
                OsString::from(spec.config_path.as_os_str()),
            ],
            dependencies: vec![],
            // `None` = LocalSystem, which holds SeSecurityPrivilege for the
            // Event Log Security channel (spec 004 R6 / 005 background).
            account_name: None,
            account_password: None,
        };
        let service = manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .map_err(|e| WindowsInstallError::CommandFailed {
                verb: "create".to_string(),
                message: e.to_string(),
            })?;
        let _ = service.set_description(&spec.description);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), WindowsInstallError> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let manager = Self::manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(name, ServiceAccess::DELETE)
            .map_err(|e| scm_err("delete", e))?;
        service.delete().map_err(|e| scm_err("delete", e))
    }

    fn start(&self, name: &str) -> Result<(), WindowsInstallError> {
        use std::ffi::OsStr;
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let manager = Self::manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(name, ServiceAccess::START)
            .map_err(|e| scm_err("start", e))?;
        service
            .start::<&OsStr>(&[])
            .map_err(|e| scm_err("start", e))
    }

    fn stop(&self, name: &str) -> Result<(), WindowsInstallError> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let manager = Self::manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(name, ServiceAccess::STOP)
            .map_err(|e| scm_err("stop", e))?;
        service.stop().map(drop).map_err(|e| scm_err("stop", e))
    }

    fn query_state(&self, name: &str) -> ServiceState {
        use windows_service::service::{ServiceAccess, ServiceState as WsState};
        use windows_service::service_manager::ServiceManagerAccess;

        let Ok(manager) = Self::manager(ServiceManagerAccess::CONNECT) else {
            return ServiceState::Other;
        };
        let Ok(service) = manager.open_service(name, ServiceAccess::QUERY_STATUS) else {
            return ServiceState::NotInstalled;
        };
        match service.query_status() {
            Ok(status) => match status.current_state {
                WsState::Running => ServiceState::Running,
                WsState::Stopped => ServiceState::Stopped,
                _ => ServiceState::Other,
            },
            Err(_) => ServiceState::Other,
        }
    }

    fn exists(&self, name: &str) -> bool {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let Ok(manager) = Self::manager(ServiceManagerAccess::CONNECT) else {
            return false;
        };
        manager
            .open_service(name, ServiceAccess::QUERY_STATUS)
            .is_ok()
    }
}

#[cfg(target_os = "windows")]
fn scm_err(verb: &str, e: windows_service::Error) -> WindowsInstallError {
    WindowsInstallError::CommandFailed {
        verb: verb.to_string(),
        message: e.to_string(),
    }
}

// ─── Mock platform (test-only, any OS) ───────────────────────────────

/// A recorded SCM invocation `(verb, service_name)`.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub verb: String,
    pub name: String,
}

/// Test double recording every call and returning configured results.
#[cfg(test)]
pub struct MockPlatform {
    calls: std::sync::Mutex<Vec<Call>>,
    fail_verb: Option<String>,
    exists: bool,
    state: ServiceState,
}

#[cfg(test)]
impl MockPlatform {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_verb: None,
            exists: false,
            state: ServiceState::NotInstalled,
        }
    }

    pub fn existing(mut self, exists: bool) -> Self {
        self.exists = exists;
        self
    }

    pub fn state(mut self, state: ServiceState) -> Self {
        self.state = state;
        self
    }

    pub fn failing(mut self, verb: &str) -> Self {
        self.fail_verb = Some(verb.to_string());
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, verb: &str, name: &str) -> Result<(), WindowsInstallError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Call {
                verb: verb.to_string(),
                name: name.to_string(),
            });
        if self.fail_verb.as_deref() == Some(verb) {
            return Err(WindowsInstallError::CommandFailed {
                verb: verb.to_string(),
                message: "mock failure".to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
impl WindowsServicePlatform for MockPlatform {
    fn create(&self, spec: &ServiceSpec) -> Result<(), WindowsInstallError> {
        self.record("create", &spec.name)
    }

    fn delete(&self, name: &str) -> Result<(), WindowsInstallError> {
        self.record("delete", name)
    }

    fn start(&self, name: &str) -> Result<(), WindowsInstallError> {
        self.record("start", name)
    }

    fn stop(&self, name: &str) -> Result<(), WindowsInstallError> {
        self.record("stop", name)
    }

    fn query_state(&self, name: &str) -> ServiceState {
        let _ = self.record("query_state", name);
        self.state
    }

    fn exists(&self, name: &str) -> bool {
        let _ = self.record("exists", name);
        self.exists
    }
}
