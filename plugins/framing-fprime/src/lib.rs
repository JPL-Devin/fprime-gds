//! The F Prime framing layer `fprime` and the profile of the same name:
//! start word `0xDEADBEEF`, packet length, packet, CRC-32 of everything before it.
//! All fields are 32-bit big-endian.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use fgds_core::{
    Deframe, Deframed, Frame, FramingLayer, Manifest, PacketMeta, PeerInfo, Plugin, PluginConfig, Registry, Result,
};
use std::sync::Arc;

const START: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
const HEADER: usize = 8;
const TRAILER: usize = 4;
/// A length at or above this is not a frame (same limit as the Python GDS).
const MAX_PACKET: usize = 4096;
const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

pub struct FprimeFramingPlugin;

impl Plugin for FprimeFramingPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "framing-fprime", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_framing_layer("fprime", Arc::new(FprimeFraming))?;
        reg.add_framing_profile("fprime", &["fprime"])
    }
}

pub struct FprimeFraming;

impl FramingLayer for FprimeFraming {
    fn deframer(&self, _: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        Box::new(FprimeFraming)
    }
    fn framer(&self, _: &PluginConfig) -> Box<dyn Frame> {
        Box::new(FprimeFraming)
    }
}

impl Frame for FprimeFraming {
    fn frame(&mut self, packet: &[u8], out: &mut BytesMut) -> Result<()> {
        let start = out.len();
        out.put_slice(&START);
        out.put_u32(packet.len() as u32);
        out.put_slice(packet);
        let checksum = CRC.checksum(&out[start..]);
        out.put_u32(checksum);
        Ok(())
    }
}

/// Where the next frame may begin: the first position whose bytes are, as far as the
/// input goes, a prefix of the start word.
fn find_start(input: &[u8]) -> usize {
    (0..input.len()).find(|&i| input[i..].iter().zip(START).all(|(a, b)| *a == b)).unwrap_or(input.len())
}

impl Deframe for FprimeFraming {
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()> {
        // Bytes that belong to no frame, reported as one piece.
        let mut junk = BytesMut::new();
        let flush = |junk: &mut BytesMut, out: &mut Vec<Deframed>, reason| {
            if !junk.is_empty() {
                out.push(Deframed::Discarded { bytes: junk.split().freeze(), reason });
            }
        };
        loop {
            let skip = find_start(input);
            junk.extend_from_slice(&input.split_to(skip));
            if input.len() < HEADER {
                break;
            }
            let length = u32::from_be_bytes([input[4], input[5], input[6], input[7]]) as usize;
            let total = HEADER + length + TRAILER;
            if length >= MAX_PACKET {
                // Not a frame: step over this start word and search again.
                junk.extend_from_slice(&input.split_to(1));
                continue;
            }
            if input.len() < total {
                break;
            }
            let expected = CRC.checksum(&input[..HEADER + length]).to_be_bytes();
            if input[HEADER + length..total] != expected {
                flush(&mut junk, out, "no start word");
                junk.extend_from_slice(&input.split_to(1));
                flush(&mut junk, out, "checksum mismatch");
                continue;
            }
            flush(&mut junk, out, "no start word");
            input.advance(HEADER);
            let bytes: Bytes = input.split_to(length).freeze();
            input.advance(TRAILER);
            out.push(Deframed::Packet { bytes, meta: PacketMeta::default() });
        }
        flush(&mut junk, out, "no start word");
        Ok(())
    }
}
