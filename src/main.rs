mod install;
mod service_windows;
mod setup;

#[cfg(test)]
mod service_windows_test;

use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tell::{Tell, TellConfig};
use tracing::{error, info, warn};

use witness::config::{AgentConfig, LogSource, load_config, resolve_hostname, state_dir};
use witness::logs::MultilineOpts;
use witness::sink::{DryRun, Sink};
use witness::{config, logs, metrics, remote_config};

#[derive(Parser)]
#[command(name = "witness", about = "Witness host monitoring agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to config file. Defaults to `C:\ProgramData\witness\config.toml` on
    /// Windows, `/etc/witness/config.toml` elsewhere (spec 005 R4).
    #[cfg_attr(
        not(target_os = "windows"),
        arg(short, long, default_value = "/etc/witness/config.toml")
    )]
    #[cfg_attr(
        target_os = "windows",
        arg(short, long, default_value = r"C:\ProgramData\witness\config.toml")
    )]
    config: PathBuf,

    /// Print what would be collected without sending. Runs two collection
    /// cycles (to show delta metrics), tails logs briefly, then exits.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Full install: install the binary, register the service, configure, start
    Install(install::InstallArgs),
    /// Stop and remove the installed service (macOS)
    Uninstall(install::UninstallArgs),
    /// Show the installed service status (macOS)
    Status,
    /// Fetch configuration from a Tell server and write it to disk
    Setup(setup::SetupArgs),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Install(args)) => {
            install::run(args);
            return;
        }
        Some(Command::Uninstall(args)) => {
            install::run_uninstall(args);
            return;
        }
        Some(Command::Status) => {
            install::run_status();
            return;
        }
        Some(Command::Setup(args)) => {
            setup::run(args);
            return;
        }
        None => {}
    }

    if cli.dry_run {
        dry_run(&cli.config).await;
        return;
    }

    // On Windows, first try to run under the Service Control Manager. When the
    // process was NOT launched by the SCM (a console/dev run), the dispatcher
    // reports it and we fall through to the normal foreground run (Ctrl-C →
    // cancellation), so `witness --config ...` still works interactively
    // (spec 005 R1).
    #[cfg(target_os = "windows")]
    {
        if service_windows::dispatch(cli.config.clone()) {
            return;
        }
    }

    init_tracing();
    agent_loop(&cli.config).await;
}

/// Load config and run the agent until shutdown, reloading on SIGHUP (Unix).
///
/// Shared entry point for the normal foreground run and the Windows service
/// host (spec 005 R1).
async fn agent_loop(config_path: &PathBuf) {
    // A missing/invalid config is fatal only at startup. After that, a bad
    // reload keeps the last good config — an agent must not die because an
    // operator fat-fingered a TOML edit and sent SIGHUP.
    let mut cfg = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(path = %config_path.display(), "failed to load config: {e}");
            process::exit(1);
        }
    };

    // Outer loop: SIGHUP or a remote-config apply restarts with new config,
    // zero data loss.
    loop {
        match run(cfg.clone(), config_path).await {
            RunResult::Shutdown => {
                info!("witness stopped");
                return;
            }
            RunResult::Reload => match load_config(config_path) {
                Ok(c) => {
                    info!(config_hash = %applied_config_hash(config_path), "config reloaded");
                    cfg = c;
                }
                Err(e) => {
                    error!(
                        path = %config_path.display(),
                        "reload failed, keeping previous config: {e}"
                    );
                }
            },
        }
    }
}

/// The applied-config hash (spec 007 R4/R5): a stable, non-secret digest of the
/// on-disk config bytes. Empty digest when the file cannot be read (the poller
/// still runs; the hash is only observability). Reads the same bytes the running
/// config was parsed from.
fn applied_config_hash(config_path: &Path) -> String {
    match std::fs::read(config_path) {
        Ok(bytes) => remote_config::config_hash(&bytes),
        Err(_) => remote_config::config_hash(b""),
    }
}

