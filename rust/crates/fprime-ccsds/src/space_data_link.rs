//! CCSDS 132.0 / 232.0 Space Data Link Protocol (TM/TC Transfer Frame).

use fprime_frame::{Deframer, FrameError, Framer};
use tracing::warn;

use crate::{crc16_ccitt_false, CcsdsError};

pub const TC_HEADER_SIZE: usize = 5;
pub const TC_TRAILER_SIZE: usize = 2;
pub const TM_HEADER_SIZE: usize = 6;
pub const TM_TRAILER_SIZE: usize = 2;

pub const SCID_MAX: u16 = 0x3FF; // 10 bits
pub const VCID_MAX: u8 = 0x3F; // 6 bits for TC; only 3 bits are validated for TM

/// Default values from `fprime_gds.common.communication.ccsds.space_data_link`
/// when the dictionary doesn't override them.
pub const FALLBACK_SCID: u16 = 0x44;
pub const FALLBACK_FRAME_SIZE: usize = 1024;

/// CCSDS 232.0 TC Transfer Frame framer.
///
/// Produces a 5-byte primary header + payload + 2-byte CRC-16/CCITT-FALSE
/// trailer.  Type-D, FARM bypass enabled, sequence number always `0` (matches
/// the Python `SpaceDataLinkFramerDeframer.frame`).
#[derive(Debug, Clone)]
pub struct TcFramer {
    pub scid: u16,
    pub vcid: u8,
}

impl TcFramer {
    pub fn new(scid: u16, vcid: u8) -> Result<Self, CcsdsError> {
        if scid > SCID_MAX {
            return Err(CcsdsError::ScidOutOfRange(scid as u32));
        }
        if vcid > VCID_MAX {
            return Err(CcsdsError::VcidOutOfRange(vcid as u32));
        }
        Ok(Self { scid, vcid })
    }
}

impl Framer for TcFramer {
    fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        // CCSDS TC length token: total frame length minus 1.
        let total_len = TC_HEADER_SIZE + payload.len() + TC_TRAILER_SIZE;
        if total_len < 1 || total_len - 1 > 0x3FF {
            return Err(CcsdsError::TooLarge(total_len).into());
        }
        let length_token = (total_len - 1) as u16;

        // Word 0: ver(2)=00 | bypass(1)=1 | type(1)=0 | reserved(2)=00 | scid(10)
        let w0: u16 = (1u16 << 13) | (self.scid & 0x3FF);
        // Word 1: vcid(6) | length(10)
        let w1: u16 = ((self.vcid as u16 & 0x3F) << 10) | (length_token & 0x3FF);
        // Word 2: 8-bit sequence number (always 0 in bypass-FARM)
        let seq: u8 = 0;

        let mut out = Vec::with_capacity(total_len);
        out.extend_from_slice(&w0.to_be_bytes());
        out.extend_from_slice(&w1.to_be_bytes());
        out.push(seq);
        out.extend_from_slice(payload);
        let crc = crc16_ccitt_false(&out);
        out.extend_from_slice(&crc.to_be_bytes());
        debug_assert_eq!(out.len(), total_len);
        Ok(out)
    }
}

/// CCSDS 132.0 TM Transfer Frame deframer.
///
/// Fixed-size frames.  Primary header layout (matching the masks the Python
/// implementation uses on the first 16-bit word):
///
/// ```text
/// bits 14-15: TF version (00)
/// bits  4-13: spacecraft id (10 bits)
/// bits  1- 3: virtual channel id (3 bits)
/// bit      0: OCF flag
/// ```
///
/// The remaining 4 bytes hold master/virtual channel frame counts and the
/// data-field status — none of which are used by the Python deframer either.
#[derive(Debug)]
pub struct TmDeframer {
    pub scid: u16,
    pub vcid: u8,
    pub frame_size: usize,
    buf: Vec<u8>,
    discarded: usize,
}

impl TmDeframer {
    pub fn new(scid: u16, vcid: u8, frame_size: usize) -> Result<Self, CcsdsError> {
        if scid > SCID_MAX {
            return Err(CcsdsError::ScidOutOfRange(scid as u32));
        }
        if frame_size <= TM_HEADER_SIZE + TM_TRAILER_SIZE {
            return Err(CcsdsError::TooLarge(frame_size));
        }
        Ok(Self {
            scid,
            vcid,
            frame_size,
            buf: Vec::new(),
            discarded: 0,
        })
    }
}

