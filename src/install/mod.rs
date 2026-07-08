//! `witness install` / `uninstall` / `status`.
//!
//! Platform-split behind `#[cfg(target_os)]`, exactly like `metrics/` and
//! `logs/`: the systemd flow (`systemd.rs`) on Linux, the launchd flow
//! (`launchd.rs`, via a `Platform` trait over `launchctl`) on macOS. Other
//! platforms print `witness setup` guidance.

use std::path::PathBuf;

use crate::setup;

#[cfg(target_os = "linux")]
mod systemd;

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(target_os = "macos")]
mod platform;

// Windows Service install. The SCM trait + mock + install/uninstall/status
// SEQUENCES compile and are unit-tested on any platform (the dev box is macOS);
// only the real SCM impl and the elevation/filesystem flow are Windows-gated
// internally (spec 005).
mod platform_windows;
mod windows;

#[cfg(all(test, target_os = "macos"))]
mod launchd_test;
#[cfg(all(test, target_os = "macos"))]
mod platform_test;
#[cfg(test)]
mod platform_windows_test;
#[cfg(test)]
mod windows_test;

/// Shared install locations. `INSTALL_DIR`/`CONFIG_FILE` are identical across
/// platforms by decision (one documented path, one `--config` default).
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
const INSTALL_DIR: &str = "/usr/local/bin";
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
const CONFIG_FILE: &str = "/etc/witness/config.toml";

/// `witness install` arguments (shared surface across platforms; some fields
/// are only consumed by one platform's flow).
#[derive(clap::Args)]
pub struct InstallArgs {
    /// API key or install token (optional — skips config if omitted)
    #[arg(long)]
    pub token: Option<String>,

    /// Tell server URL (HTTP/HTTPS) for fetching config
    #[arg(long, default_value = "https://tell.rs")]
    pub server: String,

    /// TCP data endpoint override (host:port)
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Skip auto-config fetch, generate config locally
    #[arg(long)]
    pub offline: bool,

    /// Overwrite existing config and service files
    #[arg(long)]
    pub force: bool,
}

/// `witness uninstall` arguments.
#[derive(clap::Args)]
pub struct UninstallArgs {
    /// Also remove config and state (checkpoints, offsets, disk buffer)
    #[arg(long)]
    pub purge: bool,
}

// ─── Dispatch ────────────────────────────────────────────────────────

/// Run `witness install`, dispatching to the platform implementation.
pub fn run(args: InstallArgs) {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = systemd::install(args) {
            fail(&e.to_string());
        }
    }

    #[cfg(target_os = "macos")]
    {
        let platform = platform::MacOsPlatform;
        if let Err(e) = launchd::install(&args, &platform) {
            fail(&e.to_string());
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = windows::install(&args) {
            fail(&e.to_string());
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = args;
        fail(
            "witness install is supported on Linux (systemd), macOS (launchd), and \
             Windows (SCM). On other platforms, run `witness setup` and start the \
             binary directly.",
        );
    }
}

/// Run `witness uninstall`. macOS-only for now; Linux points at `systemctl`.
pub fn run_uninstall(args: UninstallArgs) {
    #[cfg(target_os = "macos")]
    {
        let platform = platform::MacOsPlatform;
        if let Err(e) = launchd::uninstall(&args, &platform) {
            fail(&e.to_string());
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = windows::uninstall(&args) {
            fail(&e.to_string());
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = args;
        fail(
            "witness uninstall is only implemented on macOS and Windows. On Linux, use \
             `systemctl disable --now witness` and remove /etc/systemd/system/witness.service.",
        );
    }
}

/// Run `witness status`. macOS-only for now; Linux points at `systemctl`.
pub fn run_status() {
    #[cfg(target_os = "macos")]
    {
        let platform = platform::MacOsPlatform;
        if let Err(e) = launchd::status(&platform) {
            fail(&e.to_string());
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = windows::status() {
            fail(&e.to_string());
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        fail(
            "witness status is only implemented on macOS and Windows. On Linux, use \
             `systemctl status witness`.",
        );
    }
}

// ─── Windows path accessors (for the service host) ───────────────────

/// The Windows machine-wide config path (`C:\ProgramData\witness\config.toml`).
#[cfg(target_os = "windows")]
pub(crate) fn windows_config_path() -> &'static str {
    windows::CONFIG_FILE
}

/// The Windows own-log directory (`C:\ProgramData\witness\logs`).
#[cfg(target_os = "windows")]
pub(crate) fn windows_log_dir() -> &'static str {
    windows::LOG_DIR
}

// ─── Shared helpers ──────────────────────────────────────────────────

fn fail(msg: &str) -> ! {
    eprintln!("\x1b[31m✗\x1b[0m {msg}");
    std::process::exit(1);
}

/// Build `setup::SetupArgs` from install args and a token. The config path is
/// platform-appropriate: `C:\ProgramData\witness\config.toml` on Windows,
/// `/etc/witness/config.toml` elsewhere.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
fn setup_args(args: &InstallArgs, token: String) -> setup::SetupArgs {
    #[cfg(target_os = "windows")]
    let config = PathBuf::from(windows::CONFIG_FILE);
    #[cfg(not(target_os = "windows"))]
    let config = PathBuf::from(CONFIG_FILE);

    setup::SetupArgs {
        token,
        server: args.server.clone(),
        endpoint: args.endpoint.clone(),
        offline: args.offline,
        config,
        force: args.force,
    }
}
