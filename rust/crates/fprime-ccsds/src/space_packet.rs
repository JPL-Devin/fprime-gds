//! CCSDS 133.0 Space Packet framer/deframer.

use std::collections::HashMap;

use fprime_frame::{Deframer, FrameError, Framer};
use tracing::{debug, warn};

use crate::CcsdsError;

pub const HEADER_SIZE: usize = 6;
pub const SEQ_COUNT_MOD: u16 = 16384; // 2^14
pub const APID_MASK: u16 = 0x07FF; // 11 bits
pub const IDLE_APID: u16 = 0x07FF;
pub const SEQ_FLAGS_UNSEGMENTED: u16 = 0b11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Tm = 0,
    Tc = 1,
}

#[derive(Debug, Clone, Copy)]
pub struct SpacePacketHeader {
    pub version: u8,
    pub packet_type: PacketType,
    pub sec_hdr_flag: bool,
    pub apid: u16,
    pub seq_flags: u8,
    pub seq_count: u16,
    /// Number of octets in the packet data field, minus one.
    pub data_len_minus_one: u16,
}

impl SpacePacketHeader {
    pub fn pack(&self) -> [u8; HEADER_SIZE] {
        let w0 = ((self.version as u16) & 0b111) << 13
            | (self.packet_type as u16) << 12
            | (if self.sec_hdr_flag { 1 } else { 0 }) << 11
            | (self.apid & APID_MASK);
        let w1 = ((self.seq_flags as u16) & 0b11) << 14 | (self.seq_count & 0x3FFF);
        let w2 = self.data_len_minus_one;
        let mut out = [0u8; 6];
        out[0..2].copy_from_slice(&w0.to_be_bytes());
        out[2..4].copy_from_slice(&w1.to_be_bytes());
        out[4..6].copy_from_slice(&w2.to_be_bytes());
        out
    }

    pub fn unpack(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }
        let w0 = u16::from_be_bytes([data[0], data[1]]);
        let w1 = u16::from_be_bytes([data[2], data[3]]);
        let w2 = u16::from_be_bytes([data[4], data[5]]);
        let version = ((w0 >> 13) & 0b111) as u8;
        let pt_bit = (w0 >> 12) & 0b1;
        let packet_type = if pt_bit == 0 {
            PacketType::Tm
        } else {
            PacketType::Tc
        };
        let sec_hdr_flag = ((w0 >> 11) & 0b1) != 0;
        let apid = w0 & APID_MASK;
        let seq_flags = ((w1 >> 14) & 0b11) as u8;
        let seq_count = w1 & 0x3FFF;
        Some(SpacePacketHeader {
            version,
            packet_type,
            sec_hdr_flag,
            apid,
            seq_flags,
            seq_count,
            data_len_minus_one: w2,
        })
    }

    /// Total wire size of the packet (header + data field).
    pub fn packet_len(&self) -> usize {
        HEADER_SIZE + self.data_len_minus_one as usize + 1
    }
}

/// Uplink Space Packet framer.  Each call wraps the user payload in a TC
/// space packet whose APID is taken from the leading 4 bytes of the payload
/// (matching `fprime_gds.common.communication.ccsds.space_packet.SpacePacketFramerDeframer.frame`).
#[derive(Debug, Default)]
pub struct SpacePacketFramer {
    seq: HashMap<u16, u16>,
}

impl SpacePacketFramer {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_seq(&mut self, apid: u16) -> u16 {
        let entry = self.seq.entry(apid).or_insert(0);
        let cur = *entry;
        *entry = (cur + 1) % SEQ_COUNT_MOD;
        cur
    }

    fn extract_apid(payload: &[u8]) -> Result<u16, CcsdsError> {
        if payload.len() < 4 {
            return Err(CcsdsError::Empty);
        }
        let raw = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        if raw > APID_MASK as u32 {
            // The leading U32 is a F´ descriptor / opcode prefix.  When a
            // deployment uses the CCSDS chain it must be configured so the
            // value fits in 11 bits — otherwise we'd silently truncate.
            return Err(CcsdsError::ApidOutOfRange(raw));
        }
        Ok(raw as u16)
    }
}

impl Framer for SpacePacketFramer {
    fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        let apid = SpacePacketFramer::extract_apid(payload).map_err(FrameError::from)?;
        if payload.is_empty() {
            return Err(CcsdsError::Empty.into());
        }
        if payload.len() > (u16::MAX as usize + 1) {
            return Err(CcsdsError::TooLarge(payload.len()).into());
        }
        let seq_count = self.next_seq(apid);
        let header = SpacePacketHeader {
            version: 0,
            packet_type: PacketType::Tc,
            sec_hdr_flag: false,
            apid,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count,
            data_len_minus_one: (payload.len() as u16) - 1,
        };
        let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
        out.extend_from_slice(&header.pack());
        out.extend_from_slice(payload);
        Ok(out)
    }
}

/// Downlink Space Packet deframer.  Discards idle APID `0x7FF` packets and
/// resyncs one byte at a time on invalid headers (matching the Python
/// `SpacePacketFramerDeframer.deframe`).
#[derive(Debug, Default)]
pub struct SpacePacketDeframer {
    buf: Vec<u8>,
    discarded: usize,
    /// Per-APID expected sequence count.  Mismatches log a warning but the
    /// packet is still returned (matching the Python implementation, which
    /// only warns).
    seq: HashMap<u16, u16>,
}

