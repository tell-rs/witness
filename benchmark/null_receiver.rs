//! Null TCP receiver for benchmarking.
//!
//! Accepts connections, reads all data, counts bytes and frames,
//! reports throughput when each connection closes.
//!
//! Usage: cargo run --example null_receiver --release [-- --port 9999]
//!        cargo run --example null_receiver --release [-- --port 9999 --dump /tmp/capture.bin]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
struct Stats {
    total_bytes: Arc<AtomicU64>,
    connections: Arc<AtomicU64>,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(9999);

    let dump_path = args
        .iter()
        .position(|a| a == "--dump")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("failed to bind");

    let addr = listener.local_addr().unwrap();
    eprintln!("null receiver listening on {addr}");
    if let Some(ref p) = dump_path {
        eprintln!("dumping raw bytes to {p}");
    }

    let stats = Stats {
        total_bytes: Arc::new(AtomicU64::new(0)),
        connections: Arc::new(AtomicU64::new(0)),
    };

    // Print global stats every 5 seconds
    let s = stats.clone();
    let start = Instant::now();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let bytes = s.total_bytes.load(Ordering::Relaxed);
            let conns = s.connections.load(Ordering::Relaxed);
            let elapsed = start.elapsed().as_secs_f64();
            if bytes > 0 {
                eprintln!(
                    "[global] {conns} conn, {} received, {}/s avg",
                    format_bytes(bytes),
                    format_bytes((bytes as f64 / elapsed) as u64),
                );
            }
        }
    });

    loop {
        let (mut stream, peer) = listener.accept().await.expect("accept failed");
        let stats = stats.clone();
        let dump_path = dump_path.clone();
        stats.connections.fetch_add(1, Ordering::Relaxed);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 256 * 1024];
            let mut conn_bytes: u64 = 0;
            let conn_start = Instant::now();
            let mut first_byte = None;

            // Open dump file if requested
            let mut dump_file = match &dump_path {
                Some(p) => tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .await
                    .ok(),
                None => None,
            };

            loop {
                match stream.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if first_byte.is_none() {
                            first_byte = Some(Instant::now());
                        }
                        conn_bytes += n as u64;
                        stats.total_bytes.fetch_add(n as u64, Ordering::Relaxed);

                        if let Some(ref mut f) = dump_file {
                            let _ = f.write_all(&buf[..n]).await;
                        }
                    }
                    Err(_) => break,
                }
            }

            let elapsed = conn_start.elapsed();
            let data_elapsed = first_byte.map(|t| t.elapsed()).unwrap_or(elapsed);

            if conn_bytes > 0 {
                let throughput = conn_bytes as f64 / data_elapsed.as_secs_f64();
                eprintln!(
                    "[conn {peer}] {} in {:.2}s — {}/s",
                    format_bytes(conn_bytes),
                    data_elapsed.as_secs_f64(),
                    format_bytes(throughput as u64),
                );
            }
        });
    }
}

fn format_bytes(b: u64) -> String {
    if b >= 1_073_741_824 {
        format!("{:.2} GB", b as f64 / 1_073_741_824.0)
    } else if b >= 1_048_576 {
        format!("{:.1} MB", b as f64 / 1_048_576.0)
    } else if b >= 1_024 {
        format!("{:.1} KB", b as f64 / 1_024.0)
    } else {
        format!("{b} B")
    }
}