/// Compile the multiline aggregation settings from config (spec 008), shared as
/// an `Arc` across the whole file-tailer. `None` disables aggregation. The
/// pattern was already validated at startup by `parse_config`; a late compile
/// failure here is logged and degrades to the single-line path rather than
/// aborting the run.
fn multiline_opts(
    pattern: Option<&str>,
    timeout_ms: u64,
    max_bytes: usize,
) -> Option<Arc<MultilineOpts>> {
    let pattern = pattern?;
    match MultilineOpts::new(pattern, timeout_ms, max_bytes) {
        Ok(opts) => Some(Arc::new(opts)),
        Err(e) => {
            warn!("invalid multiline_start_pattern ({e}) — multiline aggregation disabled");
            None
        }
    }
}

/// Structured logging to stderr, filtered via `RUST_LOG` (default `info`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Windows service tracing: route witness's own logs to a rolling file under
/// `C:\ProgramData\witness\logs\`, NEVER the Event Log, so the 004 Event Log
/// source cannot read witness back (spec 005 R5). `RUST_LOG` still applies.
#[cfg(target_os = "windows")]
fn init_windows_file_tracing() {
    use tracing_subscriber::EnvFilter;

    let dir = std::path::Path::new(install::windows_log_dir());
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("witness.log");

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(move || -> Box<dyn std::io::Write> {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(f) => Box::new(f),
                Err(_) => Box::new(std::io::sink()),
            }
        })
        .init();
}

// ---------------------------------------------------------------------------
// Dry-run mode
// ---------------------------------------------------------------------------

