//! CCSDS framing, as the Python GDS does it for the `ComCcsds` subtopology of F Prime:
//!
//! - layer `ccsds-tm-tc`: fixed-size TM frames down (packets may span frames), TC frames up;
//! - layer `ccsds-space-packet`: Space Packets, both directions, inside whole data
//!   units of an outer layer;
//! - layer `ccsds-space-packet-stream`: the same on a byte stream.
//!
//! Profiles: `ccsds` (`space-packet-space-data-link` of the Python GDS, its default)
//! and `ccsds-space-packet` (`raw-space-packet`).
//!
//! Packet metadata: `ccsds.scid` and `ccsds.vcid` from the TM frame, `ccsds.apid` and
//! `ccsds.seq_count` from the Space Packet.
//!
//! The spacecraft identifier, the TM frame size and the application identifiers come
//! from the dictionary (`ComCfg.SpacecraftId`, `ComCfg.TmFrameFixedSize`, `ComCfg.Apid`);
//! the options `scid` and `frame-size` take precedence.

pub mod stage;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use fgds_core::{
    Deframe, Deframed, Error, Frame, FramingLayer, Manifest, MetaValue, OptionSpec, PacketMeta, PeerInfo, Plugin,
    PluginConfig, Registry, Result,
};
use fgds_dictionary::{Dictionary, Type};
use stage::{Apids, SpaceDataLink, SpacePacket, SpacePacketSplitter, Stage};
use std::sync::Arc;

pub const META_SCID: &str = "ccsds.scid";
pub const META_VCID: &str = "ccsds.vcid";
pub const META_APID: &str = "ccsds.apid";
pub const META_SEQ_COUNT: &str = "ccsds.seq_count";

pub struct CcsdsFramingPlugin;

impl Plugin for CcsdsFramingPlugin {
    fn manifest(&self) -> Manifest {
        let option = |name, help, default| OptionSpec { name, help, default, secret: false };
        Manifest {
            name: "framing-ccsds",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![
                option(
                    "scid",
                    "Spacecraft identifier [default: ComCfg.SpacecraftId of the dictionary, else 0x44]",
                    None,
                ),
                option("vcid", "Virtual channel identifier", Some("1")),
                option(
                    "frame-size",
                    "Size of TM frames in bytes [default: ComCfg.TmFrameFixedSize of the dictionary, else 1024]",
                    None,
                ),
            ],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|cfg, _| {
            [SCID, VCID, FRAME_SIZE].iter().try_for_each(|(name, min, max)| checked(cfg, name, *min, *max).map(|_| ()))
        }));
        reg.add_framing_layer("ccsds-tm-tc", Arc::new(SpaceDataLinkLayer::default()))?;
        reg.add_framing_layer(
            "ccsds-space-packet",
            Arc::new(SpacePacketLayer { stream: false, apids: Apids::default(), counts: Default::default() }),
        )?;
        let stream = SpacePacketLayer { stream: true, apids: Apids::default(), counts: Default::default() };
        reg.add_framing_layer("ccsds-space-packet-stream", Arc::new(stream))?;
        // Outermost first.
        reg.add_framing_profile("ccsds", &["ccsds-tm-tc", "ccsds-space-packet"])?;
        reg.add_framing_profile("ccsds-space-packet", &["ccsds-space-packet-stream"])
    }
}

/// An unsigned integer constant of the dictionary.
fn constant(dict: &Dictionary, name: &str) -> Option<u64> {
    let constants = dict.json["constants"].as_array()?;
    constants.iter().find(|c| c["qualifiedName"] == name)?["value"].as_u64()
}

