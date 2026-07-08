//! Remote config polling (spec 007).
//!
//! When the config's `server` field is an `https://` URL, an async poller task
//! periodically GETs `{server}/v1/agent/config` with a bearer token and an
//! `If-None-Match` conditional, validates the body with [`crate::config`], and —
//! on a *changed*, valid `200` — atomically rewrites the on-disk config and
//! signals the agent's existing reload path (a `watch` channel selected in
//! `wait_for_signal` alongside SIGHUP). The on-disk config file stays the single
//! source of truth; reload re-reads it.
//!
//! The poller reuses the [`crate::curl`] subprocess (no HTTP/TLS dependency).
//! `curl` is a *soft* dependency: if it is missing, polling is disabled with a
//! single warning and the agent keeps running. A content-hash idempotency guard
//! breaks reload loops (an identical body is a no-op even if the server ignores
//! `If-None-Match`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

use crate::logs::source::next_backoff;

/// Backoff ceiling for consecutive poll failures (spec 007 R6).
const BACKOFF_CAP: Duration = Duration::from_secs(30 * 60);

/// Everything the poller needs from the running config. Built in `run()` and
/// re-derived on every reload, so the poller always sees the current values
/// (including ones a just-applied remote config changed).
pub struct PollerConfig {
    /// Control-plane base URL (must be `https://`).
    pub server: String,
    /// Bearer token for the control plane (also the current `api_key`).
    pub api_key: String,
    /// Current data endpoint — compared against a fetched config to log a
    /// change (spec 007 R9).
    pub endpoint: String,
    /// On-disk config path to (atomically) rewrite.
    pub config_path: PathBuf,
    /// Poll cadence.
    pub interval: Duration,
    /// Hash of the currently-applied on-disk config (spec 007 R4/R5).
    pub applied_hash: String,
}

/// A short, stable, non-secret digest of the config bytes (spec 007 R4/R5).
///
/// FNV-1a (64-bit), rendered as 16 lowercase hex chars. Deterministic across
/// process runs and Rust versions (unlike `DefaultHasher`), which R4 requires.
/// It is a digest of the whole file; only the digest — never the contents — is
/// logged or sent, so it cannot leak the `api_key`.
#[must_use]
pub fn config_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Parsed HTTP response from `curl -i` (headers + body on one stdout stream).
/// `pub(crate)` so the parse/classify pipeline is unit-testable from the
/// crate-root test sibling (visibility only — no behavior).
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) etag: Option<String>,
    pub(crate) body: Vec<u8>,
}

/// Parse a `curl -i` response: split the leading header block from the body,
/// extract the status code and `ETag`. Pure; unit-tested. Returns `None` if no
/// header/body boundary or status line is found. No redirects are followed
/// (the poller does not pass `-L`), so there is exactly one header block.
pub(crate) fn parse_http_response(raw: &[u8]) -> Option<HttpResponse> {
    let (hdr_end, sep_len) = find_subslice(raw, b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| find_subslice(raw, b"\n\n").map(|i| (i, 2)))?;

    let body = raw.get(hdr_end + sep_len..)?.to_vec();
    let header_text = String::from_utf8_lossy(&raw[..hdr_end]);
    let mut lines = header_text.lines();

    let status = parse_status_code(lines.next()?)?;
    let etag = lines
        .find(|l| l.len() >= 5 && l[..5].eq_ignore_ascii_case("etag:"))
        .map(|l| l[5..].trim().to_string());

    Some(HttpResponse { status, etag, body })
}

