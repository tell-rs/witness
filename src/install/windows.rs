//! Windows `witness install` / `uninstall` / `status` — SCM flow (spec 005).
//!
//! The install/uninstall/status SEQUENCES (which SCM verbs, in what order) are
//! pure functions over the [`WindowsServicePlatform`] trait, unit-tested with
//! `MockPlatform` on the macOS dev box. The full flows that touch elevation and
//! the filesystem are Windows-gated and verified on a Windows runner.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use super::platform_windows::{
    ServiceSpec, ServiceState, WindowsInstallError, WindowsServicePlatform,
};

/// Service name (SCM key) — a constant, never user input.
pub(crate) const SERVICE_NAME: &str = "witness";
/// Service display name.
pub(crate) const DISPLAY_NAME: &str = "Witness Agent";
/// Service description.
pub(crate) const DESCRIPTION: &str = "Lightweight host monitoring agent for the Tell platform";
/// Per-machine install location (admin-writable; `Users` lack write — spec 005
/// threat model). Fully quoted at registration to avoid the unquoted-path
/// escalation.
pub(crate) const INSTALL_DIR: &str = r"C:\Program Files\witness";
/// Machine-wide config path (Windows analogue of `/etc/witness/config.toml`).
pub(crate) const CONFIG_FILE: &str = r"C:\ProgramData\witness\config.toml";
/// Own-log directory — witness's `tracing` output goes here, never the Event
/// Log (spec 004 R5 / 005 R5 self-feedback prevention).
pub(crate) const LOG_DIR: &str = r"C:\ProgramData\witness\logs";
/// Root of the machine-wide data dir (created by install).
pub(crate) const DATA_DIR: &str = r"C:\ProgramData\witness";

// ─── SCM sequences (unit-tested with MockPlatform, any platform) ─────

/// Install sequence: register then start. An existing service is left intact
/// unless `force` (then delete + recreate), mirroring the launchd/systemd
/// `--force` behavior (spec 005 R3).
///
/// Returns `Ok(false)` when an existing service was left untouched (no
/// `--force`), `Ok(true)` when it created + started.
pub(crate) fn install_sequence(
    platform: &dyn WindowsServicePlatform,
    spec: &ServiceSpec,
    force: bool,
) -> Result<bool, WindowsInstallError> {
    let exists = platform.exists(&spec.name);
    if exists && !force {
        return Ok(false);
    }
    if exists {
        platform.delete(&spec.name)?;
    }
    platform.create(spec)?;
    platform.start(&spec.name)?;
    Ok(true)
}

/// Uninstall sequence: stop if running, then delete (spec 005 R3).
pub(crate) fn uninstall_sequence(
    platform: &dyn WindowsServicePlatform,
    name: &str,
) -> Result<(), WindowsInstallError> {
    if platform.query_state(name) == ServiceState::Running {
        platform.stop(name)?;
    }
    platform.delete(name)
}

/// Summarized service status, parallel to the macOS `StatusReport`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StatusReport {
    pub loaded: bool,
    pub running: bool,
}

pub(crate) fn status_report(platform: &dyn WindowsServicePlatform, name: &str) -> StatusReport {
    match platform.query_state(name) {
        ServiceState::NotInstalled => StatusReport {
            loaded: false,
            running: false,
        },
        ServiceState::Running => StatusReport {
            loaded: true,
            running: true,
        },
        ServiceState::Stopped | ServiceState::Other => StatusReport {
            loaded: true,
            running: false,
        },
    }
}

/// Build the [`ServiceSpec`] for the installed binary.
pub(crate) fn service_spec(binary_path: std::path::PathBuf) -> ServiceSpec {
    ServiceSpec {
        name: SERVICE_NAME.to_string(),
        display_name: DISPLAY_NAME.to_string(),
        description: DESCRIPTION.to_string(),
        binary_path,
        config_path: std::path::PathBuf::from(CONFIG_FILE),
    }
}

// ─── Full flows (Windows-only: elevation + filesystem) ───────────────

#[cfg(target_os = "windows")]
pub use windows_flow::{install, status, uninstall};

#[cfg(target_os = "windows")]
mod windows_flow {
    use std::path::{Path, PathBuf};

    use super::super::platform_windows::{ScmPlatform, WindowsInstallError};
    use super::super::{InstallArgs, UninstallArgs, setup_args};
    use super::*;
    use crate::config;
    use crate::setup;

