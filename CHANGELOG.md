# Changelog

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
