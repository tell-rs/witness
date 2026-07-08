//! macOS `witness install` — launchd flow.
//!
//! Installs the binary, writes a `/Library/LaunchDaemons` plist, and loads it
//! via `launchctl bootstrap`/`enable` (rejecting `brew services`). The
//! `launchctl` calls go through the [`Platform`] trait so the load/unload/status
//! logic is unit-testable with `MockPlatform`.

use std::path::{Path, PathBuf};

use super::platform::{InstallError, Platform};
use super::{INSTALL_DIR, InstallArgs, UninstallArgs, setup_args};
use crate::config;
use crate::setup;

/// launchd label / reverse-DNS bundle id (matches the 001 self-feedback filter).
pub(crate) const LABEL: &str = "rs.tell.witness";
/// System-domain daemons.
pub(crate) const DOMAIN: &str = "system";
const PLIST_PATH: &str = "/Library/LaunchDaemons/rs.tell.witness.plist";
const CONFIG_DIR: &str = "/etc/witness";
const CONFIG_FILE: &str = "/etc/witness/config.toml";
/// Daemon stdout/stderr live under `/Library/Logs` (Mac-native), NOT
/// `/var/log/witness` — deliberate deviation from spec 002 R1/R4.
const LOG_DIR: &str = "/Library/Logs/witness";
const STDOUT_PATH: &str = "/Library/Logs/witness/witness.out.log";
const STDERR_PATH: &str = "/Library/Logs/witness/witness.err.log";

// ─── Flows ───────────────────────────────────────────────────────────

/// `witness install` on macOS.
pub fn install(args: &InstallArgs, platform: &dyn Platform) -> Result<(), InstallError> {
    require_root()?;

    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\nInstalling witness v{version} (launchd)...\n");

    let binary = install_binary()?;
    create_dirs()?;
    write_plist(&binary, args.force)?;

    if let Some(token) = args.token.clone() {
        setup::execute_checked(&setup_args(args, token)).map_err(|e| {
            InstallError::CommandFailed {
                cmd: "witness setup".to_string(),
                stderr: e.to_string(),
            }
        })?;
        ok("configured");
    }

    load_daemon(platform, Path::new(PLIST_PATH))?;
    ok("loaded launchd daemon");

    if args.token.is_none() {
        eprintln!("\nconfigure:");
        eprintln!("  witness setup --token YOUR_API_KEY");
    }
    eprintln!("\nverify:");
    eprintln!("  launchctl print {DOMAIN}/{LABEL}");
    eprintln!();
    Ok(())
}

/// `witness uninstall` on macOS.
pub fn uninstall(args: &UninstallArgs, platform: &dyn Platform) -> Result<(), InstallError> {
    require_root()?;

    // Best-effort bootout: a not-loaded daemon must not block plist removal.
    if let Err(e) = unload_daemon(platform, PLIST_PATH) {
        eprintln!("\x1b[33m!\x1b[0m bootout: {e}");
    } else {
        ok("unloaded launchd daemon");
    }

    match std::fs::remove_file(PLIST_PATH) {
        Ok(()) => ok("removed plist"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
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

/// `witness status` on macOS.
pub fn status(platform: &dyn Platform) -> Result<(), InstallError> {
    let report = status_report(platform);
    if report.loaded {
        eprintln!("witness: loaded ({DOMAIN}/{LABEL})");
        eprintln!("  running: {}", if report.running { "yes" } else { "no" });
        if let Some(exit) = &report.last_exit {
            eprintln!("  last exit: {exit}");
        }
    } else {
        eprintln!("witness: not loaded");
        eprintln!("  install with: sudo witness install --token YOUR_API_KEY");
    }
    Ok(())
}

// ─── launchctl sequences (unit-tested with MockPlatform) ─────────────

/// Load the daemon: `bootstrap system <plist>` then `enable system/<label>`.
pub(crate) fn load_daemon(platform: &dyn Platform, plist_path: &Path) -> Result<(), InstallError> {
    platform.bootstrap(DOMAIN, plist_path)?;
    platform.enable(DOMAIN, LABEL)?;
    Ok(())
}

/// Unload the daemon: `bootout system <plist>`.
pub(crate) fn unload_daemon(platform: &dyn Platform, plist_path: &str) -> Result<(), InstallError> {
    platform.bootout(DOMAIN, plist_path)
}

/// Summarized daemon status.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StatusReport {
    pub loaded: bool,
    pub running: bool,
    pub last_exit: Option<String>,
}

pub(crate) fn status_report(platform: &dyn Platform) -> StatusReport {
    let loaded = platform.is_loaded(DOMAIN, LABEL);
    if !loaded {
        return StatusReport {
            loaded: false,
            running: false,
            last_exit: None,
        };
    }
    let detail = platform.print(DOMAIN, LABEL).unwrap_or_default();
    StatusReport {
        loaded: true,
        running: detail.contains("state = running"),
        last_exit: last_exit_of(&detail),
    }
}

/// Extract the `last exit code = N` value from `launchctl print` output.
fn last_exit_of(print_output: &str) -> Option<String> {
    print_output
        .lines()
        .find_map(|l| l.trim().strip_prefix("last exit code = "))
        .map(|v| v.trim().to_string())
}

// ─── Filesystem steps ────────────────────────────────────────────────

fn require_root() -> Result<(), InstallError> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(InstallError::NotRoot);
    }
    Ok(())
}

