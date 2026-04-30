//! F´ frame format with CRC32 checksum.
//!
//! Wire format (matches `fprime_gds.common.communication.framing.FpFramerDeframer`):
//!
//! ```text
//! +-------------------+----------------+--------------------+----------------+
//! | start (4 BE)      | length (4 BE)  | payload (N bytes)  | crc32 (4 BE)   |
//! +-------------------+----------------+--------------------+----------------+
//!   0xDEADBEEF                          (length bytes)        crc of header+payload
//! ```
//!
//! `length` is the byte count of `payload` only.  The CRC32 is computed over
//! the header (start + length) and payload — i.e. the full frame minus the
//! checksum field itself.

#![deny(rust_2018_idioms)]

use thiserror::Error;

pub const START_TOKEN: u32 = 0xDEAD_BEEF;
pub const HEADER_SIZE: usize = 8;
pub const CHECKSUM_SIZE: usize = 4;
pub const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("checksum mismatch (got {got:#010x}, expected {expected:#010x})")]
    Checksum { got: u32, expected: u32 },
    #[error("payload too large: {0} bytes")]
    TooLarge(usize),
    #[error("ccsds error: {0}")]
    Ccsds(String),
}

/// Encodes a payload into a wire frame.
///
/// Each call should produce a complete, self-contained frame for `payload`.
/// Implementations may track per-channel state (e.g. CCSDS sequence counters).
pub trait Framer: Send {
    fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>, FrameError>;
}

/// Pulls complete frames out of a streaming byte source.
///
/// Callers `push` new bytes as they arrive and repeatedly call `pop` to drain
/// any frames that are now available.  Implementations buffer internally so
/// partial frames are retained across calls.
pub trait Deframer: Send {
    fn push(&mut self, data: &[u8]);
    fn pop(&mut self) -> Option<Vec<u8>>;
    fn discarded(&mut self) -> usize {
        0
    }
}

/// F´ DEADBEEF + length + CRC32 framer.
#[derive(Debug, Default, Clone)]
pub struct FpFramer;

impl Framer for FpFramer {
    fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        frame(payload)
    }
}

/// F´ DEADBEEF + length + CRC32 deframer.  Buffers incoming bytes, resyncs
/// past garbage one byte at a time on bad starts or bad checksums.
#[derive(Debug, Default)]
pub struct FpDeframer {
    buf: Vec<u8>,
    discarded: usize,
}

impl FpDeframer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Deframer for FpDeframer {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        let result = deframe(&self.buf);
        self.discarded += result.discarded.len();
        if let Some(frame) = result.frame {
            self.buf.drain(..result.consumed);
            Some(frame)
        } else {
            if result.consumed > 0 {
                self.buf.drain(..result.consumed);
            }
            None
        }
    }

    fn discarded(&mut self) -> usize {
        std::mem::take(&mut self.discarded)
    }
}

/// Outcome of a single attempt to deframe.
#[derive(Debug, Clone)]
pub struct Deframed {
    /// Payload bytes (without start, length, or CRC).  `None` when more data is
    /// needed.
    pub frame: Option<Vec<u8>>,
    /// Bytes consumed from the input.
    pub consumed: usize,
    /// Bytes that were skipped while resyncing — useful for diagnostics.
    pub discarded: Vec<u8>,
}

/// Builds an F´ frame around `payload`.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload.len() + CHECKSUM_SIZE);
    buf.extend_from_slice(&START_TOKEN.to_be_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    let crc = crc32fast::hash(&buf);
    buf.extend_from_slice(&crc.to_be_bytes());
    Ok(buf)
}

/// Pull a single frame off the front of `data`.
///
/// Returns the parsed frame (or `None` if more data is needed) plus how many
/// bytes the caller should drop from the front of its buffer, plus any bytes
/// that were discarded during resync.  When a checksum fails or a non-start
/// byte appears we shift forward by one byte and keep looking.
pub fn deframe(data: &[u8]) -> Deframed {
    let mut pos: usize = 0;
    let mut discarded = Vec::new();
    while data.len() - pos >= HEADER_SIZE {
        let start = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let length =
            u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                as usize;

        if start != START_TOKEN || length > MAX_PAYLOAD {
            discarded.push(data[pos]);
            pos += 1;
            continue;
        }

        let total = HEADER_SIZE + length + CHECKSUM_SIZE;
        if data.len() - pos < total {
            // Not enough data for the full frame yet.
            return Deframed {
                frame: None,
                consumed: pos,
                discarded,
            };
        }

        let header_and_payload = &data[pos..pos + HEADER_SIZE + length];
        let expected = crc32fast::hash(header_and_payload);
        let got = u32::from_be_bytes([
            data[pos + HEADER_SIZE + length],
            data[pos + HEADER_SIZE + length + 1],
            data[pos + HEADER_SIZE + length + 2],
            data[pos + HEADER_SIZE + length + 3],
        ]);

        if expected != got {
            tracing::warn!(got = %format!("{got:#010x}"), expected = %format!("{expected:#010x}"), "checksum mismatch — resyncing");
            discarded.push(data[pos]);
            pos += 1;
            continue;
        }

        let payload = data[pos + HEADER_SIZE..pos + HEADER_SIZE + length].to_vec();
        return Deframed {
            frame: Some(payload),
            consumed: pos + total,
            discarded,
        };
    }
    Deframed {
        frame: None,
        consumed: pos,
        discarded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let payload = b"hello fprime";
        let framed = frame(payload).unwrap();
        let out = deframe(&framed);
        assert_eq!(out.consumed, framed.len());
        assert_eq!(out.discarded.len(), 0);
        assert_eq!(out.frame.as_deref(), Some(&payload[..]));
    }

    #[test]
    fn resync_through_garbage() {
        let payload = b"hello";
        let framed = frame(payload).unwrap();
        let mut bytes = vec![0xAA, 0xBB, 0xCC];
        bytes.extend_from_slice(&framed);
        let out = deframe(&bytes);
        assert_eq!(out.frame.as_deref(), Some(&payload[..]));
        assert_eq!(out.discarded, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn partial_returns_none() {
        let framed = frame(b"data").unwrap();
        let out = deframe(&framed[..framed.len() - 1]);
        assert!(out.frame.is_none());
    }

    #[test]
    fn checksum_failure_rotates() {
        let mut framed = frame(b"data").unwrap();
        // corrupt the last byte (CRC)
        let last = framed.len() - 1;
        framed[last] ^= 0xFF;
        let out = deframe(&framed);
        assert!(out.frame.is_none());
        // start byte should have been discarded as we rotate
        assert!(!out.discarded.is_empty());
    }

    #[test]
    fn too_large_rejected() {
        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert!(matches!(frame(&big), Err(FrameError::TooLarge(_))));
    }
}
