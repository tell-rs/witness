//! Remote-config poller tests (spec 007). Covers the pure HTTP response parser,
//! response classification (change detection, validation, empty/other status),
//! content-hash determinism, and the https-only rejection of the poll loop.

use std::path::PathBuf;
use std::time::Duration;

use crate::remote_config::{
    HttpResponse, PollError, PollOutcome, PollerConfig, classify_response, config_hash,
    parse_http_response, run_poller,
};

const KEY: &str = "feed1e11feed1e11feed1e11feed1e11";

fn pc(applied_endpoint: &str, applied_hash: &str) -> PollerConfig {
    PollerConfig {
        server: "https://tell.example".to_string(),
        api_key: KEY.to_string(),
        endpoint: applied_endpoint.to_string(),
        config_path: PathBuf::from("/tmp/witness-test-config.toml"),
        interval: Duration::from_secs(300),
        applied_hash: applied_hash.to_string(),
    }
}

fn resp(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        body: body.as_bytes().to_vec(),
    }
}

// --- config_hash (R4/R5) ---

#[test]
fn test_config_hash_deterministic() {
    let a = config_hash(b"api_key = \"x\"\nendpoint = \"y\"\n");
    let b = config_hash(b"api_key = \"x\"\nendpoint = \"y\"\n");
    assert_eq!(a, b, "same bytes → same digest across calls");
    assert_eq!(a.len(), 16, "16 lowercase hex chars");
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_config_hash_differs_on_change() {
    assert_ne!(config_hash(b"a"), config_hash(b"b"));
    assert_ne!(config_hash(b""), config_hash(b"x"));
}

// --- parse_http_response (R2) ---

#[test]
fn test_parse_http_response_200_with_etag() {
    let raw = b"HTTP/1.1 200 OK\r\nETag: \"abc\"\r\nContent-Type: text/plain\r\n\r\nthe body";
    let r = parse_http_response(raw).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.etag.as_deref(), Some("\"abc\""));
    assert_eq!(r.body, b"the body");
}

#[test]
fn test_parse_http_response_304_no_body() {
    let raw = b"HTTP/1.1 304 Not Modified\r\nETag: \"v2\"\r\n\r\n";
    let r = parse_http_response(raw).unwrap();
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());
    assert_eq!(r.etag.as_deref(), Some("\"v2\""));
}

#[test]
fn test_parse_http_response_lf_only_separator() {
    // curl on some platforms emits bare LF line endings.
    let raw = b"HTTP/1.1 200 OK\nX-Test: y\n\nbody";
    let r = parse_http_response(raw).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"body");
}

#[test]
fn test_parse_http_response_etag_case_insensitive() {
    let raw = b"HTTP/1.1 200 OK\r\netag: lowercase\r\n\r\nb";
    let r = parse_http_response(raw).unwrap();
    assert_eq!(r.etag.as_deref(), Some("lowercase"));
}

#[test]
fn test_parse_http_response_malformed_no_boundary() {
    assert!(parse_http_response(b"garbage with no header boundary").is_none());
}

#[test]
fn test_parse_http_response_malformed_no_status() {
    // Header block present but the status line has no code.
    assert!(parse_http_response(b"\r\n\r\nbody").is_none());
}

// --- classify_response (R3/R4) ---

fn applied_config(endpoint: &str) -> String {
    format!("api_key = \"{KEY}\"\nendpoint = \"{endpoint}\"\n")
}

#[test]
fn test_classify_changed_flags_endpoint_and_server_drop() {
    let applied = applied_config("old:50000");
    let hash = config_hash(applied.as_bytes());
    let pc = pc("old:50000", &hash);
    // New body: different endpoint, same key, no server field.
    let body = applied_config("new:50000");

    match classify_response(&pc, &hash, resp(200, &body)) {
        Ok(PollOutcome::Changed {
            endpoint_changed,
            api_key_changed,
            server_dropped,
            ..
        }) => {
            assert!(endpoint_changed, "endpoint differs");
            assert!(!api_key_changed, "key unchanged");
            assert!(server_dropped, "new config omits server");
        }
        _ => panic!("expected Changed"),
    }
}

#[test]
fn test_classify_unchanged_by_hash() {
    let applied = applied_config("same:50000");
    let hash = config_hash(applied.as_bytes());
    let pc = pc("same:50000", &hash);

    // Identical body → hash equals applied → no reload (R4 idempotency guard).
    match classify_response(&pc, &hash, resp(200, &applied)) {
        Ok(PollOutcome::Unchanged { .. }) => {}
        _ => panic!("expected Unchanged"),
    }
}

#[test]
fn test_classify_invalid_body_kept() {
    let pc = pc("old:50000", "0000000000000000");
    // api_key too short → parse_config rejects → Invalid, current config kept.
    match classify_response(&pc, "0000000000000000", resp(200, "api_key = \"short\"")) {
        Ok(PollOutcome::Invalid(_)) => {}
        _ => panic!("expected Invalid"),
    }
}

#[test]
fn test_classify_empty_body_is_fetch_error() {
    let pc = pc("old:50000", "0000000000000000");
    match classify_response(&pc, "0000000000000000", resp(200, "   \n\t ")) {
        Err(PollError::Fetch(_)) => {}
        _ => panic!("expected a soft Fetch error for an empty body"),
    }
}

#[test]
fn test_classify_304_not_modified() {
    let pc = pc("old:50000", "0000000000000000");
    match classify_response(&pc, "0000000000000000", resp(304, "")) {
        Ok(PollOutcome::NotModified) => {}
        _ => panic!("expected NotModified"),
    }
}

#[test]
fn test_classify_other_status_is_fetch_error() {
    let pc = pc("old:50000", "0000000000000000");
    match classify_response(&pc, "0000000000000000", resp(500, "boom")) {
        Err(PollError::Fetch(_)) => {}
        _ => panic!("expected a soft Fetch error for a 500"),
    }
}

// --- R2: https-only enforcement ---

#[tokio::test]
async fn test_poller_rejects_http_scheme() {
    // Keep our own sender alive so the channel stays open after run_poller
    // drops its (moved) clone — otherwise has_changed would report a closed
    // channel rather than "no reload".
    let (reload_tx, reload_rx) = tokio::sync::watch::channel(());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut pc = pc("e:50000", "0000000000000000");
    pc.server = "http://insecure.example".to_string();

    // A non-https server disables the poller: run_poller returns immediately
    // (no curl spawned, no interval sleep) and never signals a reload.
    tokio::time::timeout(
        Duration::from_secs(2),
        run_poller(pc, reload_tx.clone(), cancel_rx),
    )
    .await
    .expect("poller returns immediately for a non-https server");

    assert!(
        matches!(reload_rx.has_changed(), Ok(false)),
        "no reload signalled"
    );
}

#[tokio::test]
async fn test_poller_disabled_when_interval_zero() {
    let (reload_tx, reload_rx) = tokio::sync::watch::channel(());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut pc = pc("e:50000", "0000000000000000");
    pc.interval = Duration::ZERO;

    tokio::time::timeout(
        Duration::from_secs(2),
        run_poller(pc, reload_tx.clone(), cancel_rx),
    )
    .await
    .expect("poller returns immediately when the interval is zero");

    assert!(matches!(reload_rx.has_changed(), Ok(false)));
}
