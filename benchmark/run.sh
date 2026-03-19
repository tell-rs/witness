#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
VECTOR_DIR="$PROJECT_DIR/../vector"

# --- Build Witness ---
echo "Building witness..."
(cd "$PROJECT_DIR" && cargo build --release)

# --- Build Vector ---
if [[ ! -d "$VECTOR_DIR" ]]; then
    echo "Cloning vector..."
    git clone --depth 1 https://github.com/vectordotdev/vector.git "$VECTOR_DIR"
fi

echo "Building vector (minimal features)..."
(cd "$VECTOR_DIR" && cargo build --release --no-default-features \
    --features "sources-file,sources-demo_logs,sinks-socket,sources-host_metrics,sinks-console")

# --- Run ---
(cd "$PROJECT_DIR" && cargo run --example bench_throughput --release)
