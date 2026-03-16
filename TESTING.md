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

### Current coverage (macOS, 2026-03-16)

```
File                            Lines    Missed   Cover
───────────────────────────────────────────────────────
collectors/macos/cpu.rs            86         2   97.67%
collectors/macos/memory.rs         58         2   96.55%
collectors/macos/network.rs        90         4   95.56%
collectors/macos/load.rs           13         1   92.31%
collectors/macos/disk.rs           79         6   92.41%
collectors/macos/mod.rs            25         0  100.00%
collectors/mod.rs                   8         5   37.50%
config.rs                         111        71   36.04%
tail/watcher.rs                   342       342    0.00%
───────────────────────────────────────────────────────
TOTAL                             812       433   46.67%
```

### Gaps

- **tail/watcher.rs** (0%) — log tailing with file polling, rotation handling, offset persistence. Needs mock filesystem or integration harness.
- **config.rs** (36%) — TOML parsing, filter logic, hostname resolution. Straightforward to unit test.
- **collectors/mod.rs** (37.5%) — `read_procfs` helper and `init_collectors` dispatch. Linux-gated paths unreachable on macOS.
