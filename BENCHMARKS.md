# Benchmarks — Witness vs Vector

Identical methodology. Same null TCP receiver (Rust), same data, same timing. Apple M4 Pro.

## How to reproduce

```bash
./benchmark/run.sh
```

Clones Vector into `../vector` if not present, builds both with release optimizations, and runs the benchmark. If Vector is already built, it reuses it.

## What it measures

### Logs

Ships 5M nginx log lines (~84 B/line avg) from a file to a null TCP receiver on localhost.

- **Throughput** — lines/sec, measured as 5M / (last_byte_time - first_byte_time) at the receiver
- **Wire size** — bytes/line, total wire bytes / 5M lines

### Metrics

Both tools collect host metrics at 5-second intervals for 60 seconds. Not directly comparable — Witness emits aggregated metrics (e.g., CPU percentages per core), Vector emits raw counters per CPU state per core (~50x more individual events for the same system).

## Test configuration

| | Witness | Vector |
|---|---|---|
| **Log encoding** | FlatBuffers (binary) | JSON (structured) |
| **Log framing** | 4-byte length prefix | Newline delimited |
| **Log batch size** | 500 (configurable) | 1,000 (hardcoded internal) |
| **Metric encoding** | FlatBuffers (binary) | JSON (structured) |
| **Metric interval** | 5 seconds | 5 seconds |
| **Transport** | Persistent TCP, TCP_NODELAY | Persistent TCP |
| **Vector build** | — | Minimal: `sources-file`, `sources-host_metrics`, `sinks-socket`, `sinks-console` |

## Source code

- Benchmark binary: [`benchmark/bench_throughput.rs`](benchmark/bench_throughput.rs)
- Null TCP receiver: [`benchmark/null_receiver.rs`](benchmark/null_receiver.rs)
- Log generator: [`benchmark/gen_logs.rs`](benchmark/gen_logs.rs)