impl SpacePacketDeframer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Deframer for SpacePacketDeframer {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        loop {
            if self.buf.len() < HEADER_SIZE {
                return None;
            }
            let header = SpacePacketHeader::unpack(&self.buf)?;
            if header.version != 0 || header.packet_type != PacketType::Tm {
                self.discard_one();
                continue;
            }
            let total = header.packet_len();
            if header.apid == IDLE_APID {
                if self.buf.len() < total {
                    return None;
                }
                self.buf.drain(..total);
                continue;
            }
            if self.buf.len() < total {
                return None;
            }
            // Sequence count: warn on mismatch but still deliver.
            let expected = self.seq.entry(header.apid).or_insert(0);
            if header.seq_count != *expected {
                warn!(
                    apid = header.apid,
                    received = header.seq_count,
                    expected = *expected,
                    "space-packet sequence-count mismatch"
                );
            }
            *expected = (header.seq_count + 1) % SEQ_COUNT_MOD;

            let body = self.buf[HEADER_SIZE..total].to_vec();
            self.buf.drain(..total);
            debug!(
                apid = header.apid,
                len = body.len(),
                "deframed space packet"
            );
            return Some(body);
        }
    }

    fn discarded(&mut self) -> usize {
        std::mem::take(&mut self.discarded)
    }
}

impl SpacePacketDeframer {
    fn discard_one(&mut self) {
        self.discarded += 1;
        self.buf.drain(..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_payload(apid: u32, body: &[u8]) -> Vec<u8> {
        let mut p = apid.to_be_bytes().to_vec();
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn frame_extracts_apid_and_increments_seq() {
        let mut framer = SpacePacketFramer::new();
        let payload = make_payload(0x42, &[1, 2, 3, 4]);
        let f0 = framer.frame(&payload).unwrap();
        let f1 = framer.frame(&payload).unwrap();
        let h0 = SpacePacketHeader::unpack(&f0).unwrap();
        let h1 = SpacePacketHeader::unpack(&f1).unwrap();
        assert_eq!(h0.version, 0);
        assert_eq!(h0.packet_type, PacketType::Tc);
        assert_eq!(h0.apid, 0x42);
        assert_eq!(h0.seq_count, 0);
        assert_eq!(h1.seq_count, 1);
        // Length token is N-1.
        assert_eq!(h0.data_len_minus_one as usize + 1, payload.len());
        // Body verbatim after the header.
        assert_eq!(&f0[HEADER_SIZE..], payload.as_slice());
    }

    #[test]
    fn deframe_round_trip_with_tm_header() {
        // Build a TM space packet manually.
        let body = b"\x00\x00\x00\x01ABCDE";
        let header = SpacePacketHeader {
            version: 0,
            packet_type: PacketType::Tm,
            sec_hdr_flag: false,
            apid: 1,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count: 0,
            data_len_minus_one: (body.len() as u16) - 1,
        };
        let mut wire = header.pack().to_vec();
        wire.extend_from_slice(body);

        let mut d = SpacePacketDeframer::new();
        d.push(&wire);
        let out = d.pop().expect("frame");
        assert_eq!(out.as_slice(), body);
        assert!(d.pop().is_none());
    }

    #[test]
    fn deframe_skips_idle_apid() {
        // First packet: idle, must be skipped.
        let idle_body = vec![0xAA; 16];
        let idle_header = SpacePacketHeader {
            version: 0,
            packet_type: PacketType::Tm,
            sec_hdr_flag: false,
            apid: IDLE_APID,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count: 0,
            data_len_minus_one: (idle_body.len() as u16) - 1,
        };
        let mut wire = idle_header.pack().to_vec();
        wire.extend_from_slice(&idle_body);

        // Second packet: real data.
        let body = b"\x00\x00\x00\x05hi";
        let real_header = SpacePacketHeader {
            version: 0,
            packet_type: PacketType::Tm,
            sec_hdr_flag: false,
            apid: 5,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count: 0,
            data_len_minus_one: (body.len() as u16) - 1,
        };
        wire.extend_from_slice(&real_header.pack());
        wire.extend_from_slice(body);

        let mut d = SpacePacketDeframer::new();
        d.push(&wire);
        let out = d.pop().expect("real frame");
        assert_eq!(out.as_slice(), body);
        assert!(d.pop().is_none());
    }

    #[test]
    fn deframe_resyncs_through_garbage() {
        let body = b"\x00\x00\x00\x05hi";
        let h = SpacePacketHeader {
            version: 0,
            packet_type: PacketType::Tm,
            sec_hdr_flag: false,
            apid: 5,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count: 0,
            data_len_minus_one: (body.len() as u16) - 1,
        };
        let mut wire = vec![0xFF, 0xFF, 0xFF];
        wire.extend_from_slice(&h.pack());
        wire.extend_from_slice(body);

        let mut d = SpacePacketDeframer::new();
        d.push(&wire);
        let out = d.pop().expect("frame");
        assert_eq!(out.as_slice(), body);
        assert!(d.discarded() >= 3);
    }
}
