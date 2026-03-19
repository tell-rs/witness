<p align="center">
  <a href="https://doc.rust-lang.org/edition-guide/rust-2024/"><img src="https://img.shields.io/badge/Rust-2024_edition-blue.svg" alt="Rust 2024"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
</p>

## What is Witness

Lightweight host agent that automatically forwards log files and collects system metrics. Ships data via the Tell binary protocol (FlatBuffers over TCP). Written in Rust, ~1.2 MB static binary.

- **Logs** — Automatically forwards your log files to a central collector. Handles rotation, crashes, and restarts out of the box.
- **Metrics** — CPU, memory, disk, network, load, TCP state, cgroups, and top processes. Per-core and per-device, all out of the box.
- **Reliable** — Disk-buffered delivery, atomic offset persistence, both rename and copytruncate rotation. Nothing lost.
- **Fast** — 76 ns per log line, 30 ns per metric, zero steady-state allocations. Your servers stay yours.
- **Single binary** — No garbage collector, no runtime dependencies. Memory safe by default.

## Use cases

- Ship log files from any server for search, alerting, and dashboards.
- Monitor servers in production — CPU, memory, disk, network, processes, and TCP state out of the box.
- Run on edge servers, IoT gateways, or containers where every MB of memory counts.
- One agent for both logs and metrics instead of running two separate tools.

## Quick Start

**1. Install**

```bash
curl -sSfL https://tell.rs/agent | bash
```

**2. Configure**

```bash
witness setup --token YOUR_API_KEY
```

Writes the config and starts collecting. Verify with `systemctl status witness`.

Logs and metrics start flowing within 15 seconds. To customise log paths, tags, or device filters see [configs/example.toml](configs/example.toml). For self-hosted or air-gapped installs see [INSTALL.md](INSTALL.md).

## Logs

Reads your log files and forwards every line to your collector. System logs, application output, web server access logs, database slow queries, security audit logs — anything that writes to a file on disk.

- **Works out of the box** — ships system, web server, and database logs by default. New files are discovered and tailed without restarting the agent.
- **Configurable** — point at specific paths or use glob patterns like `/var/log/nginx/*.log` to match multiple files.
- **Fast** — 76 ns per log line. Batched and shipped over TCP in the background.
- **Reliable** — survives rotation, truncation, crashes, and restarts. Every line is delivered at least once — a crash may re-send a few recent lines, but never skips any.
- **Low overhead** — stale files cleaned up, idle files evicted from memory. No babysitting.

## System Metrics

Collected every 15 seconds (configurable). All enabled by default, individually toggleable.

- **CPU** — per-core user, system, idle, iowait, and steal percentages
- **Memory** — total, available, used, cached, and swap
- **Load** — 1, 5, and 15 minute averages
- **Disk** — read/write bytes and ops per device, filesystem space per mount
- **Network** — bytes, packets, errors, and drops per interface
- **TCP** — connection counts by state
- **cgroups** — container-aware CPU and memory from cgroups v2
- **Processes** — top N by CPU and memory usage

## Performance

All data is batched and sent over a persistent TCP connection using a binary protocol — no JSON, no HTTP. Metrics are encoded directly into reusable buffers with zero heap allocations. SDK-level time to encode and enqueue a single data point, benchmarked on Apple M4 Pro:

| Operation | Example | Latency |
|---|---|---|
| Log line | application log with structured fields | **76 ns** |
| Metric (point-in-time) | memory used, disk space, CPU % | **30 ns** |
| Metric (delta) | network bytes, disk I/O since last tick | **31 ns** |

## Reliability

Agent:

- **Memory safe.** No garbage collector, no buffer overflows, no use-after-free, no data races. Unsafe is limited to system call wrappers (POSIX, Mach).
- **No panics.** A failing metric source or unreadable log file never takes down the process. All collection errors are handled with early returns, failed ticks are retried automatically.
- **Config reload.** SIGHUP drains all queues, saves offsets, and restarts with the new config. Zero data loss.

Delivery:

- **Retry with backoff.** Failed sends retry with exponential backoff before falling back to the disk buffer.
- **Disk-buffered delivery.** When the server is unreachable, batches are written to disk and drained on the next successful connection. Oldest data evicted when the buffer is full.
- **Graceful shutdown.** On SIGTERM or SIGINT, the agent drains all queues, saves log offsets, and persists unsent data to disk. Next startup drains them first.

Log shipping:

- **Rotation safe.** Handles rename+create (tracks inodes, drains the old file via retained fd before switching) and copytruncate (detects size decrease, resets position). No lines lost between rotation and discovery.
- **Truncation safe.** Detects when a file is emptied and resets to the beginning. No missed lines after the truncation point.
- **Crash recovery.** File positions saved to disk after every poll cycle. If the agent crashes, it picks up where it left off. A crash may re-send up to ~250ms of recent lines, but never skips any.
- **Partial line safe.** Incomplete lines are buffered until a newline arrives. A line being written mid-read won't produce garbage.
- **Memory bounded.** Line buffer capped at 1 MB per file. Binary or malformed files can't grow memory unbounded. Stale files evicted after 24 hours, idle files after 1 hour.
- **Backpressure.** Polling backs off exponentially when files are idle. No CPU burn on quiet servers.

Metrics:

- **Independent modules.** If one metric source fails (e.g., a permission error on disk stats), the others keep reporting. No cascading failures.
- **Counter safe.** Kernel counter wraps or resets after reboots are handled gracefully — always produces valid values, never negative numbers or crashes.
- **Hourly checkpoints.** Cumulative counter values are sent every hour alongside deltas. If data is lost during an outage, the checkpoints provide exact totals — no permanent drift.

## Witness vs Vector

| | Witness | Vector |
|---|---|---|
| Binary | **1.2 MB** | 46 MB |
| Memory (idle) | **4 MB** | 40 MB |
| Memory (active) | **7 MB** | 40 MB |

Vector was built with minimal features (file source, host metrics, socket sink).

## Test Coverage

86% line test coverage

## License

MIT