/// A numeric option, decimal or `0x` hexadecimal, from `min` to `max`.
fn checked(cfg: &PluginConfig, name: &str, min: u64, max: u64) -> Result<Option<u64>> {
    let Some(text) = cfg.get(name) else { return Ok(None) };
    let parsed = match text.strip_prefix("0x").or(text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    match parsed {
        Ok(value) if (min..=max).contains(&value) => Ok(Some(value)),
        _ => Err(Error::Config(format!(
            "[plugins.framing-ccsds] {name}: \"{text}\" is not a number from {min} to {max}"
        ))),
    }
}

const SCID: (&str, u64, u64) = ("scid", 1, 0x3FF);
const VCID: (&str, u64, u64) = ("vcid", 0, 0x3F);
const FRAME_SIZE: (&str, u64, u64) = ("frame-size", (stage::TM_HEADER + stage::TM_TRAILER + 1) as u64, u32::MAX as u64);

/// The option as checked when the GDS started (`add_option_check`); a value out of range is not used.
fn option(cfg: &PluginConfig, (name, min, max): (&str, u64, u64)) -> Option<u64> {
    checked(cfg, name, min, max).ok().flatten()
}

/// TM frames down, TC frames up (`SpaceDataLinkFramerDeframer` of the Python GDS).
#[derive(Default)]
pub struct SpaceDataLinkLayer {
    /// From the dictionary, when it has them.
    scid: Option<u64>,
    frame_size: Option<u64>,
}

impl SpaceDataLinkLayer {
    fn stage(&self, cfg: &PluginConfig) -> SpaceDataLink {
        let minimum = (stage::TM_HEADER + stage::TM_TRAILER + 1) as u64;
        let scid = option(cfg, SCID).or(self.scid).filter(|scid| *scid != 0);
        let frame_size = option(cfg, FRAME_SIZE).or(self.frame_size).filter(|n| *n >= minimum);
        SpaceDataLink::new(
            scid.map_or(stage::FALLBACK_SCID, |scid| scid as u16),
            option(cfg, VCID).map_or(stage::DEFAULT_VCID, |vcid| vcid as u8),
            frame_size.map_or(stage::FALLBACK_FRAME_SIZE, |n| n as usize),
        )
    }
}

impl FramingLayer for SpaceDataLinkLayer {
    fn with_dictionary(&self, dict: &Dictionary) -> Option<Arc<dyn FramingLayer>> {
        Some(Arc::new(SpaceDataLinkLayer {
            scid: constant(dict, "ComCfg.SpacecraftId"),
            frame_size: constant(dict, "ComCfg.TmFrameFixedSize"),
        }))
    }
    fn deframer(&self, cfg: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        Box::new(self.stage(cfg))
    }
    fn framer(&self, cfg: &PluginConfig) -> Box<dyn Frame> {
        Box::new(self.stage(cfg))
    }
}

fn warn(warnings: Vec<String>) {
    for warning in warnings {
        tracing::warn!("{}", warning.trim_start_matches("[WARNING] "));
    }
}

impl Frame for SpaceDataLink {
    fn frame(&mut self, packet: &[u8], _: &PacketMeta, out: &mut BytesMut) -> Result<()> {
        out.put_slice(&Stage::frame(self, packet).map_err(Error::Invalid)?);
        Ok(())
    }
}

impl Deframe for SpaceDataLink {
    /// One packet per TM frame that completes Space Packets: those packets, in order.
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()> {
        loop {
            let mut warnings = Vec::new();
            let step = Stage::deframe(self, input, &mut warnings);
            warn(warnings);
            input.advance(step.consumed);
            if !step.discarded.is_empty() {
                let reason = match self.checksum_failed {
                    true => "checksum mismatch",
                    false => "no TM frame of this spacecraft and virtual channel",
                };
                out.push(Deframed::Discarded { bytes: step.discarded.into(), reason: reason.to_string() });
            }
            let Some(packets) = step.packet else { return Ok(()) };
            let meta = PacketMeta::default()
                .with(META_SCID, MetaValue::U64(self.scid.into()))
                .with(META_VCID, MetaValue::U64(self.vcid.into()));
            out.push(Deframed::Packet { bytes: packets.into(), meta });
        }
    }
}

/// Space Packets (`SpacePacketFramerDeframer` after `SpacePacketSplitterFramerDeframer`
/// of the Python GDS).
pub struct SpacePacketLayer {
    /// True when the input is a byte stream: an incomplete packet waits for more bytes.
    /// False when an outer layer delivers whole packets: what is left over is discarded.
    stream: bool,
    apids: Apids,
    /// One count for the framers and deframers of the run, as the Python GDS.
    counts: stage::SequenceCounts,
}

impl SpacePacketLayer {
    fn stage(&self) -> Box<SpacePacketFraming> {
        Box::new(SpacePacketFraming {
            stream: self.stream,
            packet: SpacePacket::sharing(self.apids.clone(), self.counts.clone()),
        })
    }
}

impl FramingLayer for SpacePacketLayer {
    fn with_dictionary(&self, dict: &Dictionary) -> Option<Arc<dyn FramingLayer>> {
        // Without `ComCfg.Apid`, the packet type field is the application identifier.
        let apids = match dict.types.get("ComCfg.Apid") {
            Some(Type::Enum { rep, constants, .. }) => match **rep {
                Type::Int { bits, .. } => Apids {
                    width: usize::from(bits / 8),
                    values: Some(constants.iter().map(|(_, value)| *value as u64).collect()),
                },
                _ => Apids::default(),
            },
            _ => match dict.config_type("FwPacketDescriptorType", Type::U16) {
                Type::Int { bits, .. } => Apids { width: usize::from(bits / 8), values: None },
                _ => Apids::default(),
            },
        };
        Some(Arc::new(SpacePacketLayer { stream: self.stream, apids, counts: self.counts.clone() }))
    }
    fn deframer(&self, _: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        self.stage()
    }
    fn framer(&self, _: &PluginConfig) -> Box<dyn Frame> {
        self.stage()
    }
}

struct SpacePacketFraming {
    stream: bool,
    packet: SpacePacket,
}

impl Frame for SpacePacketFraming {
    fn frame(&mut self, packet: &[u8], _: &PacketMeta, out: &mut BytesMut) -> Result<()> {
        out.put_slice(&self.packet.frame(packet).map_err(Error::Invalid)?);
        Ok(())
    }
}

impl Deframe for SpacePacketFraming {
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()> {
        let mut warnings = Vec::new();
        let discard = |bytes: Vec<u8>, out: &mut Vec<Deframed>| {
            if !bytes.is_empty() {
                out.push(Deframed::Discarded { bytes: bytes.into(), reason: "no telemetry Space Packet".into() });
            }
        };
        loop {
            let split = SpacePacketSplitter.deframe(input, &mut warnings);
            input.advance(split.consumed);
            discard(split.discarded, out);
            let Some(whole) = split.packet else { break };
            let step = self.packet.deframe(&whole, &mut warnings);
            discard(step.discarded, out);
            if let (Some(payload), Some(header)) = (step.packet, self.packet.last) {
                let meta = PacketMeta::default()
                    .with(META_APID, MetaValue::U64(header.apid.into()))
                    .with(META_SEQ_COUNT, MetaValue::U64(header.seq_count.into()));
                out.push(Deframed::Packet { bytes: Bytes::from(payload), meta });
            }
        }
        if !self.stream && !input.is_empty() {
            out.push(Deframed::Discarded { bytes: input.split().freeze(), reason: "incomplete Space Packet".into() });
        }
        warn(warnings);
        Ok(())
    }
}
