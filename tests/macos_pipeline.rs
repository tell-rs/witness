//! End-to-end pipeline test — runs actual macOS collectors through the real
//! Tell SDK, captures the FlatBuffer frames from TCP, and verifies every
//! expected metric name appears in the wire data.
//!
//! Run: cargo test --test macos_pipeline -- --nocapture

#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use tell::{Tell, TellConfig};
use tell_agent::collectors;
use tell_agent::config::SystemConfig;
use tell_agent::sink::Sink;

/// Read one length-prefixed frame from a TCP stream.
async fn read_frame(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.ok()?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.ok()?;
    Some(payload)
}

fn frame_contains(frame: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    frame.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn collectors_transmit_all_metrics() {
    // 1. Start TCP server
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let recv_clone = received.clone();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Read up to 10 frames (collectors may batch differently)
        for _ in 0..10 {
            match tokio::time::timeout(Duration::from_millis(500), read_frame(&mut stream)).await {
                Ok(Some(frame)) => recv_clone.lock().unwrap().push(frame),
                _ => break,
            }
        }
    });

    // 2. Create real Tell client pointed at our listener
    let config = TellConfig::builder("feed1e11feed1e11feed1e11feed1e11")
        .endpoint(addr.to_string())
        .source("test-mac")
        .service("tell-agent")
        .batch_size(500)
        .flush_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let client = Tell::new(config).unwrap();
    let sink = Sink::live(client.clone(), Default::default());

    // 3. Initialize the actual macOS collectors
    let sys_config = SystemConfig::default();
    let mut all_collectors = collectors::init_collectors(&sys_config);

    let names: Vec<&str> = all_collectors.iter().map(|c| c.name()).collect();
    eprintln!("initialized collectors: {}", names.join(", "));

    // 4. Run collectors twice — first tick is baseline, second emits deltas
    let mut buf = String::with_capacity(8192);
    for tick in 0..2 {
        for col in &mut all_collectors {
            col.collect(&sink, "test-mac", &mut buf);
        }
        if tick == 0 {
            // Small sleep so CPU ticks actually change between measurements
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // 5. Flush and wait for TCP delivery
    client.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    client.close().await.unwrap();
    server.abort();

    // 6. Merge all received frames and scan for metric names
    let frames = received.lock().unwrap();
    assert!(!frames.is_empty(), "no frames received from TCP");

    let all_bytes: Vec<u8> = frames.iter().flat_map(|f| f.iter().copied()).collect();
    eprintln!(
        "\nreceived {} frame(s), {} total bytes\n",
        frames.len(),
        all_bytes.len()
    );

    // Every metric name our macOS collectors should emit:
    let expected_metrics = [
        // Load
        ("system.load.1", "load"),
        ("system.load.5", "load"),
        ("system.load.15", "load"),
        // CPU
        ("system.cpu.user", "cpu"),
        ("system.cpu.system", "cpu"),
        ("system.cpu.idle", "cpu"),
        // Memory
        ("system.memory.total", "memory"),
        ("system.memory.available", "memory"),
        ("system.memory.used", "memory"),
        ("system.memory.cached", "memory"),
        ("system.memory.swap_used", "memory"),
        // Network
        ("system.net.bytes_recv", "network"),
        ("system.net.bytes_sent", "network"),
        ("system.net.packets_recv", "network"),
        ("system.net.packets_sent", "network"),
        // Disk
        ("system.disk.total_bytes", "disk"),
        ("system.disk.used_bytes", "disk"),
        ("system.disk.free_bytes", "disk"),
    ];

    // Expected labels
    let expected_labels = ["core", "interface", "mount", "device"];

    let mut found = 0;
    let mut missing = Vec::new();
    for (metric, collector) in &expected_metrics {
        if frame_contains(&all_bytes, metric) {
            found += 1;
            eprintln!("  found  {metric:<30} ({collector})");
        } else {
            missing.push((*metric, *collector));
            eprintln!("  MISS   {metric:<30} ({collector})");
        }
    }

    eprintln!();
    for label in &expected_labels {
        let present = frame_contains(&all_bytes, label);
        eprintln!(
            "  label  {label:<15} {}",
            if present { "found" } else { "MISSING" }
        );
    }

    // Source and service should be in the frames
    eprintln!();
    eprintln!(
        "  source 'test-mac'     {}",
        if frame_contains(&all_bytes, "test-mac") {
            "found"
        } else {
            "MISSING"
        }
    );
    eprintln!(
        "  service 'tell-agent'  {}",
        if frame_contains(&all_bytes, "tell-agent") {
            "found"
        } else {
            "MISSING"
        }
    );

    eprintln!(
        "\n{found}/{} metrics found in wire data",
        expected_metrics.len()
    );

    // Allow swap to be missing (some Macs have swap disabled)
    let hard_missing: Vec<_> = missing
        .iter()
        .filter(|(name, _)| *name != "system.memory.swap_used")
        .collect();

    assert!(
        hard_missing.is_empty(),
        "missing metrics in transmitted data: {hard_missing:?}",
    );
}
