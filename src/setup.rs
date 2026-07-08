//! `witness setup` — fetch config from a Tell server and write it to disk.
//!
//! Uses `curl` for the HTTP request to avoid adding an HTTP client dependency.
//! Falls back to generating a sensible default config if the server is
//! unreachable or does not support the config endpoint yet.

use std::path::PathBuf;

const DEFAULT_SERVER: &str = "https://tell.rs";
const DEFAULT_ENDPOINT: &str = "collect.tell.rs:50000";

#[derive(clap::Args)]
pub struct SetupArgs {
    /// API key or install token
    #[arg(long)]
    pub token: String,

    /// Tell server URL (HTTP/HTTPS) for fetching config
    #[arg(long, default_value = DEFAULT_SERVER)]
    pub server: String,

    /// TCP data endpoint override (host:port)
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Config file path to write
    #[arg(short, long, default_value = "/etc/witness/config.toml")]
    pub config: PathBuf,

    /// Skip auto-config fetch, generate config locally
    #[arg(long)]
    pub offline: bool,

    /// Overwrite existing config file
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: SetupArgs) {
    if let Err(e) = execute(&args) {
        eprintln!("setup failed: {e}");
        std::process::exit(1);
    }
}

/// Run setup, returning errors to the caller instead of exiting.
pub fn execute_checked(args: &SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
    execute(args)
}

fn execute(args: &SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
    validate_token(&args.token)?;

    if args.config.exists() && !args.force {
        return Err(format!(
            "config already exists at {}. Use --force to overwrite.",
            args.config.display()
        )
        .into());
    }

    let endpoint = args.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);

    let config_toml = if args.offline {
        generate_default(&args.token, endpoint)
    } else {
        match fetch_config(&args.server, &args.token) {
            // Validate before writing — a bad server response must not
            // brick the agent's config.
            Ok(toml) => match crate::config::parse_config(&toml) {
                Ok(_) => {
                    eprintln!("fetched config from {}", args.server);
                    toml
                }
                Err(e) => {
                    eprintln!("server returned invalid config ({e}); using local defaults");
                    generate_default(&args.token, endpoint)
                }
            },
            Err(e) => {
                eprintln!("config fetch failed ({e}); using local defaults");
                generate_default(&args.token, endpoint)
            }
        }
    };

    // Ensure parent directory exists
    if let Some(parent) = args.config.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&args.config, config_toml.as_bytes())?;

    // Restrict permissions — config contains the API key.
    // Set owner root:witness so the service (User=witness) can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&args.config, std::fs::Permissions::from_mode(0o640))?;

        // Try to chown to root:witness (best-effort, may fail if user doesn't exist yet)
        let path = std::ffi::CString::new(args.config.to_string_lossy().as_bytes().to_vec())?;
        let group = std::ffi::CString::new("witness").expect("static string has no NUL bytes");
        unsafe {
            let gr = libc::getgrnam(group.as_ptr());
            if !gr.is_null() {
                libc::chown(path.as_ptr(), 0, (*gr).gr_gid);
            }
        }
    }

    eprintln!("config written to {}", args.config.display());
    eprintln!();
    eprintln!("start the agent:");
    eprintln!("  systemctl enable --now witness");

    Ok(())
}

fn validate_token(token: &str) -> Result<(), Box<dyn std::error::Error>> {
    if token.is_empty() {
        return Err("--token is required".into());
    }
    crate::config::validate_api_key(token).map_err(|e| format!("invalid --token: {e}").into())
}

/// Fetch agent config from the Tell server using curl.
///
/// The Authorization header goes to curl via `--config -` (stdin), not argv —
/// argv is visible to every local user through `ps`. Uses the shared
/// [`witness::curl`] invocation so the token-off-argv idiom lives in one place.
fn fetch_config(server: &str, token: &str) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("{}/v1/agent/config", server.trim_end_matches('/'));
    let ua = format!("User-Agent: witness/{}", env!("CARGO_PKG_VERSION"));

    let output = witness::curl::run_with_bearer(
        &["-sSf", "--max-time", "10", "-H", &ua, "--config", "-", &url],
        token,
    )
    .map_err(|e| format!("curl not found: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("HTTP request failed: {}", stderr.trim()).into());
    }

    let body = String::from_utf8(output.stdout)?;
    if body.trim().is_empty() {
        return Err("server returned empty config".into());
    }

    Ok(body)
}

fn generate_default(api_key: &str, endpoint: &str) -> String {
    if crate::logs::journal::is_available() {
        format!(
            r#"api_key = "{api_key}"
endpoint = "{endpoint}"

# Log ingestion: "journald", "files", or "auto"
log_source = "journald"
"#
        )
    } else {
        format!(
            r#"api_key = "{api_key}"
endpoint = "{endpoint}"

# Log ingestion: "journald", "files", or "auto"
log_source = "files"

logs = [
    "/var/log/syslog",
    "/var/log/auth.log",
]
"#
        )
    }
}
