mod collectors;
mod config;
mod sink;
mod tail;

use std::path::PathBuf;
use std::process;

use clap::Parser;
use tell::{Tell, TellConfig};

use crate::config::{load_config, resolve_hostname};
use crate::sink::{DryRun, Sink};

#[derive(Parser)]
#[command(name = "tell-agent", about = "Tell host monitoring agent")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "/etc/tell/agent.toml")]
    config: PathBuf,

    /// Print what would be collected without sending. Runs two collection
    /// cycles (to show delta metrics), tails logs briefly, then exits.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if cli.dry_run {
        dry_run(&cli.config).await;
        return;
    }

    // Outer loop: SIGHUP restarts with new config, zero data loss.
    loop {
        match run(&cli.config).await {
            RunResult::Shutdown => {
                eprintln!("tell-agent stopped");
                return;
            }
            RunResult::Reload => {
                eprintln!("reloading config...");
            }
        }
    }
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

    eprintln!("tell-agent dry-run (host={hostname})\n");

    // --- Metrics ---
    let mut all = collectors::init_collectors(&cfg.system);
    if all.is_empty() {
        eprintln!("no collectors available for this platform");
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

    // --- Logs ---
    if !cfg.logs.is_empty() {
        eprintln!("\n[logs]");

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

        // Actually tail for a short period to show sample lines
        let log_sink = sink.clone();
        let paths = cfg.logs.clone();
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let cancel = shutdown_tx.subscribe();

        let tailer = tokio::spawn(async move {
            tail::watcher::tail_files(&paths, log_sink, cancel).await;
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

async fn run(config_path: &PathBuf) -> RunResult {
    let cfg = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config from {}: {e}", config_path.display());
            process::exit(1);
        }
    };

    let hostname = resolve_hostname(&cfg.hostname);
    let interval = cfg.interval;

    let buffer_dir = std::path::Path::new("/var/lib/tell-agent/buffer");
    match std::fs::create_dir_all(buffer_dir) {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "WARNING: cannot create disk buffer at {}: {e}",
                buffer_dir.display()
            );
            eprintln!(
                "WARNING: data WILL BE LOST on network failures — fix permissions or run as root"
            );
        }
    }

    let mut builder = TellConfig::builder(&cfg.api_key)
        .endpoint(&cfg.endpoint)
        .service("tell-agent")
        .source(&hostname)
        .batch_size(500)
        .flush_interval(interval)
        .buffer_path(buffer_dir);

    if let Some(max_bytes) = cfg.buffer_max_bytes {
        builder = builder.buffer_max_bytes(max_bytes);
    }

    let tell_config = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("invalid config: {e}");
            process::exit(1);
        }
    };

    let client = match Tell::new(tell_config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to init client: {e}");
            process::exit(1);
        }
    };

    let sink = Sink::live(client, cfg.tags);

    eprintln!("tell-agent starting (host={hostname}, interval={interval:?})");

    let (shutdown_tx, _) = tokio::sync::watch::channel(false);

    // Spawn metric collection
    let collector_handle = {
        let s = sink.clone();
        let h = hostname.clone();
        let mut cancel = shutdown_tx.subscribe();
        let system_config = cfg.system;
        Some(tokio::spawn(async move {
            let mut collectors = collectors::init_collectors(&system_config);
            if collectors.is_empty() {
                eprintln!("no collectors available for this platform");
                return;
            }

            eprintln!(
                "collecting: {}",
                collectors
                    .iter()
                    .map(|c| c.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let mut buf = String::with_capacity(8192);
            let mut tick = tokio::time::interval(interval);
            let mut tick_count: u32 = 0;

            // 1 hour of ticks. Interval is in seconds.
            let checkpoint_ticks = 3600 / interval.as_secs().max(1) as u32;

            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let checkpoint = tick_count > 0 && tick_count % checkpoint_ticks == 0;
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
                                eprintln!("collector tick failed, reinitializing: {e}");
                                collectors = collectors::init_collectors(&system_config);
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

    // Spawn log tailing
    let tailer_handle = if !cfg.logs.is_empty() {
        let s = sink.clone();
        let paths = cfg.logs.clone();
        let cancel = shutdown_tx.subscribe();
        Some(tokio::spawn(async move {
            eprintln!("tailing: {paths:?}");
            tail::watcher::tail_files(&paths, s, cancel).await;
        }))
    } else {
        None
    };

    // Wait for signal
    let signal = wait_for_signal().await;

    // Graceful shutdown: drain everything, flush SDK
    let _ = shutdown_tx.send(true);
    if let Some(h) = collector_handle {
        let _ = h.await;
    }
    if let Some(h) = tailer_handle {
        let _ = h.await;
    }
    if let Err(e) = sink.close().await {
        eprintln!("error during shutdown: {e}");
    }

    match signal {
        Signal::Shutdown => RunResult::Shutdown,
        Signal::Reload => RunResult::Reload,
    }
}

enum Signal {
    Shutdown,
    Reload,
}

async fn wait_for_signal() -> Signal {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
            eprintln!("failed to register SIGTERM handler");
            process::exit(1);
        };
        let Ok(mut sighup) = signal(SignalKind::hangup()) else {
            eprintln!("failed to register SIGHUP handler");
            process::exit(1);
        };
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::select! {
            _ = ctrl_c => Signal::Shutdown,
            _ = sigterm.recv() => Signal::Shutdown,
            _ = sighup.recv() => Signal::Reload,
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        Signal::Shutdown
    }
}
