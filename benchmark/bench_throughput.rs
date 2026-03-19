//! Throughput benchmark: witness vs Vector.
//!
//! Measures log shipping and metric collection for both tools using identical
//! methodology — same null TCP receiver, same data, same timing approach.
//!
//! Usage:
//!   cargo run --example bench_throughput --release

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::process::Command;

// --- Config ---

const LOG_LINES: usize = 5_000_000;
const METRIC_DURATION_SECS: u64 = 60;
const METRIC_INTERVAL_SECS: u64 = 5;
const RECEIVER_PORT: u16 = 9999;
const SILENCE_TIMEOUT: Duration = Duration::from_secs(3);

// --- Receiver ---

struct ReceiverStats {
    total_bytes: Arc<AtomicU64>,
    line_count: Arc<AtomicU64>,
    first_byte_ms: Arc<AtomicU64>,
    last_byte_ms: Arc<AtomicU64>,
    start: Instant,
}

struct ReceiverResult {
    total_bytes: u64,
    first_byte: Option<Instant>,
    last_byte: Option<Instant>,
    line_count: u64,
}

impl ReceiverStats {
    fn snapshot(&self) -> ReceiverResult {
        let fb_ms = self.first_byte_ms.load(Ordering::Relaxed);
        let lb_ms = self.last_byte_ms.load(Ordering::Relaxed);
        ReceiverResult {
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            first_byte: if fb_ms > 1 {
                Some(self.start + Duration::from_millis(fb_ms))
            } else {
                None
            },
            last_byte: if lb_ms > 0 {
                Some(self.start + Duration::from_millis(lb_ms))
            } else {
                None
            },
            line_count: self.line_count.load(Ordering::Relaxed),
        }
    }
}

fn start_receiver(listener: TcpListener) -> (ReceiverStats, tokio::task::JoinHandle<()>) {
    let stats = ReceiverStats {
        total_bytes: Arc::new(AtomicU64::new(0)),
        line_count: Arc::new(AtomicU64::new(0)),
        first_byte_ms: Arc::new(AtomicU64::new(0)),
        last_byte_ms: Arc::new(AtomicU64::new(0)),
        start: Instant::now(),
    };

    let tb = stats.total_bytes.clone();
    let lc = stats.line_count.clone();
    let fb = stats.first_byte_ms.clone();
    let lb = stats.last_byte_ms.clone();
    let start = stats.start;

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let tb = tb.clone();
            let lc = lc.clone();
            let fb = fb.clone();
            let lb = lb.clone();

            tokio::spawn(async move {
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let now = start.elapsed().as_millis() as u64;
                            let _ = fb.compare_exchange(
                                0,
                                now.max(1),
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                            );
                            lb.store(now, Ordering::Relaxed);
                            tb.fetch_add(n as u64, Ordering::Relaxed);

                            let newlines = buf[..n].iter().filter(|&&b| b == b'\n').count();
                            lc.fetch_add(newlines as u64, Ordering::Relaxed);
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });

    (stats, handle)
}

// --- Log file generation ---

fn generate_log_file(path: &Path, lines: usize) {
    eprint!("  generating {lines} log lines... ");
    let file = std::fs::File::create(path).expect("create log file");
    let mut w = std::io::BufWriter::with_capacity(1024 * 1024, file);

    let methods = ["GET", "POST", "PUT", "DELETE"];
    let paths = [
        "/api/users",
        "/api/orders",
        "/api/products",
        "/api/auth/login",
        "/dashboard",
        "/health",
        "/static/js/app.min.js",
    ];
    let statuses = [200u16, 200, 200, 201, 301, 400, 404, 500];
    let ips = ["10.0.1.42", "10.0.1.87", "192.168.1.100", "172.16.0.15"];

    let mut state: u64 = 0xdeadbeef12345678;

    for i in 0..lines {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = state;
        let ip = ips[(r as usize) % ips.len()];
        let method = methods[((r >> 8) as usize) % methods.len()];
        let p = paths[((r >> 16) as usize) % paths.len()];
        let status = statuses[((r >> 24) as usize) % statuses.len()];
        let size = 200 + ((r >> 32) % 50000);
        let ts = 1710000000 + (i as u64 / 1000);
        let _ = writeln!(
            w,
            "{ip} - - [{ts}] \"{method} {p} HTTP/1.1\" {status} {size} \"-\" \"bench/1.0\""
        );
    }
    w.flush().expect("flush");
    let meta = std::fs::metadata(path).expect("stat");
    let mb = meta.len() as f64 / 1_048_576.0;
    let avg = meta.len() as f64 / lines as f64;
    eprintln!("{mb:.1} MB ({avg:.0} B/line avg)");
}

