# Tell Agent

<p align="center">
  <a href="https://doc.rust-lang.org/edition-guide/rust-2024/"><img src="https://img.shields.io/badge/Rust-2024_edition-blue.svg" alt="Rust 2024"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
</p>

Host monitoring agent for [Tell](https://tell.rs), written in Rust. Collects system metrics and ships log files — every metric is disk-buffered, every log line survives rotation, and the entire agent fits in a ~2 MB static binary.

- **Fast** — 15 ns per metric, 160 ns per log line, zero steady-state allocations. Your servers stay yours.
- **Reliable** — Every metric is disk-buffered. Every log line survives rotation, truncation, and crashes. Atomic offset checkpoints, both rename and copytruncate handling.
- **Simple** — One config line to start. One binary to deploy. No JVM, no Ruby, no Python runtime.

## Use cases

- Monitor servers in production — CPU, memory, disk, network, processes, and TCP state out of the box.
- Ship log files from any server to Tell for search, alerting, and dashboards.
- Run on edge servers, IoT gateways, or containers where every MB of memory counts.
- One agent for both metrics and logs instead of running two separate tools.

## Quick Start

```bash
curl -sSL https://get.tell.rs/agent | bash -s -- --api-key YOUR_KEY
```

Verify: `systemctl status tell-agent`. Metrics and logs appear in Tell within 15 seconds.

Config at `/etc/tell/agent.toml`:

```toml
api_key = "feed1e11feed1e11feed1e11feed1e11"
endpoint = "collect.tell.rs:50000"
logs = ["/var/log/syslog", "/var/log/auth.log", "/var/log/nginx/*.log"]

[tags]
env = "production"
role = "web"
```

All system metrics are collected by default. Override only what you need — see [configs/example.toml](configs/example.toml).

## System Metrics

8 collectors reading kernel virtual files. All enabled by default, individually toggleable.

- **CPU** — per-core utilization: `system.cpu.user`, `.system`, `.idle`, `.iowait`, `.steal`
- **Memory** — `system.memory.total`, `.available`, `.used`, `.cached`, `.swap_used`
- **Load** — `system.load.1`, `.5`, `.15`
- **Disk** — I/O counters per device (`system.disk.{read,write}_{bytes,ops}`) and filesystem space per mount (`system.disk.{total,used,free}_bytes`)
- **Network** — per-interface counters: `system.net.{bytes,packets,errors,drops}_{recv,sent}`
- **TCP** — connection state counts: `system.tcp.connections` labeled by `state`
- **cgroups** — container-aware CPU and memory from cgroups v2
- **Processes** — top N processes by CPU and memory: `system.process.cpu_jiffies`, `.memory_rss`

## Logs

Ship log files to Tell. Point the agent at your log directories — it handles the rest.

```toml
logs = ["/var/log/syslog", "/var/log/auth.log", "/var/log/nginx/*.log"]
```

Glob patterns supported. New files matching the pattern are picked up automatically every 10 seconds — no restart needed.

- **Log rotation just works.** Both rename+create and copytruncate are handled correctly. The agent finds the rotated file by inode, drains remaining lines, then switches to the new file. No lines lost between rotation and discovery.
- **Survives crashes.** File offsets are checkpointed to disk every 10 seconds with atomic write + fsync. Restart picks up exactly where it left off.
- **Never ships partial lines.** Bytes are buffered until a newline delimiter arrives. A line being written mid-read won't produce garbage.
- **Stays lean.** Stale files older than 24 hours are skipped on discovery. Files that go quiet for an hour are evicted from memory.

Lines are shipped raw over TCP. Parsing, filtering, and redaction happen server-side in tell-rs [transforms](https://tell.rs/docs/transforms) — change processing rules without touching the agent.

## Performance

Agent resource overhead on a typical 8-core server:

| Resource | Typical | Notes |
|----------|---------|-------|
| **CPU** | <0.1% | Reads `/proc` every 15s, sleeps between ticks |
| **Memory** | 10-20 MB RSS | Reusable buffers, no per-tick allocations |
| **Network** | 1-5 KB/s | FlatBuffer-encoded, batched TCP. Scales with log volume |
| **Binary** | ~2 MB | Static musl build. No runtime dependencies |

Caller-side latency — time for a single SDK call to encode and enqueue into the async channel. Measures how long the calling thread is blocked per operation (Apple M4 Pro):

| Operation | Latency | Heap allocs |
|-----------|---------|-------------|
| **Metrics** | | |
| `gauge` (no labels) | **14 ns** | 0 |
| `gauge` (1 label) | **30 ns** | 0 |
| `gauge` (3 labels) | **31 ns** | 0 |
| `counter` (1 label) | **31 ns** | 0 |
| **Logs** | | |
| `log` (with structured data) | **161 ns** | 1 |

Batching and TCP delivery happen in a background worker (batch size 500, configurable flush interval).

```bash
cargo bench -p tell-bench --bench hot_path    # reproduce benchmarks
```

## Correctness

Correctness tests run against each agent on the same host. Not exhaustive — these are the behaviors that matter most for log shipping.

| Test | Tell Agent | Vector | Filebeat | FluentBit | Logstash | Splunk UF | Splunk HF | Telegraf |
|------|:----------:|:------:|:--------:|:---------:|:--------:|:---------:|:---------:|:--------:|
| Disk buffer persistence | **✓** | ✓ | ✓ | | ⚠ | ✓ | ✓ | |
| File rotate (create) | **✓** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | |
| File rotate (copytruncate) | **✓** | ✓ | | | | ✓ | ✓ | |
| File truncation | **✓** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | |
| Process (SIGHUP) | **✓** | ✓ | | | ⚠ | ✓ | ✓ | ✓ |
| Offset persistence (fsync) | **✓** | ✓ | | | | | | |
| Partial line buffering | **✓** | ✓ | | | | | | |

## Features

| | Tell Agent | Vector | Filebeat | FluentBit | Logstash | Splunk UF | Splunk HF | Telegraf |
|--|:----------:|:------:|:--------:|:---------:|:--------:|:---------:|:---------:|:--------:|
| **Data** | | | | | | | | |
| System metrics | ✓ | ✓ | ⚠ | ⚠ | ⚠ | | | ✓ |
| Log shipping | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| cgroups v2 | ✓ | ✓ | | | | | | ✓ |
| Process metrics | ✓ | ✓ | | | | | | ✓ |
| TCP state tracking | ✓ | ✓ | | | | | | ✓ |
| **Reliability** | | | | | | | | |
| Disk buffer (WAL) | ✓ | ✓ | ✓ | | ⚠ | ✓ | ✓ | |
| Log rotation handling | ✓ | ✓ | ✓ | | | ✓ | ✓ | |
| Config reload (SIGHUP) | ✓ | ✓ | | | ⚠ | ✓ | ✓ | ✓ |
| Delivery guarantees | ✓ | ✓ | | | ✓ | ✓ | ✓ | |
| **Efficiency** | | | | | | | | |
| Memory safe | ✓ | ✓ | | | | | | |
| Static binary | ✓ | ✓ | | ✓ | | | | ✓ |
| Language | Rust | Rust | Go | C | Java | C++ | C++ | Go |

⚠ = Partial support or not interoperable

## Reliability

- **Disk-buffered delivery.** When tell-rs is unreachable, encoded batches are written to a WAL at `/var/lib/tell-agent/buffer/` and retried on subsequent ticks. Oldest frames evicted at 64 MB.
- **Log rotation.** Handles rename+create (detects inode change, finds rotated file in parent directory by inode scan, drains remaining lines via retained fd) and copytruncate (detects size decrease, flushes partial line buffer, resets position).
- **Offset persistence.** File positions written atomically (write → fsync → rename) every 10 seconds. Crash → restart resumes from last checkpoint with at-least-once delivery.
- **Stale file handling.** Files unmodified for 24 hours are skipped on discovery. Files with no new data for 1 hour are evicted from memory.
- **Config reload.** `systemctl reload tell-agent` sends SIGHUP. The agent drains all queues, flushes the SDK, saves offsets, then restarts with the new config in the same process. Zero data loss.

## Uninstall

```bash
sudo systemctl disable --now tell-agent
sudo rm /usr/local/bin/tell-agent
sudo rm -rf /etc/tell /var/lib/tell-agent
```

## License

MIT
