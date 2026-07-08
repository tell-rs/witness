//! Linux `witness install` — systemd flow.
//!
//! Mirrors what the curl installer does after downloading the binary: installs
//! to /usr/local/bin, creates a system user, writes the systemd unit,
//! configures, and starts the agent.

use std::path::PathBuf;
use std::process::Command;

use super::{INSTALL_DIR, InstallArgs, setup_args};
use crate::setup;

const CONFIG_DIR: &str = "/etc/witness";
const SERVICE_FILE: &str = "/etc/systemd/system/witness.service";

const SYSTEMD_UNIT: &str = "\
[Unit]
Description=Witness Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/witness --config /etc/witness/config.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=always
RestartSec=5
User=witness
Group=witness

StandardOutput=journal
StandardError=journal
SyslogIdentifier=witness

StateDirectory=witness

ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadOnlyPaths=/proc /sys /var/log
ReadWritePaths=/var/lib/witness

NoNewPrivileges=yes
CapabilityBoundingSet=
RestrictSUIDSGID=yes
SystemCallFilter=@system-service
MemoryDenyWriteExecute=yes
PrivateDevices=yes

ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes

RestrictNamespaces=yes
LockPersonality=yes
RestrictRealtime=yes

[Install]
WantedBy=multi-user.target
";

pub fn install(args: InstallArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Must be root for user creation and systemd.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        return Err("witness install must be run as root (use sudo)".into());
    }

    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\nInstalling witness v{version}...\n");

    install_binary()?;
    create_user()?;
    install_service(args.force)?;

    if let Some(token) = args.token.clone() {
        setup::execute_checked(&setup_args(&args, token))?;
        ok("configured");

        run_cmd("systemctl", &["enable", "--now", "witness"])?;
        ok("started witness");

        eprintln!("\nverify:");
        eprintln!("  systemctl status witness");
    } else {
        eprintln!("\nconfigure:");
        eprintln!("  witness setup --token YOUR_API_KEY");
        eprintln!("\nthen start:");
        eprintln!("  systemctl enable --now witness");
    }

    eprintln!();
    Ok(())
}

fn install_binary() -> Result<(), Box<dyn std::error::Error>> {
    let current = std::env::current_exe()?.canonicalize()?;
    let target = PathBuf::from(INSTALL_DIR).join("witness");

    if target.exists() && target.canonicalize().ok().as_ref() == Some(&current) {
        return Ok(());
    }

    std::fs::create_dir_all(INSTALL_DIR)?;
    std::fs::copy(&current, &target)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    }

    ok(&format!("installed to {}", target.display()));
    Ok(())
}

fn create_user() -> Result<(), Box<dyn std::error::Error>> {
    if Command::new("id")
        .args(["-u", "witness"])
        .output()?
        .status
        .success()
    {
        return Ok(());
    }

    run_cmd(
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "witness",
        ],
    )?;
    ok("created witness user");

    if Command::new("getent")
        .args(["group", "adm"])
        .output()
        .is_ok_and(|o| o.status.success())
        && run_cmd("usermod", &["-aG", "adm", "witness"]).is_ok()
    {
        ok("added witness to adm group");
    }

    Ok(())
}

fn install_service(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(SERVICE_FILE);

    if path.exists() && !force {
        return Ok(());
    }

    std::fs::create_dir_all(CONFIG_DIR)?;
    std::fs::write(&path, SYSTEMD_UNIT)?;
    run_cmd("systemctl", &["daemon-reload"])?;
    ok("installed systemd service");

    Ok(())
}

fn ok(msg: &str) {
    eprintln!("\x1b[32m✓\x1b[0m {msg}");
}

fn run_cmd(cmd: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(cmd).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{cmd} failed: {}", stderr.trim()).into());
    }
    Ok(())
}