    /// `witness install` on Windows.
    pub fn install(args: &InstallArgs) -> Result<(), WindowsInstallError> {
        require_elevated()?;

        let version = env!("CARGO_PKG_VERSION");
        eprintln!("\nInstalling witness v{version} (Windows Service)...\n");

        let binary = install_binary()?;
        create_dirs()?;

        if let Some(token) = args.token.clone() {
            setup::execute_checked(&setup_args(args, token)).map_err(|e| {
                WindowsInstallError::CommandFailed {
                    verb: "setup".to_string(),
                    message: e.to_string(),
                }
            })?;
            ok("configured");
        }

        let spec = service_spec(binary);
        eprintln!(
            "  image path: {}",
            super::super::platform_windows::image_path_command_line(&spec)
        );
        let platform = ScmPlatform;
        if install_sequence(&platform, &spec, args.force)? {
            ok("registered and started service");
        } else {
            eprintln!("\x1b[33m!\x1b[0m service exists; use --force to re-register");
        }

        if args.token.is_none() {
            eprintln!("\nconfigure:");
            eprintln!("  witness setup --token YOUR_API_KEY");
        }
        eprintln!("\nverify:");
        eprintln!("  sc query {SERVICE_NAME}");
        eprintln!("  Get-Service {SERVICE_NAME}");
        eprintln!();
        Ok(())
    }

    /// `witness uninstall` on Windows.
    pub fn uninstall(args: &UninstallArgs) -> Result<(), WindowsInstallError> {
        require_elevated()?;

        let platform = ScmPlatform;
        if let Err(e) = uninstall_sequence(&platform, SERVICE_NAME) {
            eprintln!("\x1b[33m!\x1b[0m {e}");
        } else {
            ok("stopped and removed service");
        }

        if args.purge {
            let _ = std::fs::remove_dir_all(config::state_dir());
            let _ = std::fs::remove_file(CONFIG_FILE);
            ok("purged config and state");
        } else {
            eprintln!("\nconfig and state retained (use --purge to remove).");
        }
        eprintln!();
        Ok(())
    }

    /// `witness status` on Windows.
    pub fn status() -> Result<(), WindowsInstallError> {
        let platform = ScmPlatform;
        let report = status_report(&platform, SERVICE_NAME);
        if report.loaded {
            eprintln!("witness: installed ({SERVICE_NAME})");
            eprintln!("  running: {}", if report.running { "yes" } else { "no" });
        } else {
            eprintln!("witness: not installed");
            eprintln!("  install with: witness install --token YOUR_API_KEY (as Administrator)");
        }
        Ok(())
    }

    /// Fail closed with `NotElevated` before any filesystem write if not
    /// running as Administrator (spec 005 R3 / threat model).
    fn require_elevated() -> Result<(), WindowsInstallError> {
        if is_elevated() {
            Ok(())
        } else {
            Err(WindowsInstallError::NotElevated)
        }
    }

    /// Whether the current process token is elevated (member of the
    /// Administrators group with an elevated token).
    fn is_elevated() -> bool {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Security::{
            GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut ret_len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(std::ptr::from_mut(&mut elevation).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret_len,
            )
            .is_ok();
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// Copy the running binary to `C:\Program Files\witness\witness.exe`.
    fn install_binary() -> Result<PathBuf, WindowsInstallError> {
        let current = std::env::current_exe()?.canonicalize()?;
        let target = Path::new(INSTALL_DIR).join("witness.exe");

        let same = target.exists() && target.canonicalize().ok().as_ref() == Some(&current);
        if !same {
            std::fs::create_dir_all(INSTALL_DIR)?;
            std::fs::copy(&current, &target)?;
        }
        ok(&format!("installed to {}", target.display()));
        Ok(target)
    }

    /// Create `C:\ProgramData\witness\` and its `logs\` subdir.
    fn create_dirs() -> Result<(), WindowsInstallError> {
        std::fs::create_dir_all(DATA_DIR)?;
        std::fs::create_dir_all(LOG_DIR)?;
        std::fs::create_dir_all(config::state_dir())?;
        Ok(())
    }

    fn ok(msg: &str) {
        eprintln!("\x1b[32m\u{2713}\x1b[0m {msg}");
    }
}
