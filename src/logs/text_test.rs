use std::io::{BufRead, BufReader, Cursor, Read};

use super::structured::logfmt_field_start;
use super::text;

/// Deterministic xorshift so failures reproduce without a rand dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Random bytes drawn from `alphabet`, length in `0..max_len`.
    fn bytes(&mut self, alphabet: &[&[u8]], max_len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..self.below(max_len) {
            out.extend_from_slice(alphabet[self.below(alphabet.len())]);
        }
        out
    }
}

type ReadLineFn = fn(&mut BufReader<Cursor<&[u8]>>, &mut Vec<u8>) -> std::io::Result<usize>;

/// Read every line of `data` through a `capacity`-byte buffer with `f`.
fn collect_lines(data: &[u8], capacity: usize, f: ReadLineFn) -> Vec<(usize, Vec<u8>)> {
    let mut reader = BufReader::with_capacity(capacity, Cursor::new(data));
    let mut out = Vec::new();
    loop {
        let mut buf = Vec::new();
        let n = f(&mut reader, &mut buf).unwrap();
        out.push((n, buf));
        if n == 0 {
            return out;
        }
    }
}

// ─── read_line ≡ read_until(b'\n') ──────────────────────────────────────────

#[test]
fn test_read_line_matches_read_until_randomized() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let alphabet: &[&[u8]] = &[
        b"a",
        b"\n",
        b"\r\n",
        b" ",
        b"\xff",
        "é".as_bytes(),
        b"0123456789",
    ];
    for _ in 0..2_000 {
        let data = rng.bytes(alphabet, 200);
        // Capacities around chunk-boundary edge cases, plus the std default.
        for capacity in [1, 2, 3, 7, 16, 64, 8 * 1024] {
            let ours = collect_lines(&data, capacity, |r, b| text::read_line(r, b));
            let std = collect_lines(&data, capacity, |r, b| r.read_until(b'\n', b));
            assert_eq!(ours, std, "capacity {capacity}, data {data:?}");
        }
    }
}

#[test]
fn test_read_line_appends_to_existing_buf() {
    let mut reader = Cursor::new(&b"tail\nnext"[..]);
    let mut buf = b"head-".to_vec();
    assert_eq!(text::read_line(&mut reader, &mut buf).unwrap(), 5);
    assert_eq!(buf, b"head-tail\n");
}

#[test]
fn test_read_line_partial_then_eof() {
    let mut reader = Cursor::new(&b"no newline"[..]);
    let mut buf = Vec::new();
    assert_eq!(text::read_line(&mut reader, &mut buf).unwrap(), 10);
    assert_eq!(buf, b"no newline");
    buf.clear();
    assert_eq!(text::read_line(&mut reader, &mut buf).unwrap(), 0);
    assert!(buf.is_empty());
}

/// A reader that fails with `kind` on its first read, then serves `data`.
struct FailOnce {
    kind: std::io::ErrorKind,
    failed: bool,
    data: Cursor<Vec<u8>>,
}

impl Read for FailOnce {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if !self.failed {
            self.failed = true;
            return Err(self.kind.into());
        }
        self.data.read(out)
    }
}

#[test]
fn test_read_line_retries_interrupted() {
    let inner = FailOnce {
        kind: std::io::ErrorKind::Interrupted,
        failed: false,
        data: Cursor::new(b"line\n".to_vec()),
    };
    let mut reader = BufReader::new(inner);
    let mut buf = Vec::new();
    assert_eq!(text::read_line(&mut reader, &mut buf).unwrap(), 5);
    assert_eq!(buf, b"line\n");
}

#[test]
fn test_read_line_propagates_other_errors() {
    let inner = FailOnce {
        kind: std::io::ErrorKind::BrokenPipe,
        failed: false,
        data: Cursor::new(Vec::new()),
    };
    let mut reader = BufReader::new(inner);
    let err = text::read_line(&mut reader, &mut Vec::new()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

// ─── lossy ≡ String::from_utf8_lossy ────────────────────────────────────────

#[test]
fn test_lossy_valid_borrows() {
    let line = "2026-10-05 INFO café opened ✓".as_bytes();
    assert!(matches!(text::lossy(line), std::borrow::Cow::Borrowed(s) if s.as_bytes() == line));
}

#[test]
fn test_lossy_matches_std_randomized() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    // Valid ASCII, valid multi-byte, truncated sequences, lone continuations,
    // overlongs, surrogates, and out-of-range lead bytes.
    let alphabet: &[&[u8]] = &[
        b"a",
        b" ",
        "é".as_bytes(),
        "✓".as_bytes(),
        "🦀".as_bytes(),
        b"\xc3",
        b"\xe2\x9c",
        b"\x80",
        b"\xc0\xaf",
        b"\xed\xa0\x80",
        b"\xf5",
        b"\xff",
    ];
    for _ in 0..20_000 {
        let bytes = rng.bytes(alphabet, 80);
        assert_eq!(
            text::lossy(&bytes),
            String::from_utf8_lossy(&bytes),
            "{bytes:?}"
        );
    }
}

// ─── logfmt_field_start ≡ the original per-space scan ───────────────────────

/// The original byte-by-byte implementation, kept as the reference oracle.
fn logfmt_field_start_reference(s: &str) -> Option<usize> {
    let is_key = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.';
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b' ' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && is_key(bytes[j]) {
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b'=' {
                return Some(i);
            }
        }
    }
    None
}

#[test]
fn test_logfmt_field_start_matches_reference_randomized() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let alphabet: &[&[u8]] = &[
        b" ",
        b"=",
        b"a",
        b"Z",
        b"9",
        b"_",
        b"-",
        b".",
        b"\"",
        b"\t",
        b"?",
        b"/",
        "é".as_bytes(),
    ];
    for _ in 0..50_000 {
        let bytes = rng.bytes(alphabet, 40);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(
            logfmt_field_start(s),
            logfmt_field_start_reference(s),
            "{s:?}"
        );
    }
}

#[test]
fn test_logfmt_field_start_fixtures() {
    for s in [
        "request completed method=GET path=/api status=200",
        r#"192.168.1.42 - - [05/Oct/2026:10:12:01 +0000] "GET /x?id=42 HTTP/1.1" 200 512"#,
        "=leading",
        " =empty-key",
        "a=b no-space-before-first",
        "url=/x?a=b c=d",
        "spaced key = value",
        "trailing space ",
        "",
    ] {
        assert_eq!(
            logfmt_field_start(s),
            logfmt_field_start_reference(s),
            "{s:?}"
        );
    }
}
