//! Composite framer/deframer that chains an outer protocol around an inner
//! one.  Mirrors the behaviour of
//! `fprime_gds.common.communication.ccsds.chain.ChainedFramerDeframer`.
//!
//! Framing is straightforward: payload → inner.frame → outer.frame.
//!
//! Deframing requires buffering: a single outer frame's body may contain
//! zero, one, or many inner packets, and inner packets can also span outer
//! frames.  The inner deframer's internal buffer handles the latter naturally.

use fprime_frame::{Deframer, FrameError, Framer};

pub struct ChainedFramer {
    pub inner: Box<dyn Framer>,
    pub outer: Box<dyn Framer>,
}

impl ChainedFramer {
    pub fn new(inner: Box<dyn Framer>, outer: Box<dyn Framer>) -> Self {
        Self { inner, outer }
    }
}

impl Framer for ChainedFramer {
    fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        let inner_framed = self.inner.frame(payload)?;
        self.outer.frame(&inner_framed)
    }
}

pub struct ChainedDeframer {
    pub outer: Box<dyn Deframer>,
    pub inner: Box<dyn Deframer>,
}

impl ChainedDeframer {
    pub fn new(outer: Box<dyn Deframer>, inner: Box<dyn Deframer>) -> Self {
        Self { outer, inner }
    }
}

impl Deframer for ChainedDeframer {
    fn push(&mut self, data: &[u8]) {
        self.outer.push(data);
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        loop {
            // First, try to pull a complete inner packet from whatever the
            // inner deframer is already holding.
            if let Some(p) = self.inner.pop() {
                return Some(p);
            }
            // Otherwise, drain another outer frame and feed its body to the
            // inner deframer.
            match self.outer.pop() {
                Some(outer_frame) => {
                    self.inner.push(&outer_frame);
                    continue;
                }
                None => return None,
            }
        }
    }

    fn discarded(&mut self) -> usize {
        self.outer.discarded() + self.inner.discarded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space_data_link::{build_tm_frame, TcFramer, TmDeframer};
    use crate::space_packet::{
        SpacePacketDeframer, SpacePacketFramer, SpacePacketHeader, IDLE_APID, SEQ_FLAGS_UNSEGMENTED,
    };

    fn build_sp_tm(apid: u16, seq: u16, body: &[u8]) -> Vec<u8> {
        let h = SpacePacketHeader {
            version: 0,
            packet_type: crate::space_packet::PacketType::Tm,
            sec_hdr_flag: false,
            apid,
            seq_flags: SEQ_FLAGS_UNSEGMENTED as u8,
            seq_count: seq,
            data_len_minus_one: (body.len() as u16) - 1,
        };
        let mut out = h.pack().to_vec();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn frame_roundtrip_through_chain() {
        let inner = Box::new(SpacePacketFramer::new());
        let outer = Box::new(TcFramer::new(0x44, 1).unwrap());
        let mut framer = ChainedFramer::new(inner, outer);
        let mut payload = vec![0u8, 0, 0, 0x42];
        payload.extend_from_slice(b"hello");
        let bytes = framer.frame(&payload).unwrap();
        // Outer TC frame: 5-byte header + (6-byte SP header + 9-byte payload) + 2-byte CRC = 22 bytes.
        assert_eq!(bytes.len(), 5 + 6 + payload.len() + 2);
    }

    #[test]
    fn deframe_through_chain() {
        // Build TM frame containing an SP packet, then run it through the
        // chained deframer.
        let body = b"\x00\x00\x00\x05hi";
        let sp = build_sp_tm(5, 0, body);
        let frame_size = 64;
        let tm = build_tm_frame(0x44, 1, &sp, frame_size);

        let outer = Box::new(TmDeframer::new(0x44, 1, frame_size).unwrap());
        let inner = Box::new(SpacePacketDeframer::new());
        let mut d = ChainedDeframer::new(outer, inner);
        d.push(&tm);
        let out = d.pop().expect("packet");
        assert_eq!(out.as_slice(), body);
    }

    #[test]
    fn deframe_skips_idle_in_chain() {
        // TM frame whose data field is entirely an idle SP packet (the FSW's
        // canonical way to mark a frame as carrying no real telemetry).
        // Build the idle packet to fill the data area exactly.
        let frame_size = 64;
        let data_field = frame_size - 6 /* TM hdr */ - 2 /* CRC */ - 6 /* SP hdr */;
        let idle = build_sp_tm(IDLE_APID, 0, &vec![0xAA; data_field]);
        let tm = build_tm_frame(0x44, 1, &idle, frame_size);

        let outer = Box::new(TmDeframer::new(0x44, 1, frame_size).unwrap());
        let inner = Box::new(SpacePacketDeframer::new());
        let mut d = ChainedDeframer::new(outer, inner);
        d.push(&tm);
        assert!(d.pop().is_none());
    }
}
