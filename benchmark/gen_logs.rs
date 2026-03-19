//! Generate a log file with realistic nginx access log lines.
//!
//! Usage: cargo run --example gen_logs --release -- --lines 10000000 --output /tmp/bench.log

use std::io::{BufWriter, Write};

const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH"];
const PATHS: &[&str] = &[
    "/",
    "/api/users",
    "/api/orders",
    "/api/products",
    "/api/auth/login",
    "/api/auth/logout",
    "/api/search?q=analytics",
    "/dashboard",
    "/dashboard/settings",
    "/static/js/app.min.js",
    "/static/css/main.css",
    "/health",
    "/api/v2/metrics",
    "/api/v2/events",
    "/api/webhooks/stripe",
];
const STATUSES: &[u16] = &[
    200, 200, 200, 200, 200, 201, 204, 301, 304, 400, 401, 403, 404, 500,
];
const IPS: &[&str] = &[
    "10.0.1.42",
    "10.0.1.87",
    "192.168.1.100",
    "172.16.0.15",
    "10.0.2.200",
    "192.168.0.50",
    "10.10.10.1",
    "172.20.0.33",
];
const AGENTS: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0",
    "curl/8.4.0",
    "Go-http-client/2.0",
    "python-requests/2.31.0",
];

fn main() {
    let mut lines: usize = 1_000_000;
    let mut output = String::from("/tmp/bench.log");

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--lines" | "-n" => {
                i += 1;
                lines = args[i].parse().expect("invalid --lines");
            }
            "--output" | "-o" => {
                i += 1;
                output = args[i].clone();
            }
            _ => {
                eprintln!("usage: gen_logs [--lines N] [--output PATH]");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    eprintln!("generating {lines} log lines to {output}...");

    let file = std::fs::File::create(&output).expect("failed to create output file");
    let mut w = BufWriter::with_capacity(1024 * 1024, file);

    // Simple deterministic "random" using wrapping arithmetic.
    // We don't need cryptographic randomness — just variety.
    let mut state: u64 = 0xdeadbeef12345678;

    let base_ts = 1710000000u64; // 2024-03-09

    for i in 0..lines {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = state;

        let ip = IPS[(r as usize) % IPS.len()];
        let method = METHODS[((r >> 8) as usize) % METHODS.len()];
        let path = PATHS[((r >> 16) as usize) % PATHS.len()];
        let status = STATUSES[((r >> 24) as usize) % STATUSES.len()];
        let size = 200 + ((r >> 32) % 50000);
        let agent = AGENTS[((r >> 48) as usize) % AGENTS.len()];

        // Timestamp increments ~1ms per line (realistic high-traffic server)
        let ts = base_ts + (i as u64 / 1000);

        writeln!(
            w,
            "{ip} - - [{ts}] \"{method} {path} HTTP/1.1\" {status} {size} \"-\" \"{agent}\""
        )
        .expect("write failed");
    }

    w.flush().expect("flush failed");

    let meta = std::fs::metadata(&output).expect("stat failed");
    let size_mb = meta.len() as f64 / 1_048_576.0;
    let avg_line = meta.len() as f64 / lines as f64;
    eprintln!("done: {size_mb:.1} MB, {avg_line:.0} bytes/line avg");
}