/// Parse the numeric status code from an HTTP status line (`HTTP/1.1 200 OK`).
fn parse_status_code(status_line: &str) -> Option<u16> {
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// Index of the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// The outcome of a single poll. `pub(crate)` for unit tests (visibility only).
pub(crate) enum PollOutcome {
    /// `304 Not Modified` — nothing to do.
    NotModified,
    /// Valid `200` whose body matches the applied config (hash equal).
    Unchanged { etag: Option<String> },
    /// Valid `200` whose body differs — apply it. The response `ETag` is not
    /// retained: applying triggers a reload that re-spawns the poller fresh, so
    /// a per-instance in-memory `ETag` from this response would be discarded.
    Changed {
        body: String,
        hash: String,
        endpoint_changed: bool,
        api_key_changed: bool,
        server_dropped: bool,
    },
    /// `200` whose body failed validation — keep the current config.
    Invalid(String),
}

/// A poll failure. `pub(crate)` for unit tests (visibility only).
pub(crate) enum PollError {
    /// `curl` binary not found — disable polling entirely (do not retry).
    CurlMissing,
    /// Transient/soft failure (network, timeout, non-200/304, empty body) —
    /// keep the current config and back off.
    Fetch(String),
}

/// Run the remote-config poller until cancelled. Never panics; a poll failure
/// only logs and retries. Returns early (disables polling for this run) when
/// the server is not `https://`, the interval is zero, or `curl` is missing.
pub async fn run_poller(
    pc: PollerConfig,
    reload_tx: tokio::sync::watch::Sender<()>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    if !pc.server.starts_with("https://") {
        warn!(
            server = %pc.server,
            "remote config requires an https:// server URL (it carries a bearer \
             token and returns authority-bearing config) — polling disabled"
        );
        return;
    }
    if pc.interval.is_zero() {
        return;
    }

    let base = pc.interval;
    let mut delay = base;
    let mut etag: Option<String> = None;
    let applied_hash = pc.applied_hash.clone();

    info!(
        server = %pc.server,
        interval = ?base,
        config_hash = %applied_hash,
        "remote config poller started"
    );

    loop {
        tokio::select! {
            _ = cancel.changed() => return,
            _ = tokio::time::sleep(delay) => {}
        }

        match poll_once(&pc, &applied_hash, etag.as_deref()).await {
            Ok(PollOutcome::NotModified) => delay = base,
            Ok(PollOutcome::Unchanged { etag: new }) => {
                if new.is_some() {
                    etag = new;
                }
                delay = base;
            }
            Ok(PollOutcome::Invalid(msg)) => {
                warn!("remote config rejected, keeping current config: {msg}");
                delay = base;
            }
            Ok(PollOutcome::Changed {
                body,
                hash,
                endpoint_changed,
                api_key_changed,
                server_dropped,
            }) => {
                log_authority_changes(endpoint_changed, api_key_changed, server_dropped, &hash);
                match write_config_atomic(&pc.config_path, body.as_bytes()) {
                    Ok(()) => {
                        info!(
                            config_hash = %hash,
                            path = %pc.config_path.display(),
                            "remote config applied — triggering reload"
                        );
                        // Reload re-spawns the poller with a fresh applied_hash
                        // (from the just-written config) and etag, so updating
                        // the loop-carried values here before returning is dead.
                        let _ = reload_tx.send(());
                        return;
                    }
                    Err(e) => {
                        warn!("failed to write remote config, keeping current config: {e}");
                        delay = base;
                    }
                }
            }
            Err(PollError::CurlMissing) => {
                warn!("curl not found — remote config polling disabled for this run");
                return;
            }
            Err(PollError::Fetch(msg)) => {
                warn!("remote config fetch failed, keeping current config: {msg}");
                delay = next_backoff(delay, BACKOFF_CAP);
            }
        }
    }
}

/// Log any credential/endpoint/server-management change a remote config makes
/// (spec 007 R9/R8) — never logging the key value itself.
fn log_authority_changes(
    endpoint_changed: bool,
    api_key_changed: bool,
    server_dropped: bool,
    hash: &str,
) {
    if endpoint_changed {
        warn!(config_hash = %hash, "remote config changed the data endpoint");
    }
    if api_key_changed {
        warn!(config_hash = %hash, "remote config changed the api_key");
    }
    if server_dropped {
        warn!(
            "remote config dropped 'server' — remote management will be disabled \
             after this reload"
        );
    }
}

/// Issue one conditional GET and classify the result.
async fn poll_once(
    pc: &PollerConfig,
    applied_hash: &str,
    etag: Option<&str>,
) -> Result<PollOutcome, PollError> {
    let url = format!("{}/v1/agent/config", pc.server.trim_end_matches('/'));
    let ua = format!("User-Agent: witness/{}", env!("CARGO_PKG_VERSION"));
    let hash_header = format!("X-Witness-Config-Hash: {applied_hash}");
    let inm = etag.map(|e| format!("If-None-Match: {e}"));
    let token = pc.api_key.clone();

    // The curl subprocess is blocking IO — run it off the async runtime.
    let spawned = tokio::task::spawn_blocking(move || {
        let mut args: Vec<&str> = vec![
            "-sS", // show errors, but no `-f`: we must read the status ourselves
            "-i",  // include response headers in stdout (for status + ETag)
            "--max-time",
            "10",
            "-H",
            &ua,
            "-H",
            &hash_header,
        ];
        if let Some(inm) = inm.as_deref() {
            args.push("-H");
            args.push(inm);
        }
        args.push("--config");
        args.push("-");
        args.push(&url);
        crate::curl::run_with_bearer(&args, &token)
    })
    .await;

    let output = match spawned {
        Ok(Ok(o)) => o,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PollError::CurlMissing);
        }
        Ok(Err(e)) => return Err(PollError::Fetch(format!("curl spawn failed: {e}"))),
        Err(e) => return Err(PollError::Fetch(format!("poll task failed: {e}"))),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PollError::Fetch(format!(
            "curl exited non-zero: {}",
            stderr.trim()
        )));
    }

    let resp = parse_http_response(&output.stdout)
        .ok_or_else(|| PollError::Fetch("unparseable HTTP response".to_string()))?;

    classify_response(pc, applied_hash, resp)
}