async fn dry_run(config_path: &PathBuf) {
    let cfg = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config from {}: {e}", config_path.display());
            process::exit(1);
        }
    };

    let hostname = resolve_hostname(&cfg.hostname);
    let dr = DryRun::new();
    let sink = Sink::dry_run(dr.clone(), cfg.tags.clone());

    eprintln!("witness dry-run (host={hostname})\n");

    // --- Metrics ---
    let mut all = metrics::init_collectors(&cfg.system);
    if all.is_empty() {
        if metrics::platform_supported() {
            // Collectors compile on this platform but are all config-disabled —
            // the macOS log-forwarder-first default (spec 003 R3).
            eprintln!("metrics: disabled (macOS default — enable collectors under [system])\n");
        } else {
            // Windows: no collectors compiled at all — log-forwarder posture,
            // metrics are not opt-in on this platform (spec 006 R3). Kept
            // distinct from the genuinely-unsupported-platform message.
            #[cfg(target_os = "windows")]
            eprintln!(
                "metrics: no collectors on Windows (log-forwarder posture — the Event \
                 Log is the source)\n"
            );
            #[cfg(not(target_os = "windows"))]
            eprintln!("no collectors available for this platform");
        }
    } else {
        eprintln!(
            "collectors: {}\n",
            all.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
        );

        let mut buf = String::with_capacity(8192);

        // Tick 1: silent baseline for delta collectors (CPU, network)
        for col in &mut all {
            col.collect(&Sink::discard(), &hostname, &mut buf);
        }

        // Short pause so CPU ticks and network counters change
        eprintln!("[collecting — baseline, waiting 1s]\n");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Tick 2: real values — this is what every 15s tick would send
        eprintln!("[metrics]");
        for col in &mut all {
            col.collect(&sink, &hostname, &mut buf);
        }
    }

    // --- Remote config (spec 007 R10) ---
    match &cfg.server {
        Some(server) if !cfg.config_poll_interval.is_zero() => {
            eprintln!("\n[remote config — enabled]");
            eprintln!("  server: {server}");
            eprintln!("  interval: {:?}", cfg.config_poll_interval);
            if !server.starts_with("https://") {
                eprintln!("  WARNING: server is not https:// — the poller will refuse to run");
            }
        }
        _ => eprintln!("\n[remote config — disabled]"),
    }

    // --- Logs ---
    let resolved = resolve_log_source(cfg.log_source);

    let files_fallback = matches!(resolved, Ok(ResolvedLog::Files));

    #[cfg(target_os = "windows")]
    let is_eventlog = matches!(resolved, Ok(ResolvedLog::EventLog));
    #[cfg(not(target_os = "windows"))]
    let is_eventlog = false;

    if let Err(msg) = &resolved {
        eprintln!("\n[logs — ERROR: {msg}]");
    } else if is_eventlog {
        #[cfg(target_os = "windows")]
        {
            let query = logs::eventlog_parse::effective_query(cfg.eventlog_query.as_deref());
            eprintln!("\n[logs — Windows Event Log]");
            eprintln!("  channels: {}", cfg.eventlog_channels.join(", "));
            eprintln!("  query: {query}");
            if let Some(ids) = &cfg.eventlog_event_ids {
                eprintln!("  event_ids: {ids}");
            }
            if !cfg.eventlog_exclude_providers.is_empty() {
                eprintln!(
                    "  exclude_providers: {}",
                    cfg.eventlog_exclude_providers.join(", ")
                );
            }
            let log_sink = sink.clone();
            let channels = cfg.eventlog_channels.clone();
            let query_owned = cfg.eventlog_query.clone();
            let event_ids = cfg.eventlog_event_ids.clone();
            let exclude_providers = cfg.eventlog_exclude_providers.clone();
            let (shutdown_tx, _) = tokio::sync::watch::channel(false);
            let cancel = shutdown_tx.subscribe();
            let tailer = tokio::spawn(async move {
                logs::tail_eventlog(
                    log_sink,
                    cancel,
                    channels,
                    query_owned,
                    event_ids,
                    exclude_providers,
                )
                .await;
            });
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = shutdown_tx.send(true);
            let _ = tailer.await;
        }
    } else if matches!(resolved, Ok(ResolvedLog::Journald)) {
        eprintln!("\n[logs — journald]");
        if !cfg.journal_include_services.is_empty() {
            eprintln!(
                "  include_services: {}",
                cfg.journal_include_services.join(", ")
            );
        }
        if !cfg.journal_exclude_services.is_empty() {
            eprintln!(
                "  exclude_services: {}",
                cfg.journal_exclude_services.join(", ")
            );
        }
        let log_sink = sink.clone();
        let filter = journal_filter(&cfg);
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let cancel = shutdown_tx.subscribe();
        let tailer = tokio::spawn(async move {
            logs::tail_journal(log_sink, cancel, filter).await;
        });
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let _ = shutdown_tx.send(true);
        let _ = tailer.await;
    } else if matches!(resolved, Ok(ResolvedLog::Unified)) {
        let predicate = logs::unified::effective_predicate(cfg.unified_log_predicate.as_deref());
        eprintln!("\n[logs — unified log]");
        eprintln!("  predicate: {predicate}");
        eprintln!(
            "  checkpoint: {}",
            logs::unified::checkpoint_path().display()
        );
        let log_sink = sink.clone();
        let pred_owned = cfg.unified_log_predicate.clone();
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let cancel = shutdown_tx.subscribe();
        let tailer = tokio::spawn(async move {
            logs::tail_unified_log(log_sink, cancel, pred_owned).await;
        });
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let _ = shutdown_tx.send(true);
        let _ = tailer.await;
    } else if files_fallback && !cfg.logs.is_empty() {
        eprintln!("\n[logs — files]");

        // Check each configured path and report status
        for pattern in &cfg.logs {
            let matches: Vec<_> = glob::glob(pattern)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .collect();

            if matches.is_empty() {
                eprintln!("  {pattern} — not found");
                continue;
            }

            for path in &matches {
                match std::fs::metadata(path) {
                    Ok(meta) => {
                        let age = meta
                            .modified()
                            .ok()
                            .and_then(|m| std::time::SystemTime::now().duration_since(m).ok());
                        match age {
                            Some(d) if d.as_secs() > 86400 => {
                                let days = d.as_secs() / 86400;
                                eprintln!(
                                    "  {} — skipped (not modified in {days} day{})",
                                    path.display(),
                                    if days == 1 { "" } else { "s" }
                                );
                            }
                            _ => {
                                eprintln!("  {} — tailing", path.display());
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  {} — {e}", path.display());
                    }
                }
            }
        }

        if let Some(pattern) = &cfg.multiline_start_pattern {
            eprintln!(
                "  multiline: start_pattern={pattern:?} timeout={}ms",
                cfg.multiline_timeout_ms
            );
        }

        // Actually tail for a short period to show sample lines
        let log_sink = sink.clone();
        let paths = cfg.logs.clone();
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let cancel = shutdown_tx.subscribe();
        let opts = file_parse_opts(&cfg);
        let ml = multiline_opts(
            cfg.multiline_start_pattern.as_deref(),
            cfg.multiline_timeout_ms,
            cfg.multiline_max_bytes,
        );
        let tailer = tokio::spawn(async move {
            logs::tail_files(&paths, log_sink, cancel, opts, ml).await;
        });
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let _ = shutdown_tx.send(true);
        let _ = tailer.await;
    } else {
        eprintln!("\n[logs — none configured]");
    }

    eprintln!(
        "\ndry-run complete: {} data points would be sent per tick",
        dr.count()
    );
}

// ---------------------------------------------------------------------------
// Live mode
// ---------------------------------------------------------------------------

enum RunResult {
    Shutdown,
    Reload,
}

/// A concrete log backend, resolved from the configured [`LogSource`].
enum ResolvedLog {
    Journald,
    Unified,
    /// Windows Event Log (`Evt*` pull pump). Only constructed on Windows.
    #[cfg(target_os = "windows")]
    EventLog,
    Files,
}

/// Resolve the configured log source to a concrete backend.
///
/// Returns `Err` (a clear message) for an explicitly-selected source that is
/// unavailable on this host — e.g. `journald` without `journalctl`, or
/// `unifiedlog` off macOS. `auto` never errors; it falls back to file tailing.
fn resolve_log_source(source: LogSource) -> Result<ResolvedLog, String> {
    match source {
        LogSource::Journald => {
            if logs::journal::is_available() {
                Ok(ResolvedLog::Journald)
            } else {
                Err("log_source = \"journald\" but journalctl is not available".into())
            }
        }
        LogSource::UnifiedLog => {
            if logs::unified::is_available() {
                Ok(ResolvedLog::Unified)
            } else {
                Err(
                    "log_source = \"unifiedlog\" but /usr/bin/log is not available \
                     (the unified log source requires macOS)"
                        .into(),
                )
            }
        }
        LogSource::EventLog => {
            #[cfg(target_os = "windows")]
            {
                Ok(ResolvedLog::EventLog)
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(
                    "log_source = \"eventlog\" but the Windows Event Log source \
                     requires Windows"
                        .into(),
                )
            }
        }
        LogSource::Files => Ok(ResolvedLog::Files),
        LogSource::Auto => Ok(resolve_auto()),
    }
}

/// Platform-specific `auto` resolution: unified log on macOS, journald on
/// Linux, file tailing where neither is available.
fn resolve_auto() -> ResolvedLog {
    #[cfg(target_os = "macos")]
    {
        if logs::unified::is_available() {
            return ResolvedLog::Unified;
        }
        ResolvedLog::Files
    }
    #[cfg(target_os = "linux")]
    {
        if logs::journal::is_available() {
            return ResolvedLog::Journald;
        }
        ResolvedLog::Files
    }
    // Windows: the Event Log is the primary source (always available), the
    // analogue of macOS `auto` → unified log (spec 004 R6).
    #[cfg(target_os = "windows")]
    {
        ResolvedLog::EventLog
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        ResolvedLog::Files
    }
}

/// Build the file-tailer parse options from config.
fn file_parse_opts(cfg: &AgentConfig) -> logs::FileParseOpts {
    logs::FileParseOpts {
        syslog: cfg.parse_syslog,
        structured: cfg.parse_structured,
        levels: cfg.detect_levels,
    }
}

/// Build the journald service include/exclude filter from config.
fn journal_filter(cfg: &AgentConfig) -> logs::journal::ServiceFilter {
    logs::journal::ServiceFilter {
        include: cfg.journal_include_services.clone(),
        exclude: cfg.journal_exclude_services.clone(),
    }
}

async fn run(cfg: AgentConfig, config_path: &Path) -> RunResult {
    let hostname = resolve_hostname(&cfg.hostname);
    let interval = cfg.interval;
    let applied_hash = applied_config_hash(config_path);

    let buffer_dir = std::path::Path::new(state_dir()).join("buffer");
    match std::fs::create_dir_all(&buffer_dir) {
        Ok(()) => {}
        Err(e) => {
            warn!(
                path = %buffer_dir.display(),
                "cannot create disk buffer: {e} — data WILL BE LOST on network \
                 failures; fix permissions or run as root"
            );
        }
    }

    let mut builder = TellConfig::builder(&cfg.api_key)
        .endpoint(&cfg.endpoint)
        .service("witness")
        .source(&hostname)
        .batch_size(cfg.batch_size)
        .flush_interval(interval)
        .buffer_path(buffer_dir);

    if let Some(max_bytes) = cfg.buffer_max_bytes {
        builder = builder.buffer_max_bytes(max_bytes);
    }

    let tell_config = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            error!("invalid config: {e}");
            process::exit(1);
        }
    };

    let client = match Tell::new(tell_config) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to init client: {e}");
            process::exit(1);
        }
    };

    let sink = Sink::live(client, cfg.tags);

    info!(host = %hostname, ?interval, config_hash = %applied_hash, "witness starting");

    let (shutdown_tx, _) = tokio::sync::watch::channel(false);

    // Internal reload channel (spec 007 R8): the remote-config poller signals it
    // after a successful atomic write; `wait_for_signal` selects on it alongside
    // SIGHUP so remote reload works on every platform (Windows has no SIGHUP).
    let (reload_tx, reload_rx) = tokio::sync::watch::channel(());

    // Spawn the remote-config poller when a control-plane server is configured
    // and polling is enabled (spec 007 R1). `cfg.server` is passed verbatim; the
    // poller itself rejects non-https:// URLs (R2/R3).
    let poller_handle = match &cfg.server {
        Some(server) if !cfg.config_poll_interval.is_zero() => {
            let pc = remote_config::PollerConfig {
                server: server.clone(),
                api_key: cfg.api_key.clone(),
                endpoint: cfg.endpoint.clone(),
                config_path: config_path.to_path_buf(),
                interval: cfg.config_poll_interval,
                applied_hash: applied_hash.clone(),
            };
            let reload = reload_tx.clone();
            let cancel = shutdown_tx.subscribe();
            Some(tokio::spawn(remote_config::run_poller(pc, reload, cancel)))
        }
        _ => None,
    };

    // Spawn metric collection
    let collector_handle = {
        let s = sink.clone();
        let h = hostname.clone();
        let mut cancel = shutdown_tx.subscribe();
        let system_config = cfg.system;
        Some(tokio::spawn(async move {
            let mut collectors = metrics::init_collectors(&system_config);
            if collectors.is_empty() {
                warn!("no collectors available for this platform");
                return;
            }

            info!(
                collectors = %collectors
                    .iter()
                    .map(|c| c.name())
                    .collect::<Vec<_>>()
                    .join(","),
                "collecting"
            );

            let mut buf = String::with_capacity(8192);
            let mut tick = tokio::time::interval(interval);
            let mut tick_count: u32 = 0;

            // 1 hour of ticks. Interval is in seconds.
            let checkpoint_ticks = 3600 / interval.as_secs().max(1) as u32;

            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let checkpoint = tick_count > 0 && tick_count.is_multiple_of(checkpoint_ticks);
                        match tokio::task::spawn_blocking({
                            let sink = s.clone();
                            let hostname = h.clone();
                            move || {
                                for col in &mut collectors {
                                    col.collect(&sink, &hostname, &mut buf);
                                    if checkpoint {
                                        col.checkpoint(&sink, &hostname);
                                    }
                                }
                                (collectors, buf)
                            }
                        })
                        .await
                        {
                            Ok((ret_c, ret_b)) => {
                                collectors = ret_c;
                                buf = ret_b;
                            }
                            Err(e) => {
                                warn!("collector tick failed, reinitializing: {e}");
                                collectors = metrics::init_collectors(&system_config);
                                buf = String::with_capacity(8192);
                            }
                        }
                        tick_count = tick_count.wrapping_add(1);
                    }
                    _ = cancel.changed() => return,
                }
            }
        }))
    };

    // Spawn log source (journald, unified log, or file tailing)
    let tailer_handle = match resolve_log_source(cfg.log_source) {
        Err(msg) => {
            error!("{msg}");
            process::exit(1);
        }
        Ok(ResolvedLog::Journald) => {
            let s = sink.clone();
            let cancel = shutdown_tx.subscribe();
            // Field access (not `&cfg`) — `cfg.system` was already moved into
            // the collector task above, so a whole-struct borrow is disallowed.
            let filter = logs::journal::ServiceFilter {
                include: cfg.journal_include_services.clone(),
                exclude: cfg.journal_exclude_services.clone(),
            };
            Some(tokio::spawn(async move {
                info!("log source: journald");
                logs::tail_journal(s, cancel, filter).await;
            }))
        }
        Ok(ResolvedLog::Unified) => {
            let s = sink.clone();
            let cancel = shutdown_tx.subscribe();
            let predicate = cfg.unified_log_predicate.clone();
            Some(tokio::spawn(async move {
                info!("log source: unified log");
                logs::tail_unified_log(s, cancel, predicate).await;
            }))
        }
        #[cfg(target_os = "windows")]
        Ok(ResolvedLog::EventLog) => {
            let s = sink.clone();
            let cancel = shutdown_tx.subscribe();
            let channels = cfg.eventlog_channels.clone();
            let query = cfg.eventlog_query.clone();
            let event_ids = cfg.eventlog_event_ids.clone();
            let exclude_providers = cfg.eventlog_exclude_providers.clone();
            Some(tokio::spawn(async move {
                info!("log source: Windows Event Log");
                logs::tail_eventlog(s, cancel, channels, query, event_ids, exclude_providers).await;
            }))
        }
        Ok(ResolvedLog::Files) if !cfg.logs.is_empty() => {
            let s = sink.clone();
            let paths = cfg.logs.clone();
            let cancel = shutdown_tx.subscribe();
            // Field access (not `&cfg`) — `cfg.system` was already moved into
            // the collector task above, so a whole-struct borrow is disallowed.
            let opts = logs::FileParseOpts {
                syslog: cfg.parse_syslog,
                structured: cfg.parse_structured,
                levels: cfg.detect_levels,
            };
            let ml = multiline_opts(
                cfg.multiline_start_pattern.as_deref(),
                cfg.multiline_timeout_ms,
                cfg.multiline_max_bytes,
            );
            Some(tokio::spawn(async move {
                info!(?paths, "log source: files");
                logs::tail_files(&paths, s, cancel, opts, ml).await;
            }))
        }
        Ok(ResolvedLog::Files) => None,
    };

    // Wait for a shutdown signal or a reload (SIGHUP or the poller channel).
    let signal = wait_for_signal(reload_rx).await;

    // Graceful shutdown: drain everything, flush SDK
    let _ = shutdown_tx.send(true);
    if let Some(h) = collector_handle {
        let _ = h.await;
    }
    if let Some(h) = tailer_handle {
        let _ = h.await;
    }
    // The poller returns on the shutdown signal it subscribes to (and on reload
    // it has already returned after sending). Abort so a shutdown mid-poll is
    // not blocked on an in-flight curl; the atomic write leaves no partial file.
    if let Some(h) = poller_handle {
        h.abort();
    }
    if let Err(e) = sink.close().await {
        error!("error during shutdown: {e}");
    }

    match signal {
        Signal::Shutdown => RunResult::Shutdown,
        Signal::Reload => RunResult::Reload,
    }
}

