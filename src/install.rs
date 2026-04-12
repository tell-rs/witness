//! `witness install` — full post-install setup.
//!
//! Mirrors what the curl installer (`curl -sSfL https://tell.rs/agent | bash`)
//! does after downloading the binary: installs to /usr/local/bin, creates a
//! system user, writes the systemd unit, configures, and starts the agent.
//!
//! Token is optional — without it, system setup is performed and instructions
//! for manual configuration are printed.

use std::path::PathBuf;
use std::process::Command;

use crate::setup;

const INSTALL_DIR: &str = "/usr/local/bin";
const CONFIG_DIR: &str = "/etc/witness";
const CONFIG_FILE: &str = "/etc/witness/config.toml";
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

    /// Overwrite existing config and service files
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: InstallArgs) {
    if let Err(e) = execute(args) {
        eprintln!("\x1b[31m✗\x1b[0m {e}");
        std::process::exit(1);
    }
}

fn execute(args: InstallArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Must be root for user creation and systemd
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        return Err("witness install must be run as root (use sudo)".into());
    }

    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\nInstalling witness v{version}...\n");

    // 1. Install binary to /usr/local/bin
    install_binary()?;

    // 2. Create system user
    create_user()?;

    // 3. Write systemd service
    install_service(args.force)?;

    // 4. Configure and start (if token provided)
    if let Some(token) = args.token {
        let setup_args = setup::SetupArgs {
            token,
            server: args.server,
            endpoint: args.endpoint,
            config: PathBuf::from(CONFIG_FILE),
            force: args.force,
        };
        setup::execute_checked(&setup_args)?;
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

// --- Steps ------------------------------------------------------------------

fn install_binary() -> Result<(), Box<dyn std::error::Error>> {
    let current = std::env::current_exe()?.canonicalize()?;
    let target = PathBuf::from(INSTALL_DIR).join("witness");

    // Already in the right place (e.g. re-running after curl install)
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
    // Check if user already exists
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

    // Add to adm group for log file access (best-effort)
    if Command::new("getent")
        .args(["group", "adm"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        if run_cmd("usermod", &["-aG", "adm", "witness"]).is_ok() {
            ok("added witness to adm group");
        }
    }

    Ok(())
}

fn install_service(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(SERVICE_FILE);

    if path.exists() && !force {
        return Ok(());
    }

    // Ensure config directory exists
    std::fs::create_dir_all(CONFIG_DIR)?;

    std::fs::write(&path, SYSTEMD_UNIT)?;
    run_cmd("systemctl", &["daemon-reload"])?;
    ok("installed systemd service");

    Ok(())
}

// --- Helpers ----------------------------------------------------------------

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