// --- Config generation ---

fn write_witness_log_config(path: &Path, log_file: &Path) {
    let config = format!(
        r#"api_key = "feed1e11feed1e11feed1e11feed1e11"
endpoint = "127.0.0.1:{RECEIVER_PORT}"
hostname = "bench"
interval = "60s"
batch_size = 500
logs = ["{log_path}"]

[system]
cpu = false
memory = false
load = false
disk = false
network = false
tcp = false
cgroups = false
containers = false
processes = false
"#,
        log_path = log_file.display()
    );
    std::fs::write(path, config).expect("write tell config");
}

fn write_witness_metrics_config(path: &Path) {
    let config = format!(
        r#"api_key = "feed1e11feed1e11feed1e11feed1e11"
endpoint = "127.0.0.1:{RECEIVER_PORT}"
hostname = "bench"
interval = "{METRIC_INTERVAL_SECS}s"
batch_size = 500
logs = []
"#
    );
    std::fs::write(path, config).expect("write tell metrics config");
}

fn write_vector_log_config(path: &Path, log_file: &Path, data_dir: &Path) {
    let config = format!(
        r#"data_dir = "{data_dir}"

[sources.file_in]
type = "file"
include = ["{log_path}"]
read_from = "beginning"

[sinks.tcp_out]
type = "socket"
inputs = ["file_in"]
mode = "tcp"
address = "127.0.0.1:{RECEIVER_PORT}"

[sinks.tcp_out.encoding]
codec = "json"
"#,
        data_dir = data_dir.display(),
        log_path = log_file.display(),
    );
    std::fs::write(path, config).expect("write vector config");
}

fn write_vector_metrics_config(path: &Path, data_dir: &Path) {
    let config = format!(
        r#"data_dir = "{data_dir}"

[sources.host]
type = "host_metrics"
scrape_interval_secs = {METRIC_INTERVAL_SECS}

[sinks.tcp_out]
type = "socket"
inputs = ["host"]
mode = "tcp"
address = "127.0.0.1:{RECEIVER_PORT}"

[sinks.tcp_out.encoding]
codec = "json"
"#,
        data_dir = data_dir.display(),
    );
    std::fs::write(path, config).expect("write vector metrics config");
}

// --- Benchmark runners ---

/// Wait until the receiver has had no new bytes for SILENCE_TIMEOUT, or max_wait expires.
async fn wait_for_silence(stats: &ReceiverStats, max_wait: Duration) {
    let deadline = Instant::now() + max_wait;
    let mut last_bytes = 0u64;
    let mut silence_since = Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;

        let current = stats.total_bytes.load(Ordering::Relaxed);
        if current != last_bytes {
            last_bytes = current;
            silence_since = Instant::now();
        } else if current > 0 && silence_since.elapsed() >= SILENCE_TIMEOUT {
            break; // data flowed and then stopped
        }

        if Instant::now() >= deadline {
            break;
        }
    }
}

