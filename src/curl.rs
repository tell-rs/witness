//! Shared `curl` subprocess invocation.
//!
//! `witness setup` (one-shot fetch) and the remote-config poller (spec 007)
//! both fetch `/v1/agent/config` from a Tell control plane with a bearer token.
//! The token is supplied to `curl` through its stdin config (`--config -`),
//! never on argv — argv is visible to every local user via `ps`. Centralizing
//! that idiom here keeps the one security-critical detail (token off argv) in a
//! single, audited place.

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Run `curl` with `args`, writing `Authorization: Bearer <token>` to its stdin
/// config so the token never appears on argv, and return the completed
/// [`Output`].
///
/// Blocking: call from a synchronous context (`witness setup`) or from
/// `tokio::task::spawn_blocking` (the poller). TLS certificate verification is
/// curl's default — callers must not pass `-k`/`--insecure`.
///
/// # Errors
///
/// Returns the spawn/IO error. A missing `curl` binary surfaces as
/// [`std::io::ErrorKind::NotFound`], which the poller treats as "disable
/// polling" rather than a transient failure (spec 007 R6).
pub fn run_with_bearer(args: &[&str], token: &str) -> std::io::Result<Output> {
    let mut child = Command::new("curl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "header = \"Authorization: Bearer {token}\"")?;
    }

    child.wait_with_output()
}
