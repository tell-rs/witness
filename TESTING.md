# Testing

## Run tests

```bash
cargo test --all
```

## Test Coverage

```bash
cargo llvm-cov --all --ignore-filename-regex '_test\.rs$|src/main\.rs|tests/|benchmark/|src/setup\.rs'
```

### Exclusions

| Pattern | Reason |
|---------|--------|
| `_test\.rs$` | Test files — we measure source coverage, not test-of-tests |
| `src/main\.rs` | Binary entry point: CLI parsing, signal handling, async runtime bootstrap — integration-level, not unit-testable |
| `tests/` | Integration test harness code (mock servers, assertions) — not source under test |
| `benchmark/` | Benchmark binaries (throughput harness, log generator, null receiver) — not production code |
| `src/setup\.rs` | CLI setup utility (`witness setup`) — shells out to `curl`, not unit-testable |

Linux collectors (`src/metrics/linux/`) are excluded naturally — they're behind `#[cfg(target_os = "linux")]` and don't compile on macOS.