async fn bench_logs(witness: &Path, vector: Option<&Path>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let log_source = tmp.path().join("source.log");
    let log_watched = tmp.path().join("watched.log");

    generate_log_file(&log_source, LOG_LINES);

    println!();
    println!("=== Log Shipping ({} lines) ===", format_count(LOG_LINES));
    println!();
    println!(
        "  {:>14}  {:>12}  {:>10}  {:>8}",
        "", "lines/sec", "ns/line", "B/line"
    );
    println!(
        "  {:>14}  {:>12}  {:>10}  {:>8}",
        "", "---------", "-------", "------"
    );

    // --- witness ---
    {
        std::fs::write(&log_watched, b"").expect("create empty watched file");
        let config_path = tmp.path().join("tell.toml");
        write_witness_log_config(&config_path, &log_watched);

        let listener = TcpListener::bind(format!("127.0.0.1:{RECEIVER_PORT}"))
            .await
            .expect("bind");
        let (stats, recv_handle) = start_receiver(listener);

        let mut child = Command::new(witness)
            .args(["--config", config_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn witness");

        // Wait for agent to start and discover the empty file
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Append log data — tailer reads from pos=0
        std::fs::copy(&log_source, &log_watched).expect("copy log data");

        // Wait for all data to flow through
        wait_for_silence(&stats, Duration::from_secs(120)).await;

        child.kill().await.ok();
        child.wait().await.ok();
        recv_handle.abort();

        print_log_result("witness", &stats.snapshot(), LOG_LINES);
        let _ = std::fs::remove_file(&log_watched);
    }

    // --- Vector ---
    if let Some(vector_bin) = vector {
        let vector_data = tmp.path().join("vector-data");
        std::fs::create_dir_all(&vector_data).expect("create vector data dir");
        std::fs::copy(&log_source, &log_watched).expect("copy for vector");
        let config_path = tmp.path().join("vector.toml");
        write_vector_log_config(&config_path, &log_watched, &vector_data);

        let listener = TcpListener::bind(format!("127.0.0.1:{RECEIVER_PORT}"))
            .await
            .expect("bind");
        let (stats, recv_handle) = start_receiver(listener);

        let mut child = Command::new(vector_bin)
            .args(["--config", config_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn vector");

        wait_for_silence(&stats, Duration::from_secs(120)).await;

        child.kill().await.ok();
        child.wait().await.ok();
        recv_handle.abort();

        print_log_result("vector", &stats.snapshot(), LOG_LINES);
        let _ = std::fs::remove_file(&log_watched);
    }
}

async fn bench_metrics(witness: &Path, vector: Option<&Path>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let duration = Duration::from_secs(METRIC_DURATION_SECS);
    let expected_ticks = (METRIC_DURATION_SECS / METRIC_INTERVAL_SECS).saturating_sub(1); // first tick is baseline

    println!();
    println!(
        "=== Metric Collection ({METRIC_DURATION_SECS}s, {METRIC_INTERVAL_SECS}s ticks, ~{expected_ticks} real ticks) ==="
    );
    println!();
    println!(
        "  {:>14}  {:>10}  {:>12}  {:>10}",
        "", "metrics", "bytes/tick", "ns/metric"
    );
    println!(
        "  {:>14}  {:>10}  {:>12}  {:>10}",
        "", "-------", "----------", "---------"
    );

    // --- witness ---
    {
        let config_path = tmp.path().join("tell-metrics.toml");
        write_witness_metrics_config(&config_path);

        let listener = TcpListener::bind(format!("127.0.0.1:{RECEIVER_PORT}"))
            .await
            .expect("bind");
        let (stats, recv_handle) = start_receiver(listener);

        let mut child = Command::new(witness)
            .args(["--config", config_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn witness");

        tokio::time::sleep(duration + Duration::from_secs(5)).await;

        child.kill().await.ok();
        child.wait().await.ok();
        recv_handle.abort();

        print_metric_result("witness", &stats.snapshot(), expected_ticks);
    }

    // --- Vector ---
    if let Some(vector_bin) = vector {
        let vector_data = tmp.path().join("vector-data");
        std::fs::create_dir_all(&vector_data).expect("create vector data dir");
        let config_path = tmp.path().join("vector-metrics.toml");
        write_vector_metrics_config(&config_path, &vector_data);

        let listener = TcpListener::bind(format!("127.0.0.1:{RECEIVER_PORT}"))
            .await
            .expect("bind");
        let (stats, recv_handle) = start_receiver(listener);

        let mut child = Command::new(vector_bin)
            .args(["--config", config_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn vector");

        tokio::time::sleep(duration + Duration::from_secs(5)).await;

        child.kill().await.ok();
        child.wait().await.ok();
        recv_handle.abort();

        print_metric_result("vector", &stats.snapshot(), expected_ticks);
    }

    println!();
}

// --- Output formatting ---

fn print_log_result(name: &str, r: &ReceiverResult, expected_lines: usize) {
    let elapsed = match (r.first_byte, r.last_byte) {
        (Some(f), Some(l)) => l.duration_since(f),
        _ => {
            println!("  {name:>14}  NO DATA RECEIVED");
            return;
        }
    };

    let secs = elapsed.as_secs_f64();
    if secs < 0.001 {
        println!("  {name:>14}  elapsed too small to measure");
        return;
    }

    let lines_sec = expected_lines as f64 / secs;
    let ns_line = (elapsed.as_nanos() as f64) / expected_lines as f64;
    let bytes_line = r.total_bytes as f64 / expected_lines as f64;

    println!(
        "  {name:>14}  {:>12}  {:>10}  {:>8}",
        format_rate(lines_sec),
        format_ns(ns_line),
        format!("{:.0} B", bytes_line),
    );
}

fn print_metric_result(name: &str, r: &ReceiverResult, expected_ticks: u64) {
    if r.total_bytes == 0 {
        println!("  {name:>14}  NO DATA RECEIVED");
        return;
    }

    let bytes_per_tick = r.total_bytes as f64 / expected_ticks as f64;

    // For witness: we can't count lines (FlatBuffer binary, no newlines).
    // For Vector: line_count = metric count (one JSON event per line).
    // Use line_count if available, otherwise estimate from known per-metric sizes.
    let total_metrics = if r.line_count > 0 {
        r.line_count
    } else {
        // FlatBuffer: estimate from known 211 bytes/metric
        r.total_bytes / 211
    };

    let ns_metric = if total_metrics > 0 {
        let elapsed = match (r.first_byte, r.last_byte) {
            (Some(f), Some(l)) => l.duration_since(f),
            _ => Duration::from_secs(METRIC_DURATION_SECS),
        };
        elapsed.as_nanos() as f64 / total_metrics as f64
    } else {
        0.0
    };

    println!(
        "  {name:>14}  {:>10}  {:>12}  {:>10}",
        format_count_u64(total_metrics),
        format_bytes_rate(bytes_per_tick),
        format_ns(ns_metric),
    );
}

fn format_rate(r: f64) -> String {
    if r >= 1_000_000.0 {
        format!("{:.1}M/s", r / 1_000_000.0)
    } else if r >= 1_000.0 {
        format!("{:.0}K/s", r / 1_000.0)
    } else {
        format!("{r:.0}/s")
    }
}

fn format_ns(ns: f64) -> String {
    if ns >= 1_000_000.0 {
        format!("{:.1} ms", ns / 1_000_000.0)
    } else if ns >= 1_000.0 {
        format!("{:.1} us", ns / 1_000.0)
    } else {
        format!("{ns:.0} ns")
    }
}

fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn format_count_u64(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_bytes_rate(b: f64) -> String {
    if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1_024.0 {
        format!("{:.1} KB", b / 1_024.0)
    } else {
        format!("{b:.0} B")
    }
}

// --- Binary paths (relative to project root) ---

const WITNESS_BIN: &str = "./target/release/witness";
const VECTOR_BIN: &str = "../vector/target/release/vector";

// --- Clean slate ---

fn kill_leftover_processes() {
    use std::process::Command as StdCommand;
    for name in ["witness", "vector"] {
        // pkill by name — ignore errors (nothing to kill is fine)
        let _ = StdCommand::new("pkill")
            .args(["-f", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    std::thread::sleep(Duration::from_millis(500));
}

// --- Main ---

#[tokio::main]
async fn main() {
    // Clean slate — kill any leftover processes from previous runs
    kill_leftover_processes();

    let witness = PathBuf::from(WITNESS_BIN);
    let vector = PathBuf::from(VECTOR_BIN);

    assert!(
        witness.exists(),
        "witness binary not found at {WITNESS_BIN} — run: cargo build --release"
    );

    let vector = if vector.exists() {
        Some(vector)
    } else {
        eprintln!("WARNING: vector binary not found at {VECTOR_BIN} — skipping Vector benchmarks");
        None
    };

    println!();
    println!("=== Benchmark: witness vs Vector ===");
    println!("  witness: {}", witness.display());
    match &vector {
        Some(v) => println!("  vector:     {}", v.display()),
        None => println!("  vector:     (not found, skipped)"),
    }

    bench_logs(&witness, vector.as_deref()).await;
    bench_metrics(&witness, vector.as_deref()).await;

    println!();
}