/// Copy the running binary to `/usr/local/bin/witness` (mode 0755). Returns the
/// installed path (for the plist `ProgramArguments`).
fn install_binary() -> Result<PathBuf, InstallError> {
    use std::os::unix::fs::PermissionsExt;

    let current = std::env::current_exe()?.canonicalize()?;
    let target = PathBuf::from(INSTALL_DIR).join("witness");

    if !(target.exists() && target.canonicalize().ok().as_ref() == Some(&current)) {
        std::fs::create_dir_all(INSTALL_DIR)?;
        std::fs::copy(&current, &target)?;
    }
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    ok(&format!("installed to {}", target.display()));
    Ok(target)
}

/// Create the config dir and the daemon log dir (`0750`, `root:wheel`).
fn create_dirs() -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(CONFIG_DIR)?;
    std::fs::create_dir_all(LOG_DIR)?;
    std::fs::set_permissions(LOG_DIR, std::fs::Permissions::from_mode(0o750))?;
    chown_root_wheel(LOG_DIR);
    Ok(())
}

/// Write the plist `root:wheel 0644`, honoring `--force` for overwrite.
fn write_plist(binary: &Path, force: bool) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;

    let path = Path::new(PLIST_PATH);
    if path.exists() && !force {
        eprintln!("\x1b[33m!\x1b[0m plist exists ({PLIST_PATH}); use --force to overwrite");
        return Ok(());
    }
    let contents = plist_contents(&binary.to_string_lossy(), CONFIG_FILE);
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    chown_root_wheel(PLIST_PATH);
    ok(&format!("wrote {PLIST_PATH}"));
    Ok(())
}

fn chown_root_wheel(path: &str) {
    if let Ok(c) = std::ffi::CString::new(path) {
        // root:wheel = 0:0. Best-effort; already running as root.
        unsafe {
            libc::chown(c.as_ptr(), 0, 0);
        }
    }
}

/// Escape a string for use inside an XML text node (`&` and `<`/`>` would
/// otherwise break the plist; quotes are escaped for robustness).
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Render the LaunchDaemon plist. `plutil -lint`-valid XML property list.
pub(crate) fn plist_contents(binary: &str, config: &str) -> String {
    let binary = xml_escape(binary);
    let config = xml_escape(config);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>--config</string>
        <string>{config}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{STDOUT_PATH}</string>
    <key>StandardErrorPath</key>
    <string>{STDERR_PATH}</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#
    )
}

fn ok(msg: &str) {
    eprintln!("\x1b[32m✓\x1b[0m {msg}");
}