impl Deframer for TmDeframer {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        loop {
            if self.buf.len() < self.frame_size {
                return None;
            }
            let w0 = u16::from_be_bytes([self.buf[0], self.buf[1]]);
            let scid = (w0 & 0x3FF0) >> 4;
            let vcid = ((w0 & 0x000E) >> 1) as u8;
            if scid != self.scid || vcid != self.vcid {
                self.discard_one();
                continue;
            }
            let crc_offset = self.frame_size - TM_TRAILER_SIZE;
            let body = &self.buf[..crc_offset];
            let expected = crc16_ccitt_false(body);
            let got = u16::from_be_bytes([self.buf[crc_offset], self.buf[crc_offset + 1]]);
            if expected != got {
                warn!(
                    expected = format!("{expected:#06x}"),
                    got = format!("{got:#06x}"),
                    "TM frame CRC mismatch — resyncing"
                );
                self.discard_one();
                continue;
            }
            let payload = self.buf[TM_HEADER_SIZE..crc_offset].to_vec();
            self.buf.drain(..self.frame_size);
            return Some(payload);
        }
    }

    fn discarded(&mut self) -> usize {
        std::mem::take(&mut self.discarded)
    }
}

impl TmDeframer {
    fn discard_one(&mut self) {
        self.discarded += 1;
        self.buf.drain(..1);
    }
}

/// Build a TM transfer frame for testing purposes.  Mirrors the Python's
/// header packing on the first 16-bit word; the rest of the header is zero.
pub fn build_tm_frame(scid: u16, vcid: u8, payload: &[u8], frame_size: usize) -> Vec<u8> {
    assert!(frame_size > TM_HEADER_SIZE + TM_TRAILER_SIZE);
    let inner_max = frame_size - TM_HEADER_SIZE - TM_TRAILER_SIZE;
    assert!(payload.len() <= inner_max);
    let mut out = Vec::with_capacity(frame_size);
    let w0: u16 = ((scid & 0x3FF) << 4) | (((vcid as u16) & 0x07) << 1);
    out.extend_from_slice(&w0.to_be_bytes());
    out.extend_from_slice(&[0u8; TM_HEADER_SIZE - 2]);
    out.extend_from_slice(payload);
    out.resize(frame_size - TM_TRAILER_SIZE, 0);
    let crc = crc16_ccitt_false(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc_frame_layout_matches_python() {
        let mut framer = TcFramer::new(0x44, 1).unwrap();
        let payload = b"hello";
        let bytes = framer.frame(payload).unwrap();

        // Total length = header(5) + payload(5) + crc(2) = 12.
        assert_eq!(bytes.len(), 12);
        // Length token = 11 (total-1) in the lower 10 bits of word 1.
        let w0 = u16::from_be_bytes([bytes[0], bytes[1]]);
        let w1 = u16::from_be_bytes([bytes[2], bytes[3]]);
        // Word 0: ver=0, bypass=1, type=0, reserved=0, scid=0x44
        // bypass bit at position 13 -> 0x2000
        assert_eq!(w0, 0x2000 | 0x44);
        // Word 1: vcid=1 in upper 6 bits, length=11 in lower 10
        assert_eq!(w1, (1u16 << 10) | 11);
        // Sequence number byte
        assert_eq!(bytes[4], 0);
        // Payload verbatim
        assert_eq!(&bytes[5..10], payload);
        // CRC matches our own calc on header+payload
        let expected = crc16_ccitt_false(&bytes[..10]);
        let got = u16::from_be_bytes([bytes[10], bytes[11]]);
        assert_eq!(expected, got);
    }

    #[test]
    fn tm_round_trip() {
        let payload = b"\x00\x00\x00\x05hi";
        let frame = build_tm_frame(0x44, 1, payload, 32);
        let mut d = TmDeframer::new(0x44, 1, 32).unwrap();
        d.push(&frame);
        let out = d.pop().expect("frame");
        // Payload is padded to fill the data field.
        assert_eq!(&out[..payload.len()], payload);
    }

    #[test]
    fn tm_resyncs_on_wrong_scid() {
        let mut wire = vec![0xFFu8; 4];
        wire.extend_from_slice(&build_tm_frame(0x44, 1, b"hi", 32));
        let mut d = TmDeframer::new(0x44, 1, 32).unwrap();
        d.push(&wire);
        let out = d.pop().expect("frame");
        assert!(out.starts_with(b"hi"));
        assert!(d.discarded() >= 4);
    }

    #[test]
    fn tm_drops_bad_crc() {
        let mut frame = build_tm_frame(0x44, 1, b"hi", 32);
        let last = frame.len() - 1;
        frame[last] ^= 0xFF; // corrupt CRC
        let mut d = TmDeframer::new(0x44, 1, 32).unwrap();
        d.push(&frame);
        // Should resync past the bad frame and return None.
        assert!(d.pop().is_none());
        assert!(d.discarded() > 0);
    }
}
