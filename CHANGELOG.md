# Changelog

## v0.2.0

New:
- logs: structured fields extracted from logfmt and JSON payloads inside log messages
- logs: noisy SYSLOG_FACILITY, SYSLOG_PID, and SYSLOG_RAW journal fields filtered from forwarded properties
- logs: journald backend with structured service name, severity, and cursor-based resume
- logs: syslog parser extracts service name from RFC 3164 and ISO 8601 lines
- logs: application-emitted journald fields forwarded as structured log properties, with systemd-trusted fields filtered out
- config: log_source selects journald, files, or auto-detect — setup picks based on the system
- setup: --offline flag skips auto-config fetch

Fix:
- logs: source field carries hostname instead of filename
- sink: log message truncation safe on multi-byte UTF-8
- setup: silent fallback when server is unreachable

## v0.1.4

New:
- install: witness install subcommand handles full setup — binary, user, systemd, config, start
- security: hardened systemd unit with syscall filtering, kernel protection, namespace lockdown

Fix:
- config: paths moved from /etc/tell/agent.toml to /etc/witness/config.toml
- dist: example config and systemd unit consolidated into dist/

## v0.1.3

Fix:
- container: test used 68-char hex ID instead of 64, causing parse_containerd_scope to fail
- ci: explicit rustup target add for aarch64-unknown-linux-musl to fix cross-compilation build

## v0.1.2

New:
- setup: witness setup subcommand fetches config from server or generates defaults, validates token format before any network call
- config: api_key validated at load time (32 hex chars), platform-aware state directory on macOS
- config: example.toml shows all options commented out with platform-aware log path defaults
- ci: GitHub Actions for format, clippy, test, and multi-arch musl release builds
- benchmark: throughput harness with null TCP receiver and log generator for Witness vs Vector comparison

Fix:
- sink: tag slice uses Arc instead of Box::leak, no longer leaks memory on SIGHUP reload
- sink: DryRun counter is per-instance instead of global static, safe for parallel tests
- process: stale PID cleanup uses HashSet instead of Vec for O(1) contains
- config: parse_duration no longer uses unreachable!() panic path
- logs: module doc corrected from inotify/notify to polling-based design
- systemd: witness.service adds ExecReload for SIGHUP config reload

## v0.1.1

New:
- collectors: hourly cumulative checkpoints for disk and network counters, recovers exact totals after outages
- config: default log paths for system, web server, and database logs on Linux and macOS
- readme: rewrite with logs-first positioning, updated benchmarks, removed competitor comparison tables

Fix:
- main: collector task panics reinitialize instead of crashing the process
- main: signal handler registration uses graceful exit instead of expect
- tail: save offsets after every data cycle, crash duplicate window from ~10s to ~250ms

## v0.1.0

New:
- collectors: CPU, memory, disk, network, load for Linux and macOS
- collectors: TCP connection state, cgroups v2, per-container metrics, top-N processes (Linux)
- tail: log file tailing with polling, partial line buffering, and glob pattern discovery
- tail: rotation handling for both rename and copytruncate, retained fd draining
- tail: atomic offset checkpoints for crash recovery
- sink: metric and log dispatch with global tag merging, dry-run and discard modes
- config: TOML config with duration parsing, device/interface glob filters, hostname auto-detection
- main: SIGHUP config reload with graceful drain, dry-run mode with two-tick delta display
- install: one-line curl installer with systemd unit, architecture detection, config templating
