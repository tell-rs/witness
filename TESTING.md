# Testing

## Run tests

```bash
cargo test --all
```

## Coverage

```bash
cargo llvm-cov --all --ignore-filename-regex '_test\.rs$|src/main\.rs|tests/'
```

### Exclusions

| Pattern | Reason |
|---------|--------|
| `_test\.rs$` | Test files — we measure source coverage, not test-of-tests |
| `src/main\.rs` | Binary entry point: CLI parsing, signal handling, async runtime bootstrap — integration-level, not unit-testable |
| `tests/` | Integration test harness code (mock servers, assertions) — not source under test |

Linux collectors (`src/collectors/linux/`) are excluded naturally — they're behind `#[cfg(target_os = "linux")]` and don't compile on macOS.
