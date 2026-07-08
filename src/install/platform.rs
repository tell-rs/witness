//! `launchctl` abstraction for the macOS install flow.
//!
//! A small `Platform` trait wraps only the `launchctl` verbs witness needs, so
//! the install/uninstall/status flows are unit-testable against a
//! [`MockPlatform`] with no real `launchctl` (the macwarden model). The real
//! [`MacOsPlatform`] shells out via `std::process::Command`.

use std::path::Path;
use std::process::Command;

/// Errors from the install flow (lib-module convention: `thiserror`, not
/// `anyhow`).
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("`{cmd}` failed:\n{stderr}")]
    CommandFailed { cmd: String, stderr: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("witness install must be run as root (use sudo)")]
    NotRoot,
}

/// The `launchctl` operations witness needs. Domains are `"system"` for a
/// LaunchDaemon; labels/paths are witness-controlled constants passed as argv
/// (never through a shell).
pub trait Platform {
    /// `launchctl bootstrap <domain> <plist_path>`
    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), InstallError>;
    /// `launchctl bootout <domain> <target>` (`target` = plist path or label).
    fn bootout(&self, domain: &str, target: &str) -> Result<(), InstallError>;
    /// `launchctl enable <domain>/<label>`
    fn enable(&self, domain: &str, label: &str) -> Result<(), InstallError>;
    /// Whether `launchctl print <domain>/<label>` succeeds (daemon loaded).
    fn is_loaded(&self, domain: &str, label: &str) -> bool;
    /// `launchctl print <domain>/<label>` → stdout for status summaries.
    fn print(&self, domain: &str, label: &str) -> Result<String, InstallError>;
}

/// Real `launchctl` platform.
pub struct MacOsPlatform;

impl MacOsPlatform {
    fn run(args: &[&str]) -> Result<String, InstallError> {
        let output = Command::new("launchctl").args(args).output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(InstallError::CommandFailed {
                cmd: format!("launchctl {}", args.join(" ")),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }
}

impl Platform for MacOsPlatform {
    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), InstallError> {
        Self::run(&["bootstrap", domain, &plist_path.to_string_lossy()]).map(drop)
    }

    fn bootout(&self, domain: &str, target: &str) -> Result<(), InstallError> {
        Self::run(&["bootout", domain, target]).map(drop)
    }

    fn enable(&self, domain: &str, label: &str) -> Result<(), InstallError> {
        Self::run(&["enable", &format!("{domain}/{label}")]).map(drop)
    }

    fn is_loaded(&self, domain: &str, label: &str) -> bool {
        Command::new("launchctl")
            .args(["print", &format!("{domain}/{label}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn print(&self, domain: &str, label: &str) -> Result<String, InstallError> {
        Self::run(&["print", &format!("{domain}/{label}")])
    }
}

/// A recorded `launchctl` invocation `(verb, domain, target)`.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub verb: String,
    pub domain: String,
    pub target: String,
}

/// Test double recording every call and returning configured results.
#[cfg(test)]
pub struct MockPlatform {
    calls: std::sync::Mutex<Vec<Call>>,
    /// When set, the named verb fails with `CommandFailed`.
    fail_verb: Option<String>,
    loaded: bool,
    print_output: String,
}

#[cfg(test)]
impl MockPlatform {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_verb: None,
            loaded: false,
            print_output: String::new(),
        }
    }

    pub fn loaded(mut self, loaded: bool) -> Self {
        self.loaded = loaded;
        self
    }

    pub fn print_output(mut self, output: &str) -> Self {
        self.print_output = output.to_string();
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

    fn record(&self, verb: &str, domain: &str, target: &str) -> Result<(), InstallError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Call {
                verb: verb.to_string(),
                domain: domain.to_string(),
                target: target.to_string(),
            });
        if self.fail_verb.as_deref() == Some(verb) {
            return Err(InstallError::CommandFailed {
                cmd: format!("launchctl {verb}"),
                stderr: "mock failure".to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
impl Platform for MockPlatform {
    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), InstallError> {
        self.record("bootstrap", domain, &plist_path.to_string_lossy())
    }

    fn bootout(&self, domain: &str, target: &str) -> Result<(), InstallError> {
        self.record("bootout", domain, target)
    }

    fn enable(&self, domain: &str, label: &str) -> Result<(), InstallError> {
        self.record("enable", domain, label)
    }

    fn is_loaded(&self, domain: &str, label: &str) -> bool {
        let _ = self.record("is_loaded", domain, label);
        self.loaded
    }

    fn print(&self, domain: &str, label: &str) -> Result<String, InstallError> {
        self.record("print", domain, label)?;
        Ok(self.print_output.clone())
    }
}