/// Turn a parsed `200`/`304`/other response into a [`PollOutcome`].
pub(crate) fn classify_response(
    pc: &PollerConfig,
    applied_hash: &str,
    resp: HttpResponse,
) -> Result<PollOutcome, PollError> {
    match resp.status {
        304 => Ok(PollOutcome::NotModified),
        200 => {
            if resp.body.iter().all(u8::is_ascii_whitespace) {
                return Err(PollError::Fetch(
                    "server returned an empty body".to_string(),
                ));
            }
            let body = String::from_utf8_lossy(&resp.body).into_owned();
            let new_cfg = match crate::config::parse_config(&body) {
                Ok(c) => c,
                Err(e) => return Ok(PollOutcome::Invalid(e.to_string())),
            };
            let hash = config_hash(body.as_bytes());
            if hash == applied_hash {
                return Ok(PollOutcome::Unchanged { etag: resp.etag });
            }
            Ok(PollOutcome::Changed {
                endpoint_changed: new_cfg.endpoint != pc.endpoint,
                api_key_changed: new_cfg.api_key != pc.api_key,
                server_dropped: new_cfg.server.is_none(),
                body,
                hash,
            })
        }
        other => Err(PollError::Fetch(format!("unexpected HTTP status {other}"))),
    }
}

/// Atomically write `bytes` to `path` with witness's permission conventions
/// (spec 007 R7): temp file in the same directory → write → `sync_all` → mode
/// `0640` + best-effort `chown root:witness` (Unix) → rename over `path`. The
/// rename is atomic, so no observer ever sees a partial config at the live path.
fn write_config_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("tmp-remote");

    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o640))?;
        chown_root_witness(&tmp);
    }

    std::fs::rename(&tmp, path)
}

/// Best-effort `chown root:witness` so the `User=witness` service can read the
/// config (mirrors `setup.rs`). A failure (e.g. the group does not exist, or we
/// are not root) is ignored — the mode `0640` still protects the key at rest.
#[cfg(unix)]
fn chown_root_witness(path: &Path) {
    let Ok(cpath) = std::ffi::CString::new(path.to_string_lossy().as_bytes().to_vec()) else {
        return;
    };
    let Ok(group) = std::ffi::CString::new("witness") else {
        return;
    };
    unsafe {
        let gr = libc::getgrnam(group.as_ptr());
        if !gr.is_null() {
            libc::chown(cpath.as_ptr(), 0, (*gr).gr_gid);
        }
    }
}
