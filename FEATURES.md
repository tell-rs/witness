# Features

## System Metrics

- CPU utilization — user, system, idle, iowait, and steal percentages per tick.
- Memory usage — total, available, used, cached, and swap.
- Load averages — 1, 5, and 15 minute system load.
- Disk I/O and space — read/write bytes (delta), total and used bytes per device.
- Network traffic — bytes and packets sent/received per interface (delta).
- TCP connections — connection count by state (Linux only).
- Cgroups v2 — CPU and memory metrics from the cgroup hierarchy (Linux only).
- Container metrics — per-container CPU and memory from cgroup v2 scopes (Linux only).
- Top processes — CPU and memory for the top N processes by resource usage (Linux only).
- Hourly checkpoints — cumulative disk and network counters saved every hour, recovers exact totals after outages.
- Device filtering — include/exclude glob patterns for network interfaces and disk devices.

## Log Ingestion

- Log source selection — `log_source` config: `journald`, `files`, or `auto` (detect at install time).
- Journald backend — reads structured entries from systemd journal with service name, severity, and cursor-based resume.
- Journald auto-restart — reconnects with exponential backoff if journalctl exits unexpectedly.
- Journald self-filtering — witness's own log entries are excluded to prevent feedback loops.
- Syslog parsing — extracts service name (sshd, kernel, CRON, etc.) from RFC 3164 and ISO 8601 syslog lines.
- Per-entry service name — both journald and syslog-parsed file logs set the Tell protocol `service` field per line.
- Structured journald properties — application-emitted journald fields are forwarded as structured log properties, with systemd-internal fields (`_*`, `SYSLOG_FACILITY`, `SYSLOG_PID`, `SYSLOG_RAW`) filtered out.
- Inline payload parsing — logfmt (`key=value`) and embedded JSON inside log messages are extracted into structured fields alongside journald properties.
- Polling-based file tailing — 250ms base poll with adaptive backoff to 2s when idle, 50ms fast catchup under load.
- Glob pattern discovery — paths like `/var/log/nginx/*.log` re-scanned every 10 seconds for new files.
- Rotation handling — rename (logrotate) and copytruncate both supported, old file descriptors retained and drained to EOF.
- Crash-safe offsets — atomic write/fsync/rename checkpoints, duplicate window under 250ms on crash recovery.
- Backpressure — stops reading when the send channel is full without advancing offsets; the filesystem acts as overflow buffer.
- Partial line buffering — holds bytes until a newline is seen, never emits truncated lines mid-write.
- Platform-aware defaults — Linux: syslog, auth, kern, nginx, apache, postgres, mysql, mongodb, redis, haproxy, traefik, elasticsearch, rabbitmq. macOS: system.log.
- Idle eviction — files with no new data for 1 hour automatically removed from tracking.

## Transport

- Tell SDK — TCP transport to tell-rs with FlatBuffer encoding, batching, and retry.
- Disk buffer — persists unsent data during network outages, configurable max size (default 3 GiB), oldest data evicted at limit.
- Configurable batching — data points per TCP flush (default 500).
- Hostname as source — all log entries carry the hostname, metrics and logs use the same source field for consistent filtering.
- Global tags — key/value pairs applied to every metric and log entry.

## Installation

- One-line install — `curl -sSfL https://tell.rs/agent | bash` with architecture detection.
- `witness install` — full automated setup: binary to `/usr/local/bin`, system user, hardened systemd unit, config, and start.
- `witness setup` — fetches config from a Tell server or generates sensible defaults. Detects journald availability and sets `log_source` accordingly. `--offline` skips all outbound connections. Validates API key format before any network call.
- Static binaries — musl-linked releases for x86_64 and aarch64 Linux.

## Configuration

- TOML config — `/etc/witness/config.toml`, minimal config is just `api_key`, everything else has sensible defaults.
- Human-friendly durations — `15s`, `250ms`, `1m`, `1h` in config values.
- Per-collector toggles — enable or disable individual metric collectors.
- Hostname auto-detection — reads `/etc/hostname`, falls back to `gethostname()`.
- API key validation — 32 hex character format checked at load time before connecting.

## Operations

- Dry-run mode — `--dry-run` shows what would be collected without sending, respects `log_source` setting, runs two ticks to display delta metrics.
- SIGHUP reload — `systemctl reload witness` applies config changes with zero downtime and graceful drain.
- Graceful shutdown — SIGTERM/SIGINT drains collectors and log tailers, flushes the SDK, then exits cleanly.
- Collector resilience — panics in metric collection trigger reinitialization instead of crashing the process.

## Security

- Systemd hardening — ProtectSystem=strict, ProtectHome, NoNewPrivileges, MemoryDenyWriteExecute, restricted syscalls and namespaces.
- Config file permissions — setup writes config as 640 owned by root:witness.
- Dedicated system user — no-login witness user with adm group access for log file reading.

## Platform Support

- Linux — 9 collectors: CPU, memory, load, disk, network, TCP, cgroups, containers, processes.
- macOS — 5 collectors: CPU, memory, load, disk, network.