enum Signal {
    Shutdown,
    /// Reload: SIGHUP (Unix) or the internal remote-config poller channel (all
    /// platforms — this is what makes remote reload reachable on Windows).
    Reload,
}

/// Wait for a shutdown or reload trigger. `reload_rx` fires when the
/// remote-config poller has written a new config (spec 007 R8); it is selected
/// alongside the platform's shutdown/SIGHUP handling so remote reload works even
/// where there is no `SIGHUP` (Windows).
async fn wait_for_signal(mut reload_rx: tokio::sync::watch::Receiver<()>) -> Signal {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
            error!("failed to register SIGTERM handler");
            process::exit(1);
        };
        let Ok(mut sighup) = signal(SignalKind::hangup()) else {
            error!("failed to register SIGHUP handler");
            process::exit(1);
        };
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::select! {
            _ = ctrl_c => Signal::Shutdown,
            _ = sigterm.recv() => Signal::Shutdown,
            _ = sighup.recv() => Signal::Reload,
            _ = reload_rx.changed() => Signal::Reload,
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Under the SCM, a Stop/Shutdown control flips the service watch; a
        // console run stops on Ctrl-C. Either maps to the drain-and-exit path
        // (spec 005 R1). A remote-config apply flips `reload_rx`.
        if let Some(mut rx) = service_windows::shutdown_receiver() {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => Signal::Shutdown,
                _ = rx.changed() => Signal::Shutdown,
                _ = reload_rx.changed() => Signal::Reload,
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => Signal::Shutdown,
                _ = reload_rx.changed() => Signal::Reload,
            }
        }
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => Signal::Shutdown,
            _ = reload_rx.changed() => Signal::Reload,
        }
    }
}
