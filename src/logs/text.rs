//! Byte-level primitives for the file tailer's per-line hot path.
//!
//! Drop-in replacements for std calls, kept behavior-identical:
//! - [`lossy`] — `String::from_utf8_lossy` with SIMD validation of the
//!   (overwhelmingly common) valid input.
//! - [`read_line`] — `BufRead::read_until(b'\n', ..)` with a SIMD newline
//!   search instead of std's word-at-a-time scan.

use std::borrow::Cow;
use std::io::{BufRead, ErrorKind};

/// Decode bytes as UTF-8, replacing invalid sequences with U+FFFD.
///
/// Valid input is validated with SIMD and borrowed without allocation;
/// invalid input falls back to std lossy decoding, so output is identical to
/// `String::from_utf8_lossy` in every case.
#[must_use]
pub(crate) fn lossy(bytes: &[u8]) -> Cow<'_, str> {
    match simdutf8::basic::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => String::from_utf8_lossy(bytes),
    }
}

/// Append bytes from `reader` to `buf` up to and including the next `\n`, or
/// to EOF. Returns the number of bytes appended (0 at EOF).
///
/// Same contract as `read_until(b'\n', buf)`: `Interrupted` is retried, and on
/// any other error the bytes already consumed stay appended to `buf`.
pub(crate) fn read_line<R: BufRead + ?Sized>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    let mut read = 0;
    loop {
        let (done, used) = {
            let available = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            match memchr::memchr(b'\n', available) {
                Some(i) => {
                    buf.extend_from_slice(&available[..=i]);
                    (true, i + 1)
                }
                None => {
                    buf.extend_from_slice(available);
                    (available.is_empty(), available.len())
                }
            }
        };
        reader.consume(used);
        read += used;
        if done {
            return Ok(read);
        }
    }
}
