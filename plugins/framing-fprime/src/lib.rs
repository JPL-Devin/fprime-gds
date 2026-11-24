//! The F Prime framing layer `fprime` and the profile of the same name:
//! start word `0xDEADBEEF`, packet length, packet, CRC-32 of everything before it.
//! All fields are 32-bit big-endian.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use fgds_core::Dictionary;
use fgds_core::{
    Deframe, Deframed, Error, Frame, FramingLayer, Manifest, OptionSpec, PacketMeta, PeerInfo, Plugin, PluginConfig,
    Registry, Result,
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
        Manifest {
            name: "framing-fprime",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![OptionSpec {
                name: FLIGHT_FRAME_SIZE,
                help: "Largest frame in bytes, header and checksum included, the flight software accepts: a larger one is not sent. Flight software built with a smaller accumulator than given here still drops the frames [default: ComFprimeConfig.BuffMgr.frameAccumulatorSize of the dictionary, else 2048]",
                default: None,
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|cfg, _| flight_frame_size(cfg, STOCK_FLIGHT_FRAME).map(|_| ())));
        reg.add_framing_layer("fprime", Arc::new(FprimeFraming::default()))?;
        reg.add_framing_profile("fprime", &["fprime"])
    }
}

const FLIGHT_FRAME_SIZE: &str = "flight-frame-size";
/// `frameAccumulatorSize` of the stock `ComFprime` subtopology: a frame that does not fit there is dropped.
const STOCK_FLIGHT_FRAME: usize = 2048;
/// That constant, when a project puts it in its dictionary.
const FLIGHT_CONSTANT: &str = "ComFprimeConfig.BuffMgr.frameAccumulatorSize";

/// The largest frame the flight software accepts: from a frame of one byte to the largest of this framing.
fn flight_frame_size(cfg: &PluginConfig, otherwise: usize) -> Result<usize> {
    let (least, most) = (HEADER + TRAILER + 1, HEADER + TRAILER + MAX_PACKET - 1);
    match cfg.get(FLIGHT_FRAME_SIZE) {
        None => Ok(otherwise),
        Some(text) => text.parse().ok().filter(|size| (least..=most).contains(size)).ok_or_else(|| {
            Error::Config(format!(
                "framing-fprime {FLIGHT_FRAME_SIZE}: {text:?} is not a number of bytes from {least} to {most}"
            ))
        }),
    }
}

/// `flight_frame`: the largest frame sent. Receiving has the limit of the framing alone.
pub struct FprimeFraming {
    flight_frame: usize,
}

impl Default for FprimeFraming {
    fn default() -> Self {
        FprimeFraming { flight_frame: STOCK_FLIGHT_FRAME }
    }
}

impl FprimeFraming {
    /// The option as checked when the GDS started (`add_option_check`), else what the dictionary or stock F Prime has.
    fn configured(&self, cfg: &PluginConfig) -> FprimeFraming {
        FprimeFraming { flight_frame: flight_frame_size(cfg, self.flight_frame).unwrap_or(self.flight_frame) }
    }
}

impl FramingLayer for FprimeFraming {
    fn with_dictionary(&self, dict: &Dictionary) -> Option<Arc<dyn FramingLayer>> {
        let constants = dict.json["constants"].as_array()?;
        let size = constants.iter().find(|c| c["qualifiedName"] == FLIGHT_CONSTANT)?["value"].as_u64()?;
        Some(Arc::new(FprimeFraming { flight_frame: usize::try_from(size).ok()?.max(HEADER + TRAILER + 1) }))
    }
    fn deframer(&self, _: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        Box::new(FprimeFraming::default())
    }
    fn framer(&self, cfg: &PluginConfig) -> Box<dyn Frame> {
        Box::new(self.configured(cfg))
    }
    fn limit(&self, cfg: &PluginConfig) -> Option<String> {
        let frame = self.configured(cfg).flight_frame;
        (frame < HEADER + TRAILER + MAX_PACKET - 1).then(|| {
            format!("frames of at most {frame} bytes, frameAccumulatorSize of ComFprime: --framing-fprime-{FLIGHT_FRAME_SIZE}")
        })
    }
}

impl Frame for FprimeFraming {
    fn frame(&mut self, packet: &[u8], _: &PacketMeta, out: &mut BytesMut) -> Result<()> {
        // The receiver takes a length of 4,096 or more for noise.
        if packet.len() >= MAX_PACKET {
            let message =
                format!("packet of {} bytes exceeds the {} of an F Prime frame", packet.len(), MAX_PACKET - 1);
            return Err(Error::Invalid(message));
        }
        let frame = HEADER + packet.len() + TRAILER;
        if frame > self.flight_frame {
            return Err(Error::Invalid(format!(
                "frame of {frame} bytes exceeds the {} the flight software accepts (frameAccumulatorSize of ComFprime, --framing-fprime-{FLIGHT_FRAME_SIZE})",
                self.flight_frame
            )));
        }
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
        let flush = |junk: &mut BytesMut, out: &mut Vec<Deframed>, reason: &str| {
            if !junk.is_empty() {
                out.push(Deframed::Discarded { bytes: junk.split().freeze(), reason: reason.into() });
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
